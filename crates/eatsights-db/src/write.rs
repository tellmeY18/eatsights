//! Write-path queries. Each accepted write is one transaction that also appends
//! to `public.changes` and notifies `eatsights_changes`.

use std::collections::HashSet;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use eatsights_core::geo;
use eatsights_core::trust::{self, Edge, KeyNode, Level, TrustConfig};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

use crate::read::EntityTable;
use crate::{DbError, Result, add_change};

#[derive(Debug, Clone, PartialEq)]
pub struct KeyRow {
    pub key_id: String,
    pub public_key: String,
    pub level: String,
    pub status: String,
    pub depth: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub successor_key_id: Option<String>,
    pub vouch_cooldown_until: Option<DateTime<Utc>>,
}

const KEY_COLS: &str =
    "key_id, public_key, level, status, depth, created_at, successor_key_id, vouch_cooldown_until";

fn key_row(r: &sqlx::postgres::PgRow) -> Result<KeyRow> {
    Ok(KeyRow {
        key_id: r.try_get("key_id")?,
        public_key: r.try_get("public_key")?,
        level: r.try_get("level")?,
        status: r.try_get("status")?,
        depth: r.try_get("depth")?,
        created_at: r.try_get("created_at")?,
        successor_key_id: r.try_get("successor_key_id")?,
        vouch_cooldown_until: r.try_get("vouch_cooldown_until")?,
    })
}

