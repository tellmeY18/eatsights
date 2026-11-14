//! `eatsights`: the single binary (API, worker, migrations and tooling).

use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use chrono::Utc;
use clap::{Parser, Subcommand};
use eatsights_api::{ApiConfig, AppState, Cidr};
use eatsights_core::crypto;
use eatsights_core::registry::Registry;
use eatsights_core::submission::CheckConfig;
use eatsights_db::Db;
use eatsights_db::write::{self, NewEntitySubmission};
use poem::Server;
use poem::listener::TcpListener;
use serde_json::{Value, json};
use tokio::sync::watch;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser)]
#[command(name = "eatsights", version, about = "Eatsights: open food data API")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the API and/or worker.
    Serve {
        /// Comma-separated roles: api, worker.
        #[arg(long, default_value = "api,worker", env = "EATSIGHTS_ROLES")]
        roles: String,
        /// Apply migrations before serving (development only).
        #[arg(long)]
        migrate: bool,
    },
    /// Apply database migrations.
    Migrate,
    /// Registry tasks.
    Registry {
        #[command(subcommand)]
        cmd: RegistryCmd,
    },
    /// Print a fresh Ed25519 keypair as JSON (nothing is stored).
    Keygen,
    /// Import entities from NDJSON lines `{"entity_type": "...", "entity": {...}}`.
    Import { file: PathBuf },
    /// Write gzip NDJSON snapshots of the public tables and a manifest.
    Export { dir: PathBuf },
    /// Print the OpenAPI document.
    Openapi,
    /// Sign a JSON object from stdin with a private key (development/e2e helper).
    #[command(hide = true)]
    Sign {
        #[arg(long, env = "EATSIGHTS_SIGNING_KEY")]
        key: String,
    },
    /// Print a new prefixed ULID, e.g. `id pl_` (development/e2e helper).
    #[command(hide = true)]
    Id { prefix: String },
    /// Print the public key and key_id of a private key (development/e2e helper).
    #[command(hide = true)]
    Pubkey {
        #[arg(long, env = "EATSIGHTS_SIGNING_KEY")]
        key: String,
    },
}

#[derive(Subcommand)]
enum RegistryCmd {
    /// Mirror the embedded registry into the database.
    Sync,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_num<T: std::str::FromStr>(name: &str, default: T) -> Result<T, BoxError> {
    match env(name) {
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| format!("{name} is not a valid number").into()),
        None => Ok(default),
    }
}

async fn connect() -> Result<Db, BoxError> {
    let url = env("DATABASE_URL").ok_or("DATABASE_URL is required")?;
    let read = env("DATABASE_READ_URL");
    Ok(Db::connect(&url, read.as_deref(), env_num("EATSIGHTS_DB_POOL", 8u32)?).await?)
}

fn registry() -> Result<&'static Registry, BoxError> {
    eatsights_core::schema::init()?;
    Ok(Registry::embedded()?)
}

async fn sync_registry(db: &Db, reg: &Registry) -> Result<u64, BoxError> {
    let entries: Vec<(String, Value)> = reg
        .iter()
        .filter_map(|p| reg.get_raw(&p.id).map(|r| (p.id.clone(), r.clone())))
        .collect();
    Ok(write::sync_registry(&db.write, reg.version(), reg.hash(), &entries).await?)
}

fn trust_config() -> Result<eatsights_core::trust::TrustConfig, BoxError> {
    let d = eatsights_core::trust::TrustConfig::default();
    Ok(eatsights_core::trust::TrustConfig {
        max_depth: env_num("EATSIGHTS_TRUST_MAX_DEPTH", d.max_depth)?,
        vendor_of_max_days: env_num("EATSIGHTS_VENDOR_OF_MAX_DAYS", d.vendor_of_max_days)?,
        strikes: env_num("EATSIGHTS_TRUST_STRIKES", d.strikes)?,
        strike_window_days: env_num("EATSIGHTS_TRUST_STRIKE_WINDOW_DAYS", d.strike_window_days)?,
        vouch_cooldown_days: env_num("EATSIGHTS_TRUST_VOUCH_COOLDOWN_DAYS", d.vouch_cooldown_days)?,
    })
}

