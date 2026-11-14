//! Duplicate detector (AGENTS.md 9.3, thresholds ADR-0006 #6).
//!
//! Follows the change feed for created or edited places, looks for same-category active
//! places in the 3x3 precision-6 geohash cells around the pin, and stores suggestions in
//! `private.merge_suggestions`. It never merges: a person confirms through the admin API.

use std::collections::BTreeSet;
use std::time::Duration;

use eatsights_core::geo;
use eatsights_db::osm::{self as q, Neighbour};
use eatsights_db::{Db, DbError, work};
use tokio::sync::watch;

pub const CURSOR_NAME: &str = "dedupe";
const BATCH: i64 = 500;
/// Precision 6 cells are about 1.2 km x 0.6 km, so the 3x3 block covers the largest radius.
const CELL_PRECISION: usize = 6;

/// ADR-0006 #6: (distance <= near_m AND similarity >= near_sim) OR
/// (distance <= far_m AND similarity >= far_sim), same category.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DedupeConfig {
    pub near_m: f64,
    pub near_sim: f64,
    pub far_m: f64,
    pub far_sim: f64,
}

impl Default for DedupeConfig {
    fn default() -> Self {
        Self {
            near_m: 50.0,
            near_sim: 0.4,
            far_m: 150.0,
            far_sim: 0.7,
        }
    }
}

impl DedupeConfig {
    pub fn is_duplicate(&self, n: &Neighbour) -> bool {
        (n.distance_m <= self.near_m && n.similarity >= self.near_sim)
            || (n.distance_m <= self.far_m && n.similarity >= self.far_sim)
    }
}

/// Find and store suggestions for one place. Returns how many pairs matched.
pub async fn check_place(db: &Db, cfg: &DedupeConfig, place_id: &str) -> Result<usize, DbError> {
    let Some(p) = q::active_place(&db.write, place_id).await? else {
        return Ok(0);
    };
    let cells = geo::cells_3x3(p.lat, p.lng, CELL_PRECISION);
    let max = cfg.near_m.max(cfg.far_m);
    let mut n = 0;
    for nb in q::neighbours(&db.write, &p, &cells, max).await? {
        if cfg.is_duplicate(&nb) {
            let score = (nb.similarity * 1000.0).round() / 1000.0;
            let dist = (nb.distance_m * 10.0).round() / 10.0;
            q::upsert_suggestion(&db.write, "place", &p.id, &nb.id, score, dist).await?;
            n += 1;
        }
    }
    Ok(n)
}

/// Process one batch of changes. Returns change rows consumed.
pub async fn step(db: &Db, cfg: &DedupeConfig) -> Result<usize, DbError> {
    let after = work::cursor(&db.write, CURSOR_NAME).await?;
    let changes = work::changes_after(&db.write, after, BATCH).await?;
    let Some(last) = changes.last().map(|c| c.seq) else {
        return Ok(0);
    };
    let places: BTreeSet<&str> = changes
        .iter()
        .filter(|c| c.entity_type == "place" && matches!(c.op.as_str(), "create" | "edit"))
        .map(|c| c.entity_id.as_str())
        .collect();
    for p in places {
        check_place(db, cfg, p).await?;
    }
    work::set_cursor(&db.write, CURSOR_NAME, last).await?;
    Ok(changes.len())
}

pub async fn run(
    db: Db,
    cfg: DedupeConfig,
    mut wake: watch::Receiver<i64>,
    mut shutdown: watch::Receiver<bool>,
    interval: Duration,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        match step(&db, &cfg).await {
            Ok(n) if n as i64 == BATCH => continue,
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "dedupe step failed"),
        }
        tokio::select! {
            Ok(()) = wake.changed() => {}
            Ok(()) = shutdown.changed() => {}
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nb(d: f64, s: f64) -> Neighbour {
        Neighbour {
            id: "pl_x".into(),
            distance_m: d,
            similarity: s,
        }
    }

    #[test]
    fn thresholds() {
        let c = DedupeConfig::default();
        assert!(c.is_duplicate(&nb(50.0, 0.4)));
        assert!(!c.is_duplicate(&nb(50.1, 0.4)));
        assert!(!c.is_duplicate(&nb(10.0, 0.39)));
        assert!(c.is_duplicate(&nb(150.0, 0.7)));
        assert!(!c.is_duplicate(&nb(150.1, 0.9)));
        assert!(!c.is_duplicate(&nb(100.0, 0.69)));
    }
}