pub async fn get_key(pool: &PgPool, key_id: &str) -> Result<Option<KeyRow>> {
    let row = sqlx::query(&format!(
        "SELECT {KEY_COLS} FROM private.keys WHERE key_id = $1"
    ))
    .bind(key_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(key_row).transpose()
}

/// Register an `unverified` key. Returns `false` when the key already existed.
pub async fn register_key(
    pool: &PgPool,
    key_id: &str,
    public_key: &str,
    created_at: DateTime<Utc>,
) -> Result<bool> {
    let r = sqlx::query(
        "INSERT INTO private.keys (key_id, public_key, level, status, depth, created_at)
         VALUES ($1, $2, 'unverified', 'active', NULL, $3) ON CONFLICT (key_id) DO NOTHING",
    )
    .bind(key_id)
    .bind(public_key)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

fn key_doc(k: &KeyRow) -> Value {
    json!({
        "key_id": k.key_id,
        "algorithm": "ed25519",
        "public_key": k.public_key,
        "created_at": k.created_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "status": k.status,
        "level": k.level,
        "depth": k.depth.unwrap_or(0),
    })
}

type Tx<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

/// Make `public.published_keys` hold exactly the active root and verifier keys.
async fn sync_published(tx: &mut Tx<'_>) -> Result<()> {
    let rows = sqlx::query(&format!(
        "SELECT {KEY_COLS} FROM private.keys WHERE status = 'active' AND level IN ('verifier', 'root')"
    ))
    .fetch_all(&mut **tx)
    .await?;
    let mut keep = HashSet::new();
    for r in &rows {
        let k = key_row(r)?;
        let changed = sqlx::query(
            "INSERT INTO public.published_keys (key_id, doc, updated_at) VALUES ($1, $2, now())
             ON CONFLICT (key_id) DO UPDATE SET doc = EXCLUDED.doc, updated_at = now()
             WHERE published_keys.doc IS DISTINCT FROM EXCLUDED.doc",
        )
        .bind(&k.key_id)
        .bind(key_doc(&k))
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if changed > 0 {
            add_change(tx, "key", &k.key_id, "publish").await?;
        }
        keep.insert(k.key_id);
    }
    let published: Vec<String> = sqlx::query_scalar("SELECT key_id FROM public.published_keys")
        .fetch_all(&mut **tx)
        .await?;
    for k in published.iter().filter(|k| !keep.contains(*k)) {
        sqlx::query("DELETE FROM public.published_keys WHERE key_id = $1")
            .bind(k)
            .execute(&mut **tx)
            .await?;
        add_change(tx, "key", k, "unpublish").await?;
    }
    Ok(())
}

/// Serialise trust-graph changes.
async fn lock_trust(tx: &mut Tx<'_>) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(7243001)")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Recompute every key's level and depth from the private graph
/// ([`trust::recompute`]), store the differences and republish keys.
async fn recompute_trust(tx: &mut Tx<'_>, max_depth: i32) -> Result<()> {
    let rows =
        sqlx::query("SELECT key_id, status, level, depth, level_cap, demoted_at FROM private.keys")
            .fetch_all(&mut **tx)
            .await?;
    let mut keys = Vec::with_capacity(rows.len());
    let mut current = std::collections::HashMap::new();
    for r in &rows {
        let id: String = r.try_get("key_id")?;
        let level: String = r.try_get("level")?;
        let depth: Option<i32> = r.try_get("depth")?;
        let cap: Option<String> = r.try_get("level_cap")?;
        keys.push(KeyNode {
            key_id: id.clone(),
            status: r.try_get("status")?,
            is_root: level == "root",
            level_cap: cap.as_deref().and_then(Level::parse),
            demoted_at: r.try_get("demoted_at")?,
        });
        current.insert(id, (level, depth));
    }
    let erows = sqlx::query(
        "SELECT issuer_key_id, subject_key_id, type, raw->>'level_granted' AS granted, received_at
           FROM private.attestations
          WHERE type IN ('vouch', 'rotate') AND revoked_at IS NULL
            AND (raw->>'expires_at' IS NULL OR (raw->>'expires_at')::timestamptz > now())",
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut edges = Vec::with_capacity(erows.len());
    for r in &erows {
        let kind: String = r.try_get("type")?;
        let granted: Option<String> = r.try_get("granted")?;
        let granted = if kind == "vouch" {
            match granted.as_deref().and_then(Level::parse) {
                Some(l) => Some(l),
                None => continue,
            }
        } else {
            None
        };
        edges.push(Edge {
            issuer: r.try_get("issuer_key_id")?,
            subject: r.try_get("subject_key_id")?,
            granted,
            received_at: r.try_get("received_at")?,
        });
    }
    let standings = trust::recompute(&keys, &edges, max_depth);
    for (id, s) in &standings {
        let Some((level, depth)) = current.get(id) else {
            continue;
        };
        if level == "root" || (level.as_str() == s.level.as_str() && *depth == s.depth) {
            continue;
        }
        sqlx::query("UPDATE private.keys SET level = $2, depth = $3 WHERE key_id = $1")
            .bind(id)
            .bind(s.level.as_str())
            .bind(s.depth)
            .execute(&mut **tx)
            .await?;
    }
    sync_published(tx).await
}

/// Upsert the root key (level root, depth 0), recompute the graph and publish.
pub async fn upsert_root_key(
    pool: &PgPool,
    key_id: &str,
    public_key: &str,
    max_depth: i32,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    lock_trust(&mut tx).await?;
    // Only the configured root stays root.
    sqlx::query(
        "UPDATE private.keys SET level = 'unverified', status = 'revoked', depth = NULL
         WHERE level = 'root' AND key_id <> $1",
    )
    .bind(key_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO private.keys (key_id, public_key, level, status, depth, created_at)
         VALUES ($1, $2, 'root', 'active', 0, now())
         ON CONFLICT (key_id) DO UPDATE SET level = 'root', status = 'active', depth = 0",
    )
    .bind(key_id)
    .bind(public_key)
    .execute(&mut *tx)
    .await?;
    recompute_trust(&mut tx, max_depth).await?;
    tx.commit().await?;
    Ok(())
}

/// Mirror the embedded registry into `public.registry_properties`.
pub async fn sync_registry(
    pool: &PgPool,
    version: &str,
    hash: &str,
    entries: &[(String, Value)],
) -> Result<u64> {
    let mut tx = pool.begin().await?;
    let mut changed = 0;
    for (id, doc) in entries {
        changed += sqlx::query(
            "INSERT INTO public.registry_properties (id, doc, registry_version, registry_hash, synced_at)
             VALUES ($1, $2, $3, $4, now())
             ON CONFLICT (id) DO UPDATE SET doc = EXCLUDED.doc, registry_version = EXCLUDED.registry_version,
               registry_hash = EXCLUDED.registry_hash, synced_at = now()
             WHERE registry_properties.registry_hash IS DISTINCT FROM EXCLUDED.registry_hash",
        )
        .bind(id)
        .bind(doc)
        .bind(version)
        .bind(hash)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    if changed > 0 {
        add_change(&mut tx, "registry", hash, "sync").await?;
    }
    tx.commit().await?;
    Ok(changed)
}

pub async fn has_vendor_binding(
    pool: &PgPool,
    key_id: &str,
    place_id: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM public.vendor_bindings WHERE holder_key_id = $1 AND place_id = $2
           AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > $3))",
    )
    .bind(key_id)
    .bind(place_id)
    .bind(now)
    .fetch_one(pool)
    .await?)
}

/// The stored per-item result of an earlier submission with this id.
pub async fn submission_result(pool: &PgPool, id: &str) -> Result<Option<Value>> {
    Ok(
        sqlx::query_scalar("SELECT result FROM private.submissions WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

#[derive(Debug, Clone)]
pub struct NewObservation<'a> {
    pub id: &'a str,
    pub property: &'a str,
    pub scope: &'a str,
    pub place_id: Option<&'a str>,
    pub menu_item_id: Option<&'a str>,
    pub dish_id: Option<&'a str>,
    pub subject_key: &'a str,
    pub public_doc: &'a Value,
    pub pseudonym: &'a str,
    pub salt_version: i32,
    pub class: &'a str,
    pub observed_at: DateTime<Utc>,
    pub submitted_at: DateTime<Utc>,
    pub raw: &'a Value,
    pub key_id: &'a str,
    pub result: &'a Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Accepted,
    /// Already stored; carries the original result where one exists.
    Duplicate(Option<Value>),
}

pub async fn insert_observation(pool: &PgPool, o: &NewObservation<'_>) -> Result<Outcome> {
    let mut tx = pool.begin().await?;
    // Subject must exist (and a menu item must belong to the stated place).
    let ok: bool =
        match o.scope {
            "place" => {
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM public.places WHERE id = $1)")
                    .bind(o.place_id)
                    .fetch_one(&mut *tx)
                    .await?
            }
            "menu_item" => sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM public.menu_items WHERE id = $1 AND place_id = $2)",
            )
            .bind(o.menu_item_id)
            .bind(o.place_id)
            .fetch_one(&mut *tx)
            .await?,
            _ => {
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM public.dishes WHERE id = $1)")
                    .bind(o.dish_id)
                    .fetch_one(&mut *tx)
                    .await?
            }
        };
    if !ok {
        return Err(DbError::BadReference(format!(
            "unknown subject {}",
            o.subject_key
        )));
    }
    let fresh: Option<String> = sqlx::query_scalar(
        "INSERT INTO public.observation_ids (id, submitted_at, property, subject_key, place_id)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO NOTHING RETURNING id",
    )
    .bind(o.id)
    .bind(o.submitted_at)
    .bind(o.property)
    .bind(o.subject_key)
    .bind(o.place_id)
    .fetch_optional(&mut *tx)
    .await?;
    if fresh.is_none() {
        tx.rollback().await?;
        return Ok(Outcome::Duplicate(submission_result(pool, o.id).await?));
    }
    sqlx::query(
        "INSERT INTO public.observations (id, property, scope, place_id, menu_item_id, dish_id, subject_key, doc,
           pseudonym, pseudonym_salt_version, class, observed_at, submitted_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(o.id)
    .bind(o.property)
    .bind(o.scope)
    .bind(o.place_id)
    .bind(o.menu_item_id)
    .bind(o.dish_id)
    .bind(o.subject_key)
    .bind(o.public_doc)
    .bind(o.pseudonym)
    .bind(o.salt_version)
    .bind(o.class)
    .bind(o.observed_at)
    .bind(o.submitted_at)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO private.submissions (id, raw, key_id, result) VALUES ($1, $2, $3, $4)",
    )
    .bind(o.id)
    .bind(o.raw)
    .bind(o.key_id)
    .bind(o.result)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO private.salt_versions (version) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(o.salt_version)
        .execute(&mut *tx)
        .await?;
    add_change(&mut tx, "observation", o.id, "create").await?;
    tx.commit().await?;
    Ok(Outcome::Accepted)
}

