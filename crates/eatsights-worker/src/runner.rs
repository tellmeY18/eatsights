//! The aggregation worker loop: follows `public.changes` from a persisted cursor,
//! recomputes facts of the touched (subject, property) pairs and rebuilds menu documents.
//! Idempotent: re-running over the same changes produces the same rows.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use eatsights_core::registry::{Registry, Strategy as RegStrategy};
use eatsights_db::{Db, DbError, work};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::aggregate::{
    self, AggConfig, ContributorClass, Kind, ObsInput, PropertyDef, SourceType, Strategy,
};

pub const CURSOR_NAME: &str = "aggregate";
const BATCH: i64 = 500;

fn strategy(s: RegStrategy) -> Strategy {
    match s {
        RegStrategy::LatestWeighted => Strategy::LatestWeighted,
        RegStrategy::Median => Strategy::Median,
        RegStrategy::Mode => Strategy::Mode,
        RegStrategy::Union => Strategy::Union,
        RegStrategy::Schedule => Strategy::Schedule,
        RegStrategy::Derived => Strategy::Derived,
    }
}

fn class(s: &str) -> ContributorClass {
    match s {
        "member" => ContributorClass::Member,
        "trusted" => ContributorClass::Trusted,
        "verifier" => ContributorClass::Verifier,
        "vendor" => ContributorClass::Vendor,
        "import" => ContributorClass::Import,
        _ => ContributorClass::Unverified,
    }
}

fn to_input(row: &work::ObsRow) -> Option<ObsInput> {
    let d = &row.doc;
    let kind = match d.get("kind")?.as_str()? {
        "value" => Kind::Value,
        "confirm" => Kind::Confirm,
        "dispute" => Kind::Dispute,
        _ => return None,
    };
    let source_type = match d.get("source_type")?.as_str()? {
        "vendor" => SourceType::Vendor,
        "import" => SourceType::Import,
        _ => SourceType::Customer,
    };
    Some(ObsInput {
        id: d.get("id")?.as_str()?.to_owned(),
        kind,
        value: d.get("value").cloned(),
        refers_to: d
            .get("refers_to")
            .and_then(Value::as_str)
            .map(str::to_owned),
        observed_at: row.observed_at,
        class: class(&row.class),
        pseudonym: row.pseudonym.clone(),
        source_type,
    })
}

type SubjectIds = (Option<String>, Option<String>, Option<String>);

fn subject_json(subject_key: &str, place_id: Option<&str>) -> Option<(Value, SubjectIds)> {
    let (scope, id) = subject_key.split_once(':')?;
    match scope {
        "place" => Some((
            json!({"scope": "place", "place_id": id}),
            (Some(id.to_owned()), None, None),
        )),
        "menu_item" => {
            let p = place_id?;
            Some((
                json!({"scope": "menu_item", "place_id": p, "menu_item_id": id}),
                (Some(p.to_owned()), Some(id.to_owned()), None),
            ))
        }
        "dish" => Some((
            json!({"scope": "dish", "dish_id": id}),
            (None, None, Some(id.to_owned())),
        )),
        _ => None,
    }
}

/// Recompute one (subject, property) fact. Returns the place whose menu needs rebuilding.
pub async fn recompute(
    db: &Db,
    registry: &Registry,
    cfg: &AggConfig,
    subject_key: &str,
    property: &str,
    now: DateTime<Utc>,
) -> Result<Option<String>, DbError> {
    let Some(prop) = registry.get(property) else {
        return Ok(None);
    };
    let target = work::resolve_subject(&db.write, subject_key).await?;
    let place_id = match target.split_once(':') {
        Some(("place", id)) => Some(id.to_owned()),
        Some(("menu_item", id)) => work::place_of(&db.write, "menu_item", id).await?,
        _ => None,
    };
    let Some((subject, ids)) = subject_json(&target, place_id.as_deref()) else {
        return Ok(None);
    };
    let sources = work::merged_sources(&db.write, &target).await?;
    let rows = work::observations_for(&db.write, &sources, property).await?;
    let inputs: Vec<ObsInput> = rows.iter().filter_map(to_input).collect();
    let def = PropertyDef {
        id: prop.id.clone(),
        half_life_hours: prop.half_life_hours,
        strategy: strategy(prop.aggregation.strategy),
        field: prop.aggregation.field.clone(),
        window: prop.aggregation.window,
    };
    let Some(fact) = aggregate::aggregate(&def, &subject, &inputs, now, cfg) else {
        return Ok(None);
    };
    let changed = work::upsert_fact(
        &db.write,
        &target,
        property,
        (ids.0.as_deref(), ids.1.as_deref(), ids.2.as_deref()),
        &fact,
        now,
    )
    .await?;
    Ok(if changed { place_id } else { None })
}

pub async fn rebuild_menu(db: &Db, place_id: &str, now: DateTime<Utc>) -> Result<bool, DbError> {
    match eatsights_db::read::build_menu_doc(&db.write, place_id, now).await? {
        Some(doc) => work::store_menu_doc(&db.write, place_id, &doc, now).await,
        None => Ok(false),
    }
}

/// Process one batch of changes. Returns how many change rows were consumed.
pub async fn step(db: &Db, registry: &Registry, cfg: &AggConfig) -> Result<usize, DbError> {
    let after = work::cursor(&db.write, CURSOR_NAME).await?;
    let changes = work::changes_after(&db.write, after, BATCH).await?;
    let Some(last) = changes.last().map(|c| c.seq) else {
        return Ok(0);
    };
    let now = Utc::now();
    let mut obs_ids = Vec::new();
    let mut places: BTreeSet<String> = BTreeSet::new();
    for c in &changes {
        match c.entity_type.as_str() {
            "observation" => obs_ids.push(c.entity_id.clone()),
            "place" => {
                places.insert(c.entity_id.clone());
            }
            "section" | "menu_item" => {
                if let Some(p) = work::place_of(&db.write, &c.entity_type, &c.entity_id).await? {
                    places.insert(p);
                }
            }
            _ => {}
        }
    }
    let pairs = work::pairs_for_observations(&db.write, &obs_ids).await?;
    for (subject_key, property, _) in &pairs {
        if let Some(p) = recompute(db, registry, cfg, subject_key, property, now).await? {
            places.insert(p);
        }
    }
    for p in &places {
        rebuild_menu(db, p, now).await?;
    }
    work::set_cursor(&db.write, CURSOR_NAME, last).await?;
    Ok(changes.len())
}

/// Run until `shutdown` turns true. Wakes on `wake` (NOTIFY) or every `interval`.
pub async fn run(
    db: Db,
    registry: &'static Registry,
    cfg: AggConfig,
    mut wake: watch::Receiver<i64>,
    mut shutdown: watch::Receiver<bool>,
    interval: Duration,
) {
    tracing::info!(algorithm_version = %cfg.algorithm_version, "aggregation worker started");
    loop {
        if *shutdown.borrow() {
            break;
        }
        match step(&db, registry, &cfg).await {
            Ok(n) if n as i64 == BATCH => continue,
            Ok(n) => {
                if n > 0 {
                    tracing::debug!(changes = n, "worker step");
                }
            }
            Err(e) => tracing::warn!(error = %e, "worker step failed"),
        }
        tokio::select! {
            Ok(()) = wake.changed() => {}
            Ok(()) = shutdown.changed() => {}
            _ = tokio::time::sleep(interval) => {}
        }
    }
    tracing::info!("aggregation worker stopped");
}
