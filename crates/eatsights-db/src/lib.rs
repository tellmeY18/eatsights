//! Postgres access for Eatsights: pools, embedded migrations and typed queries.
//!
//! Runtime `sqlx::query` only (no compile-time checked macros, so no offline data).

pub mod export;
pub mod read;
pub mod work;
pub mod write;

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
pub use sqlx::{PgPool, Postgres, Transaction};

/// Forward-only migrations, embedded at build time. Add each new `migrations/NNNN_*.sql` here.
const MIGRATIONS: &[(i64, &str, &str)] = &[
    (1, "init", include_str!("../../../migrations/0001_init.sql")),
    (
        2,
        "trust",
        include_str!("../../../migrations/0002_trust.sql"),
    ),
    (
        3,
        "exports",
        include_str!("../../../migrations/0003_exports.sql"),
    ),
];

#[derive(Debug)]
struct Embedded;

impl sqlx::migrate::MigrationSource<'static> for Embedded {
    fn resolve(
        self,
    ) -> futures_core::future::BoxFuture<
        'static,
        std::result::Result<Vec<sqlx::migrate::Migration>, sqlx::error::BoxDynError>,
    > {
        Box::pin(async {
            Ok(MIGRATIONS
                .iter()
                .map(|(v, d, sql)| {
                    sqlx::migrate::Migration::new(
                        *v,
                        (*d).into(),
                        sqlx::migrate::MigrationType::Simple,
                        (*sql).into(),
                        false,
                    )
                })
                .collect())
        })
    }
}

pub async fn migrator() -> Result<sqlx::migrate::Migrator> {
    Ok(sqlx::migrate::Migrator::new(Embedded).await?)
}

/// Channel notified (inside the writing transaction) whenever `public.changes` grows.
pub const CHANGES_CHANNEL: &str = "eatsights_changes";

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid reference: {0}")]
    BadReference(String),
}

impl DbError {
    /// Map a foreign-key violation to `BadReference`.
    pub(crate) fn classify(e: sqlx::Error) -> Self {
        if let sqlx::Error::Database(d) = &e
            && d.code().as_deref() == Some("23503")
        {
            return DbError::BadReference(d.message().to_owned());
        }
        DbError::Sqlx(e)
    }
}

pub type Result<T> = std::result::Result<T, DbError>;

/// Write/worker pool and read pool (the same pool when no read URL is given).
#[derive(Clone, Debug)]
pub struct Db {
    pub write: PgPool,
    pub read: PgPool,
}

pub async fn pool(url: &str, size: u32) -> Result<PgPool> {
    let opts = PgConnectOptions::from_str(url)?.application_name("eatsights");
    Ok(PgPoolOptions::new()
        .max_connections(size.max(1))
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(300))
        .connect_with(opts)
        .await?)
}

impl Db {
    pub async fn connect(write_url: &str, read_url: Option<&str>, size: u32) -> Result<Self> {
        let write = pool(write_url, size).await?;
        let read = match read_url {
            Some(u) if u != write_url => pool(u, size).await?,
            _ => write.clone(),
        };
        Ok(Self { write, read })
    }

    pub async fn migrate(&self) -> Result<()> {
        migrator().await?.run(&self.write).await?;
        Ok(())
    }

    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.read).await?;
        Ok(())
    }
}

/// `"<scope>:<id>"`, the key of one (subject) in observations and facts.
pub fn subject_key(
    scope: &str,
    place_id: Option<&str>,
    menu_item_id: Option<&str>,
    dish_id: Option<&str>,
) -> Option<String> {
    let id = match scope {
        "place" => place_id,
        "menu_item" => menu_item_id,
        "dish" => dish_id,
        _ => None,
    }?;
    Some(format!("{scope}:{id}"))
}

pub(crate) async fn add_change(
    tx: &mut Transaction<'_, Postgres>,
    entity_type: &str,
    entity_id: &str,
    op: &str,
) -> Result<i64> {
    let seq: i64 = sqlx::query_scalar(
        "INSERT INTO public.changes (entity_type, entity_id, op) VALUES ($1, $2, $3) RETURNING seq",
    )
    .bind(entity_type)
    .bind(entity_id)
    .bind(op)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(CHANGES_CHANNEL)
        .bind(seq.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(seq)
}