// ------------------------------------------------------------------ entities

#[derive(Debug, Clone)]
pub struct NewEntitySubmission<'a> {
    pub id: &'a str,
    pub raw: &'a Value,
    pub key_id: &'a str,
    pub entity_type: &'a str,
    pub entity_id: &'a str,
    pub op: &'a str,
}

pub async fn entity_submission_exists(pool: &PgPool, id: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM private.entity_submissions WHERE id = $1)",
        )
        .bind(id)
        .fetch_one(pool)
        .await?,
    )
}

fn text_of(doc: &Value, fields: &[&str]) -> String {
    let mut parts = Vec::new();
    for f in fields {
        match doc.get(*f) {
            Some(Value::Object(m)) => {
                parts.extend(m.values().filter_map(Value::as_str).map(str::to_owned))
            }
            Some(Value::Array(a)) => {
                for m in a.iter().filter_map(Value::as_object) {
                    parts.extend(m.values().filter_map(Value::as_str).map(str::to_owned));
                }
            }
            _ => {}
        }
    }
    parts.join(" ")
}

fn s(doc: &Value, f: &str) -> Option<String> {
    doc.get(f).and_then(Value::as_str).map(str::to_owned)
}

fn ts(doc: &Value, f: &str, fallback: DateTime<Utc>) -> DateTime<Utc> {
    doc.get(f)
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(fallback)
}

