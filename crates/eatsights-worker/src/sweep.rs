//! Periodic re-evaluation of facts whose status can change by time alone (they go
//! `stale` without new observations). Only changed facts are written, so a sweep over
//! unchanged data appends nothing to `changes`; running it twice is harmless.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use eatsights_core::registry::Registry;
use eatsights_db::{Db, DbError, export};
use tokio::sync::watch;

use crate::aggregate::AggConfig;
use crate::runner;

const CHUNK: i64 = 500;

/// One full pass at the current time.
pub async fn sweep(db: &Db, registry: &Registry, cfg: &AggConfig) -> Result<usize, DbError> {
    sweep_at(db, registry, cfg, Utc::now()).await
}

/// One full pass as of `now`. Returns how many candidate facts were re-evaluated.
pub async fn sweep_at(
    db: &Db,
    registry: &Registry,
    cfg: &AggConfig,
    now: DateTime<Utc>,
) -> Result<usize, DbError> {
    let mut after: Option<(String, String)> = None;
    let mut evaluated = 0usize;
    let mut places = BTreeSet::new();
    loop {
        let batch = export::stale_candidates(
            &db.write,
            cfg.thresholds.stale_half_lives,
            now,
            after.as_ref(),
            CHUNK,
        )
        .await?;
        for (subject_key, property) in &batch {
            if let Some(p) =
                runner::recompute(db, registry, cfg, subject_key, property, now).await?
            {
                places.insert(p);
            }
            evaluated += 1;
        }
        if (batch.len() as i64) < CHUNK {
            break;
        }
        after = batch.last().cloned();
    }
    for p in &places {
        runner::rebuild_menu(db, p, now).await?;
    }
    Ok(evaluated)
}

pub async fn run(
    db: Db,
    registry: &'static Registry,
    cfg: AggConfig,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            Ok(()) = shutdown.changed() => {}
            _ = tokio::time::sleep(interval) => {}
        }
        if *shutdown.borrow() {
            break;
        }
        match sweep(&db, registry, &cfg).await {
            Ok(n) if n > 0 => tracing::info!(evaluated = n, "stale sweep"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "stale sweep failed"),
        }
    }
}
