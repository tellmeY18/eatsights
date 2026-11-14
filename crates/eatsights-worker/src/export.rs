//! Public snapshots (AGENTS.md 9.4): gzip NDJSON per table plus `manifest.json`,
//! one directory per snapshot, a `latest` pointer file, the newest `keep` retained.
//!
//! Layout of `dir`:
//! ```text
//! latest                         -> "20261114T120000Z-1234"
//! 20261114T120000Z-1234/manifest.json
//! 20261114T120000Z-1234/places.ndjson.gz
//! ...
//! ```

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use eatsights_db::export::{Cursor, EXPORT_TABLES, Snapshot};
use eatsights_db::{DbError, PgPool};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

pub const LATEST: &str = "latest";
pub const MANIFEST: &str = "manifest.json";
const BATCH: i64 = 1000;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub records: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub snapshot: String,
    pub generated_at: String,
    pub changes_seq: i64,
    pub license: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone)]
pub struct ExportConfig {
    pub dir: PathBuf,
    pub interval: Duration,
    pub keep: usize,
}

/// Counts and hashes everything written through it.
struct HashWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A snapshot directory name we created (`[0-9A-Za-z-]`, no dots or slashes).
pub fn valid_snapshot_name(s: &str) -> bool {
    !s.is_empty() && s.len() < 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The latest snapshot's manifest, if any.
pub fn latest_manifest(dir: &Path) -> Option<Manifest> {
    let name = std::fs::read_to_string(dir.join(LATEST)).ok()?;
    let name = name.trim();
    if !valid_snapshot_name(name) {
        return None;
    }
    let raw = std::fs::read(dir.join(name).join(MANIFEST)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Write one snapshot of the public data into `dir`, point `latest` at it and prune.
pub async fn write_snapshot(
    pool: &PgPool,
    dir: &Path,
    keep: usize,
) -> Result<Manifest, ExportError> {
    std::fs::create_dir_all(dir)?;
    let mut snap = Snapshot::begin(pool).await?;
    let now = Utc::now();
    let mut name = format!("{}-{}", now.format("%Y%m%dT%H%M%SZ"), snap.changes_seq);
    let mut n = 1;
    while dir.join(&name).exists() {
        name = format!("{}-{}-{n}", now.format("%Y%m%dT%H%M%SZ"), snap.changes_seq);
        n += 1;
    }
    let tmp = dir.join(format!(".tmp-{name}"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;
    let result = write_files(&mut snap, &tmp).await;
    let files = match result {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    let changes_seq = snap.changes_seq;
    snap.finish().await?;
    let manifest = Manifest {
        snapshot: name.clone(),
        generated_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        changes_seq,
        license: "CC0-1.0".into(),
        files,
    };
    std::fs::write(tmp.join(MANIFEST), serde_json::to_vec_pretty(&manifest)?)?;
    std::fs::rename(&tmp, dir.join(&name))?;
    let ptr = dir.join(".latest.tmp");
    std::fs::write(&ptr, format!("{name}\n"))?;
    std::fs::rename(&ptr, dir.join(LATEST))?;
    prune(dir, keep, &name)?;
    Ok(manifest)
}

async fn write_files(snap: &mut Snapshot, tmp: &Path) -> Result<Vec<FileEntry>, ExportError> {
    let mut files = Vec::with_capacity(EXPORT_TABLES.len());
    for table in EXPORT_TABLES {
        let fname = format!("{}.ndjson.gz", table.name);
        let file = std::fs::File::create(tmp.join(&fname))?;
        let mut gz = GzEncoder::new(
            HashWriter {
                inner: BufWriter::new(file),
                hasher: Sha256::new(),
                bytes: 0,
            },
            Compression::default(),
        );
        let mut cursor = Cursor::default();
        let mut records = 0u64;
        loop {
            let docs = snap.next_batch(table, &mut cursor, BATCH).await?;
            if docs.is_empty() {
                break;
            }
            for d in &docs {
                serde_json::to_writer(&mut gz, d)?;
                gz.write_all(b"\n")?;
            }
            records += docs.len() as u64;
            if (docs.len() as i64) < BATCH {
                break;
            }
        }
        let mut hw = gz.finish()?;
        hw.flush()?;
        hw.inner.get_ref().sync_all()?;
        files.push(FileEntry {
            name: fname,
            bytes: hw.bytes,
            sha256: hex(&hw.hasher.finalize()),
            records,
        });
    }
    Ok(files)
}

/// Remove all but the newest `keep` snapshot directories (never `current`) and stale temp dirs.
fn prune(dir: &Path, keep: usize, current: &str) -> io::Result<()> {
    let mut snaps = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if !e.file_type()?.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(".tmp-") && name != format!(".tmp-{current}") {
            std::fs::remove_dir_all(e.path())?;
        } else if valid_snapshot_name(&name) {
            snaps.push(name);
        }
    }
    snaps.sort();
    let excess = snaps.len().saturating_sub(keep.max(1));
    for name in snaps.into_iter().take(excess) {
        if name != current {
            std::fs::remove_dir_all(dir.join(name))?;
        }
    }
    Ok(())
}

fn generated_at(m: &Manifest) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&m.generated_at)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Export on start when the latest snapshot is older than `interval`, then every `interval`.
pub async fn run(pool: PgPool, cfg: ExportConfig, mut shutdown: watch::Receiver<bool>) {
    tracing::info!(dir = %cfg.dir.display(), "export worker started");
    loop {
        if *shutdown.borrow() {
            break;
        }
        let due_in = latest_manifest(&cfg.dir)
            .and_then(|m| generated_at(&m))
            .map(|at| {
                let age = (Utc::now() - at).to_std().unwrap_or_default();
                cfg.interval.saturating_sub(age)
            })
            .unwrap_or_default();
        let wait = if due_in.is_zero() {
            match write_snapshot(&pool, &cfg.dir, cfg.keep).await {
                Ok(m) => {
                    tracing::info!(snapshot = %m.snapshot, changes_seq = m.changes_seq, "export written");
                    cfg.interval.max(Duration::from_secs(60))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "export failed; retrying in 5 minutes");
                    Duration::from_secs(300)
                }
            }
        } else {
            due_in
        };
        tokio::select! {
            Ok(()) = shutdown.changed() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
    tracing::info!("export worker stopped");
}