async fn upsert_entity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: EntityTable,
    doc: &Value,
    create: bool,
    now: DateTime<Utc>,
) -> Result<u64> {
    let id = s(doc, "id").unwrap_or_default();
    let created_at = ts(doc, "created_at", now);
    let conflict = if create { "DO NOTHING" } else { "" };
    let r = match table {
        EntityTable::Places => {
            let lat = doc
                .pointer("/location/lat")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let lng = doc
                .pointer("/location/lng")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let sql = if create {
                format!(
                    "INSERT INTO public.places (id, doc, search_text, category, city, lat, lng, geohash, record_state, merged_into, created_at, updated_at)
                     VALUES ($1, $2, public.es_fold($3), $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT (id) {conflict}"
                )
            } else {
                "UPDATE public.places SET doc = $2, search_text = public.es_fold($3), category = $4, city = $5, lat = $6,
                   lng = $7, geohash = $8, record_state = $9, merged_into = $10, created_at = $11, updated_at = $12 WHERE id = $1"
                    .to_owned()
            };
            sqlx::query(&sql)
                .bind(&id)
                .bind(doc)
                .bind(text_of(doc, &["names", "aliases"]))
                .bind(s(doc, "category"))
                .bind(s(doc, "city"))
                .bind(lat)
                .bind(lng)
                .bind(geo::encode(lat, lng, 9))
                .bind(s(doc, "record_state").unwrap_or_else(|| "active".into()))
                .bind(s(doc, "merged_into"))
                .bind(created_at)
                .bind(now)
                .execute(&mut **tx)
                .await
        }
        EntityTable::Sections => {
            let sql = if create {
                format!(
                    "INSERT INTO public.sections (id, place_id, doc, sort, created_at, updated_at)
                     VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT (id) {conflict}"
                )
            } else {
                "UPDATE public.sections SET doc = $3, sort = $4, updated_at = $5 WHERE id = $1 AND place_id = $2".to_owned()
            };
            sqlx::query(&sql)
                .bind(&id)
                .bind(s(doc, "place_id"))
                .bind(doc)
                .bind(doc.get("sort").and_then(Value::as_i64).map(|v| v as i32))
                .bind(now)
                .execute(&mut **tx)
                .await
        }
        EntityTable::Dishes => {
            let sql = if create {
                format!(
                    "INSERT INTO public.dishes (id, doc, search_text, created_at, updated_at)
                     VALUES ($1, $2, public.es_fold($3), $4, $4) ON CONFLICT (id) {conflict}"
                )
            } else {
                "UPDATE public.dishes SET doc = $2, search_text = public.es_fold($3), updated_at = $4 WHERE id = $1".to_owned()
            };
            sqlx::query(&sql)
                .bind(&id)
                .bind(doc)
                .bind(text_of(doc, &["labels", "aliases"]))
                .bind(now)
                .execute(&mut **tx)
                .await
        }
        EntityTable::MenuItems => {
            let sql = if create {
                format!(
                    "INSERT INTO public.menu_items (id, place_id, section_id, dish_id, doc, record_state, merged_into, created_at, updated_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (id) {conflict}"
                )
            } else {
                "UPDATE public.menu_items SET section_id = $3, dish_id = $4, doc = $5, record_state = $6, merged_into = $7,
                   created_at = $8, updated_at = $9 WHERE id = $1 AND place_id = $2"
                    .to_owned()
            };
            sqlx::query(&sql)
                .bind(&id)
                .bind(s(doc, "place_id"))
                .bind(s(doc, "section_id"))
                .bind(s(doc, "dish_id"))
                .bind(doc)
                .bind(s(doc, "record_state").unwrap_or_else(|| "active".into()))
                .bind(s(doc, "merged_into"))
                .bind(created_at)
                .bind(now)
                .execute(&mut **tx)
                .await
        }
    };
    Ok(r.map_err(DbError::classify)?.rows_affected())
}

/// Store one accepted entity submission. `doc` is the full entity with server fields filled.
/// For `create`, an existing entity with identical content gives `Duplicate`, different content `Conflict`.
pub async fn write_entity(
    pool: &PgPool,
    es: &NewEntitySubmission<'_>,
    doc: &Value,
    create: bool,
    now: DateTime<Utc>,
) -> Result<Outcome> {
    let table = EntityTable::from_entity_type(es.entity_type)
        .ok_or_else(|| DbError::BadReference(format!("entity type {}", es.entity_type)))?;
    let mut tx = pool.begin().await?;
    if table == EntityTable::MenuItems {
        // section and place must agree
        if let Some(sec) = s(doc, "section_id") {
            let same: Option<bool> =
                sqlx::query_scalar("SELECT place_id = $2 FROM public.sections WHERE id = $1")
                    .bind(&sec)
                    .bind(s(doc, "place_id"))
                    .fetch_optional(&mut *tx)
                    .await?;
            if same != Some(true) {
                return Err(DbError::BadReference(format!(
                    "section {sec} is not in this place"
                )));
            }
        }
    }
    let fresh: Option<String> = sqlx::query_scalar(
        "INSERT INTO private.entity_submissions (id, raw, key_id, entity_type, entity_id, op)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING RETURNING id",
    )
    .bind(es.id)
    .bind(es.raw)
    .bind(es.key_id)
    .bind(es.entity_type)
    .bind(es.entity_id)
    .bind(es.op)
    .fetch_optional(&mut *tx)
    .await?;
    if fresh.is_none() {
        tx.rollback().await?;
        return Ok(Outcome::Duplicate(None));
    }
    let n = upsert_entity(&mut tx, table, doc, create, now).await?;
    if n == 0 {
        if !create {
            return Err(DbError::NotFound(es.entity_id.to_owned()));
        }
        let existing: Option<Value> = sqlx::query_scalar(&format!(
            "SELECT doc FROM public.{} WHERE id = $1",
            table.name()
        ))
        .bind(es.entity_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.rollback().await?;
        let strip = |v: &Value| {
            let mut v = v.clone();
            if let Some(o) = v.as_object_mut() {
                o.remove("created_at");
            }
            v
        };
        return match existing {
            Some(e) if strip(&e) == strip(doc) => Ok(Outcome::Duplicate(None)),
            _ => Err(DbError::Conflict(format!(
                "{} exists with different content",
                es.entity_id
            ))),
        };
    }
    add_change(&mut tx, es.entity_type, es.entity_id, es.op).await?;
    tx.commit().await?;
    Ok(Outcome::Accepted)
}

/// Current entity document and its place (for permission checks and edits).
pub async fn entity_for_edit(pool: &PgPool, entity_type: &str, id: &str) -> Result<Option<Value>> {
    match EntityTable::from_entity_type(entity_type) {
        Some(t) => crate::read::get_doc(pool, t, id).await,
        None => Ok(None),
    }
}

// ------------------------------------------------------------------ attestations

#[derive(Debug, Clone, PartialEq)]
pub struct AttestationRow {
    pub id: String,
    pub kind: String,
    pub issuer_key_id: String,
    pub subject_key_id: String,
    pub revoked: bool,
}

pub async fn get_attestation(pool: &PgPool, id: &str) -> Result<Option<AttestationRow>> {
    let row = sqlx::query(
        "SELECT id, type, issuer_key_id, subject_key_id, revoked_at IS NOT NULL AS revoked FROM private.attestations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(r) => Some(AttestationRow {
            id: r.try_get("id")?,
            kind: r.try_get("type")?,
            issuer_key_id: r.try_get("issuer_key_id")?,
            subject_key_id: r.try_get("subject_key_id")?,
            revoked: r.try_get("revoked")?,
        }),
        None => None,
    })
}

