//! Fact recomputation after a merge or an undone merge.
//!
//! Aggregation follows `merged_into` (`work::resolve_subject` / `work::merged_sources`):
//! a merged record's observations count for its survivor. When a merge changes, every
//! property observed on the affected subjects is recomputed; facts left without observations
//! (the loser's own facts after a merge, the survivor's borrowed ones after an undo) are deleted.

use chrono::{DateTime, Utc};
use eatsights_core::registry::Registry;
use eatsights_db::{Db, DbError, admin, work};

use crate::aggregate::AggConfig;
use crate::runner::recompute;

/// Recompute the facts of `subject_key` (`place:<id>` or `menu_item:<id>`) after a
/// merge/unmerge. Returns the places whose menus need rebuilding.
pub async fn recompute_subject(
    db: &Db,
    registry: &Registry,
    cfg: &AggConfig,
    subject_key: &str,
    now: DateTime<Utc>,
) -> Result<Vec<String>, DbError> {
    let target = work::resolve_subject(&db.write, subject_key).await?;
    let mut keys = work::merged_sources(&db.write, subject_key).await?;
    if target != subject_key {
        keys.push(target.clone());
    }
    let props = admin::subject_properties(&db.write, &keys).await?;
    let target_sources = work::merged_sources(&db.write, &target).await?;
    let mut places = Vec::new();
    for p in &props {
        if target != subject_key {
            admin::delete_fact(&db.write, subject_key, p).await?;
        }
        if work::observations_for(&db.write, &target_sources, p)
            .await?
            .is_empty()
        {
            admin::delete_fact(&db.write, &target, p).await?;
        } else if let Some(pl) = recompute(db, registry, cfg, &target, p, now).await? {
            places.push(pl);
        }
    }
    Ok(places)
}
