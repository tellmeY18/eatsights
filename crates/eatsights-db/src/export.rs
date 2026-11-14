//! Read-only queries for public snapshots (AGENTS.md 9.4).
//!
//! Everything here reads `public` tables only, inside one REPEATABLE READ, READ ONLY
//! transaction so every file and the manifest's `changes_seq` describe the same instant.
//! Rows are fetched in keyset batches, never whole tables.

use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};

use crate::Result;

/// One exported table: file stem and keyset query. `$1` (and `$2` for two-column keys)
/// is the last key seen, the final parameter the batch size. Columns: key1, key2, doc.
#[derive(Debug, Clone, Copy)]
pub struct ExportTable {
    pub name: &'static str,
    two_keys: bool,
    first: &'static str,
    next: &'static str,
}

/// Public data in a snapshot. Only tables of the public schema belong here (tested).
pub const EXPORT_TABLES: &[ExportTable] = &[
    ExportTable {
        name: "places",
        two_keys: false,
        first: "SELECT id, '', doc FROM public.places ORDER BY id LIMIT $1",
        next: "SELECT id, '', doc FROM public.places WHERE id > $1 ORDER BY id LIMIT $2",
    },
    ExportTable {
        name: "sections",
        two_keys: false,
        first: "SELECT id, '', doc FROM public.sections ORDER BY id LIMIT $1",
        next: "SELECT id, '', doc FROM public.sections WHERE id > $1 ORDER BY id LIMIT $2",
    },
    ExportTable {
        name: "menu_items",
        two_keys: false,
        first: "SELECT id, '', doc FROM public.menu_items ORDER BY id LIMIT $1",
        next: "SELECT id, '', doc FROM public.menu_items WHERE id > $1 ORDER BY id LIMIT $2",
    },
    ExportTable {
        name: "dishes",
        two_keys: false,
        first: "SELECT id, '', doc FROM public.dishes ORDER BY id LIMIT $1",
        next: "SELECT id, '', doc FROM public.dishes WHERE id > $1 ORDER BY id LIMIT $2",
    },
    ExportTable {
        name: "observations",
        two_keys: false,
        first: "SELECT id, '', doc FROM public.observations ORDER BY id LIMIT $1",
        next: "SELECT id, '', doc FROM public.observations WHERE id > $1 ORDER BY id LIMIT $2",
    },
    ExportTable {
        name: "facts",
        two_keys: true,
        first: "SELECT subject_key, property, doc FROM public.facts
                ORDER BY subject_key, property LIMIT $1",
        next: "SELECT subject_key, property, doc FROM public.facts
               WHERE (subject_key, property) > ($1, $2) ORDER BY subject_key, property LIMIT $3",
    },
    ExportTable {
        name: "vendor_of",
        two_keys: false,
        first: "SELECT id, '', doc || jsonb_build_object('revoked', revoked_at IS NOT NULL)
                FROM public.vendor_bindings ORDER BY id LIMIT $1",
        next: "SELECT id, '', doc || jsonb_build_object('revoked', revoked_at IS NOT NULL)
               FROM public.vendor_bindings WHERE id > $1 ORDER BY id LIMIT $2",
    },
    ExportTable {
        name: "registry",
        two_keys: false,
        first: "SELECT id, '', doc FROM public.registry_properties ORDER BY id LIMIT $1",
        next: "SELECT id, '', doc FROM public.registry_properties WHERE id > $1 ORDER BY id LIMIT $2",
    },
];

/// Keyset cursor within one table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor(pub Option<(String, String)>);

/// A consistent, read-only view of the public data.
pub struct Snapshot {
    tx: Transaction<'static, Postgres>,
    /// Latest `public.changes.seq` visible in this snapshot.
    pub changes_seq: i64,
}

impl Snapshot {
    pub async fn begin(pool: &PgPool) -> Result<Self> {
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let changes_seq: i64 =
            sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM public.changes")
                .fetch_one(&mut *tx)
                .await?;
        Ok(Self { tx, changes_seq })
    }

    /// The next batch of `doc`s after `cursor` (advanced in place). Empty at the end.
    pub async fn next_batch(
        &mut self,
        table: &ExportTable,
        cursor: &mut Cursor,
        limit: i64,
    ) -> Result<Vec<Value>> {
        let rows: Vec<(String, String, Value)> = match &cursor.0 {
            None => {
                sqlx::query_as(table.first)
                    .bind(limit)
                    .fetch_all(&mut *self.tx)
                    .await?
            }
            Some((k1, k2)) => {
                let q = sqlx::query_as(table.next).bind(k1);
                let q = if table.two_keys { q.bind(k2) } else { q };
                q.bind(limit).fetch_all(&mut *self.tx).await?
            }
        };
        if let Some((k1, k2, _)) = rows.last() {
            cursor.0 = Some((k1.clone(), k2.clone()));
        }
        Ok(rows.into_iter().map(|(_, _, d)| d).collect())
    }

    pub async fn finish(self) -> Result<()> {
        self.tx.commit().await?;
        Ok(())
    }
}

/// Non-stale facts whose newest observation (`last_observed_at`) is older than
/// `stale_half_lives` half-lives at `now`, after `after` (keyset), at most `limit`.
pub async fn stale_candidates(
    pool: &PgPool,
    stale_half_lives: f64,
    now: chrono::DateTime<chrono::Utc>,
    after: Option<&(String, String)>,
    limit: i64,
) -> Result<Vec<(String, String)>> {
    let (k1, k2) = after.cloned().unwrap_or_default();
    Ok(sqlx::query_as(
        "SELECT subject_key, property FROM public.facts
         WHERE (subject_key, property) > ($1, $2)
           AND doc->>'status' IS DISTINCT FROM 'stale'
           AND (doc->>'last_observed_at')::timestamptz
               < $5 - make_interval(secs => $3 * 3600 * COALESCE((doc->>'half_life_hours')::float8, 0))
         ORDER BY subject_key, property LIMIT $4",
    )
    .bind(k1)
    .bind(k2)
    .bind(stale_half_lives)
    .bind(limit)
    .bind(now)
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_sql_never_touches_private() {
        for t in EXPORT_TABLES {
            for sql in [t.first, t.next] {
                assert!(!sql.contains("private."), "{}", t.name);
                assert!(sql.contains("public."), "{}", t.name);
            }
        }
        let src = include_str!("export.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or_default();
        assert!(!code.contains(concat!("private", ".")));
    }
}