#[derive(Debug, Clone)]
pub struct NewAttestation<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub issuer_key_id: &'a str,
    pub subject_key_id: &'a str,
    pub raw: &'a Value,
    pub place_id: Option<&'a str>,
    pub revokes: Option<&'a str>,
    pub issued_at: DateTime<Utc>,
    /// Effective expiry (for `vendor_of`, defaulted by the caller).
    pub expires_at: Option<DateTime<Utc>>,
    /// For a revoke of a vouch by root or a verifier other than its issuer: the
    /// vouch issuer, who receives an abuse strike (ADR-0007).
    pub strike_against: Option<&'a str>,
}

/// The key that currently stands for `key_id` (follows rotation successors).
async fn current_holder(tx: &mut Tx<'_>, key_id: &str) -> Result<String> {
    let mut k = key_id.to_owned();
    for _ in 0..16 {
        let next: Option<Option<String>> =
            sqlx::query_scalar("SELECT successor_key_id FROM private.keys WHERE key_id = $1")
                .bind(&k)
                .fetch_optional(&mut **tx)
                .await?;
        match next.flatten() {
            Some(n) => k = n,
            None => break,
        }
    }
    Ok(k)
}

/// Record a strike; demote and impose a vouch cooldown at the threshold.
async fn strike(
    tx: &mut Tx<'_>,
    key_id: &str,
    vouch_id: &str,
    revoke_id: &str,
    cfg: &TrustConfig,
) -> Result<()> {
    let holder = current_holder(tx, key_id).await?;
    let row: Option<(String, Option<String>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT level, level_cap, demoted_at FROM private.keys WHERE key_id = $1 FOR UPDATE",
    )
    .bind(&holder)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((level, cap, demoted_at)) = row else {
        return Ok(());
    };
    if level == "root" {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO private.strikes (key_id, attestation_id, revoke_id) VALUES ($1, $2, $3)",
    )
    .bind(&holder)
    .bind(vouch_id)
    .bind(revoke_id)
    .execute(&mut **tx)
    .await?;
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM private.strikes WHERE key_id = $1
           AND at > now() - make_interval(days => $2::int)
           AND at > COALESCE($3, '-infinity'::timestamptz)",
    )
    .bind(&holder)
    .bind(i32::try_from(cfg.strike_window_days).unwrap_or(i32::MAX))
    .bind(demoted_at)
    .fetch_one(&mut **tx)
    .await?;
    if n < cfg.strikes {
        return Ok(());
    }
    let cur = Level::parse(&level).unwrap_or(Level::Unverified);
    let mut new_cap = cur.demoted();
    if let Some(c) = cap.as_deref().and_then(Level::parse) {
        new_cap = new_cap.min(c);
    }
    let now = Utc::now();
    sqlx::query(
        "UPDATE private.keys SET level_cap = $2, demoted_at = $3, vouch_cooldown_until = $4 WHERE key_id = $1",
    )
    .bind(&holder)
    .bind(new_cap.as_str())
    .bind(now)
    .bind(now + Duration::days(cfg.vouch_cooldown_days))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn insert_attestation(
    pool: &PgPool,
    a: &NewAttestation<'_>,
    cfg: &TrustConfig,
) -> Result<Outcome> {
    let mut tx = pool.begin().await?;
    lock_trust(&mut tx).await?;
    let fresh: Option<String> = sqlx::query_scalar(
        "INSERT INTO private.attestations (id, type, issuer_key_id, subject_key_id, raw)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO NOTHING RETURNING id",
    )
    .bind(a.id)
    .bind(a.kind)
    .bind(a.issuer_key_id)
    .bind(a.subject_key_id)
    .bind(a.raw)
    .fetch_optional(&mut *tx)
    .await?;
    if fresh.is_none() {
        tx.rollback().await?;
        return Ok(Outcome::Duplicate(None));
    }
    match a.kind {
        "vouch" => {}
        "vendor_of" => {
            let mut public = a.raw.clone();
            if let Some(o) = public.as_object_mut() {
                o.remove("signature");
                if let Some(e) = a.expires_at {
                    o.insert(
                        "expires_at".into(),
                        json!(e.to_rfc3339_opts(SecondsFormat::Secs, true)),
                    );
                }
            }
            sqlx::query(
                "INSERT INTO public.vendor_bindings (id, place_id, subject_key_id, holder_key_id, doc, issued_at, expires_at)
                 VALUES ($1, $2, $3, $3, $4, $5, $6)",
            )
            .bind(a.id)
            .bind(a.place_id)
            .bind(a.subject_key_id)
            .bind(&public)
            .bind(a.issued_at)
            .bind(a.expires_at)
            .execute(&mut *tx)
            .await
            .map_err(DbError::classify)?;
            add_change(&mut tx, "vendor_binding", a.id, "create").await?;
        }
        "revoke" => {
            let target = a.revokes.unwrap_or_default();
            let kind: Option<String> = sqlx::query_scalar(
                "UPDATE private.attestations SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL
                 RETURNING type",
            )
            .bind(target)
            .fetch_optional(&mut *tx)
            .await?;
            match kind.as_deref() {
                Some("vendor_of") => {
                    sqlx::query(
                        "UPDATE public.vendor_bindings SET revoked_at = now() WHERE id = $1",
                    )
                    .bind(target)
                    .execute(&mut *tx)
                    .await?;
                    add_change(&mut tx, "vendor_binding", target, "revoke").await?;
                }
                Some("vouch") => {
                    if let Some(k) = a.strike_against {
                        strike(&mut tx, k, target, a.id, cfg).await?;
                    }
                }
                Some(other) => {
                    return Err(DbError::Conflict(format!(
                        "a {other} attestation cannot be revoked"
                    )));
                }
                None => return Err(DbError::Conflict(format!("{target} is already revoked"))),
            }
        }
        "rotate" => {
            let n = sqlx::query(
                "UPDATE private.keys SET status = 'rotated', successor_key_id = $2
                  WHERE key_id = $1 AND status = 'active'",
            )
            .bind(a.issuer_key_id)
            .bind(a.subject_key_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if n == 0 {
                return Err(DbError::Conflict("issuer key is not active".into()));
            }
            // The new key carries the old key's strike state (no laundering).
            sqlx::query(
                "UPDATE private.keys n SET level_cap = o.level_cap, demoted_at = o.demoted_at,
                   vouch_cooldown_until = o.vouch_cooldown_until
                  FROM private.keys o WHERE o.key_id = $1 AND n.key_id = $2",
            )
            .bind(a.issuer_key_id)
            .bind(a.subject_key_id)
            .execute(&mut *tx)
            .await?;
            let moved: Vec<String> = sqlx::query_scalar(
                "UPDATE public.vendor_bindings SET holder_key_id = $2
                  WHERE holder_key_id = $1 AND revoked_at IS NULL
                    AND (expires_at IS NULL OR expires_at > now()) RETURNING id",
            )
            .bind(a.issuer_key_id)
            .bind(a.subject_key_id)
            .fetch_all(&mut *tx)
            .await?;
            for id in &moved {
                add_change(&mut tx, "vendor_binding", id, "transfer").await?;
            }
        }
        other => {
            return Err(DbError::BadReference(format!(
                "attestation type {other} not supported"
            )));
        }
    }
    recompute_trust(&mut tx, cfg.max_depth).await?;
    tx.commit().await?;
    Ok(Outcome::Accepted)
}