fn api_config() -> Result<ApiConfig, BoxError> {
    let salt = env("EATSIGHTS_PSEUDONYM_SALT").ok_or("EATSIGHTS_PSEUDONYM_SALT is required")?;
    if salt.len() < 32 {
        return Err("EATSIGHTS_PSEUDONYM_SALT must be at least 32 bytes".into());
    }
    let d = ApiConfig::default();
    Ok(ApiConfig {
        base_url: env("EATSIGHTS_BASE_URL").unwrap_or(d.base_url),
        pseudonym_salt: salt.into_bytes(),
        pseudonym_salt_version: env_num("EATSIGHTS_PSEUDONYM_SALT_VERSION", 1)?,
        check: CheckConfig {
            max_age: chrono::Duration::days(env_num("EATSIGHTS_MAX_OBSERVATION_AGE_DAYS", 30)?),
            ..CheckConfig::default()
        },
        trusted_proxies: env("EATSIGHTS_TRUSTED_PROXIES")
            .map(|s| Cidr::parse_list(&s))
            .unwrap_or_default(),
        rate_per_key_per_min: env_num("EATSIGHTS_RATE_PER_KEY_PER_MIN", d.rate_per_key_per_min)?,
        rate_per_ip_per_min: env_num("EATSIGHTS_RATE_PER_IP_PER_MIN", d.rate_per_ip_per_min)?,
        keyreg_per_ip_per_hour: env_num(
            "EATSIGHTS_KEYREG_PER_IP_PER_HOUR",
            d.keyreg_per_ip_per_hour,
        )?,
        cache_mb: env_num("EATSIGHTS_CACHE_MB", d.cache_mb)?,
        max_age_factor: env_num("EATSIGHTS_MAX_AGE_FACTOR", d.max_age_factor)?,
        request_timeout_secs: env_num("EATSIGHTS_REQUEST_TIMEOUT_SECS", d.request_timeout_secs)?,
        body_limit_bytes: env_num("EATSIGHTS_BODY_LIMIT_BYTES", d.body_limit_bytes)?,
        swagger_ui: env_num("EATSIGHTS_SWAGGER_UI", d.swagger_ui)?,
        trust: trust_config()?,
        export_dir: export_dir(),
    })
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn serve(roles: &str, migrate: bool) -> Result<(), BoxError> {
    let roles: Vec<&str> = roles.split(',').map(str::trim).collect();
    let (run_api, run_worker) = (roles.contains(&"api"), roles.contains(&"worker"));
    if !run_api && !run_worker {
        return Err("--roles must include api and/or worker".into());
    }
    let reg = registry()?;
    let db = connect().await?;
    if migrate {
        db.migrate().await?;
    }
    let root = env("EATSIGHTS_ROOT_PUBLIC_KEY").ok_or("EATSIGHTS_ROOT_PUBLIC_KEY is required")?;
    let root_vk = crypto::parse_public_key(&root)?;
    write::upsert_root_key(
        &db.write,
        &crypto::key_id(&root_vk),
        &crypto::encode_public_key(&root_vk),
        trust_config()?.max_depth,
    )
    .await?;
    sync_registry(&db, reg).await?;

    let (stop_tx, stop_rx) = watch::channel(false);
    let (seq_tx, seq_rx) = watch::channel(0i64);
    let seq = Arc::new(AtomicI64::new(
        eatsights_db::read::latest_seq(&db.read).await?,
    ));
    let poll = Duration::from_millis(env_num("EATSIGHTS_POLL_MS", 2000u64)?);
    let tracker = tokio::spawn(eatsights_api::track_changes(
        db.clone(),
        seq.clone(),
        seq_tx,
        stop_rx.clone(),
        poll,
    ));

    let worker = if run_worker {
        let cfg = eatsights_worker::aggregate::AggConfig::from_env()?;
        tokio::spawn(eatsights_worker::sweep::run(
            db.clone(),
            reg,
            cfg.clone(),
            Duration::from_secs(60 * env_num("EATSIGHTS_STALE_SWEEP_MINUTES", 15u64)?.max(1)),
            stop_rx.clone(),
        ));
        tokio::spawn(eatsights_worker::export::run(
            db.read.clone(),
            eatsights_worker::export::ExportConfig {
                dir: export_dir(),
                interval: Duration::from_secs(
                    3600 * env_num("EATSIGHTS_EXPORT_INTERVAL_HOURS", 24u64)?.max(1),
                ),
                keep: env_num("EATSIGHTS_EXPORT_KEEP", 7usize)?,
            },
            stop_rx.clone(),
        ));
        Some(tokio::spawn(eatsights_worker::runner::run(
            db.clone(),
            reg,
            cfg,
            seq_rx,
            stop_rx.clone(),
            Duration::from_secs(env_num("EATSIGHTS_WORKER_INTERVAL_SECS", 30u64)?),
        )))
    } else {
        None
    };

    if run_api {
        let state = Arc::new(AppState::new(db.clone(), reg, api_config()?, seq));
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                st.forget_old_limits();
            }
        });
        let listen = env("EATSIGHTS_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into());
        tracing::info!(%listen, "serving");
        Server::new(TcpListener::bind(listen))
            .run_with_graceful_shutdown(
                eatsights_api::app(state),
                shutdown_signal(),
                Some(Duration::from_secs(20)),
            )
            .await?;
    } else {
        shutdown_signal().await;
    }
    tracing::info!("shutting down");
    let _ = stop_tx.send(true);
    if let Some(w) = worker {
        let _ = tokio::time::timeout(Duration::from_secs(10), w).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), tracker).await;
    db.write.close().await;
    Ok(())
}

async fn import(file: PathBuf) -> Result<(), BoxError> {
    registry()?;
    let db = connect().await?;
    let f = std::io::BufReader::new(std::fs::File::open(&file)?);
    let (mut ok, mut skipped) = (0u64, 0u64);
    for (n, line) in f.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).map_err(|e| format!("line {}: {e}", n + 1))?;
        let etype = v
            .get("entity_type")
            .and_then(Value::as_str)
            .ok_or(format!("line {}: entity_type missing", n + 1))?;
        let mut entity = v
            .get("entity")
            .cloned()
            .ok_or(format!("line {}: entity missing", n + 1))?;
        let now = Utc::now();
        if let Some(o) = entity.as_object_mut() {
            let ts = json!(now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            match etype {
                "place" => {
                    o.entry("osm").or_insert(json!({"state": "unchecked"}));
                    o.entry("record_state").or_insert(json!("active"));
                    o.entry("created_at").or_insert(ts);
                }
                "menu_item" => {
                    o.entry("record_state").or_insert(json!("active"));
                    o.entry("created_at").or_insert(ts);
                }
                _ => {}
            }
        }
        if let Err(e) = eatsights_core::schema::validate_def(etype, &entity) {
            return Err(format!("line {}: {}", n + 1, e.join("; ")).into());
        }
        let entity_id = entity
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let es_id = eatsights_core::ids::EntitySubmissionId::new();
        match write::write_entity(
            &db.write,
            &NewEntitySubmission {
                id: es_id.as_str(),
                raw: &v,
                key_id: "import",
                entity_type: etype,
                entity_id: &entity_id,
                op: "create",
            },
            &entity,
            true,
            now,
        )
        .await
        {
            Ok(_) => ok += 1,
            Err(eatsights_db::DbError::Conflict(_)) => skipped += 1,
            Err(e) => return Err(format!("line {}: {e}", n + 1).into()),
        }
    }
    println!(
        "{}",
        json!({ "imported": ok, "skipped_conflicts": skipped })
    );
    Ok(())
}

async fn export(dir: PathBuf) -> Result<(), BoxError> {
    let db = connect().await?;
    let m = eatsights_worker::export::write_snapshot(
        &db.read,
        &dir,
        env_num("EATSIGHTS_EXPORT_KEEP", 7usize)?,
    )
    .await?;
    println!("{}", serde_json::to_string(&m)?);
    Ok(())
}

fn export_dir() -> PathBuf {
    env("EATSIGHTS_EXPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".dev/exports"))
}

async fn run(cli: Cli) -> Result<(), BoxError> {
    match cli.cmd {
        Cmd::Serve { roles, migrate } => serve(&roles, migrate).await,
        Cmd::Migrate => {
            connect().await?.migrate().await?;
            tracing::info!("migrations applied");
            Ok(())
        }
        Cmd::Registry {
            cmd: RegistryCmd::Sync,
        } => {
            let reg = registry()?;
            let n = sync_registry(&connect().await?, reg).await?;
            println!(
                "{}",
                json!({ "registry_version": reg.version(), "changed": n })
            );
            Ok(())
        }
        Cmd::Keygen => {
            let sk = crypto::generate_signing_key();
            let vk = sk.verifying_key();
            println!(
                "{}",
                json!({
                    "private_key": crypto::encode_signing_key(&sk),
                    "public_key": crypto::encode_public_key(&vk),
                    "key_id": crypto::key_id(&vk),
                })
            );
            Ok(())
        }
        Cmd::Import { file } => import(file).await,
        Cmd::Export { dir } => export(dir).await,
        Cmd::Openapi => {
            println!("{}", eatsights_api::openapi_json());
            Ok(())
        }
        Cmd::Sign { key } => {
            let sk = crypto::parse_signing_key(key.trim())?;
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            let doc: Value = serde_json::from_str(&input)?;
            println!("{}", crypto::sign_object(&doc, &sk)?);
            Ok(())
        }
        Cmd::Pubkey { key } => {
            let vk = crypto::parse_signing_key(key.trim())?.verifying_key();
            println!(
                "{}",
                json!({ "public_key": crypto::encode_public_key(&vk), "key_id": crypto::key_id(&vk) })
            );
            Ok(())
        }
        Cmd::Id { prefix } => {
            println!("{prefix}{}", ulid_string());
            Ok(())
        }
    }
}

fn ulid_string() -> String {
    // Reuse the core id generator; strip its prefix.
    let id = eatsights_core::ids::PlaceId::new();
    id.as_str().trim_start_matches("pl_").to_owned()
}

#[tokio::main]
async fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stdout)
        .init();
    if let Err(e) = run(Cli::parse()).await {
        tracing::error!(error = %e, "fatal");
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
