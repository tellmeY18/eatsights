//! Signed admin actions (AGENTS.md 6.3): OSM links, merges, suggestion decisions and
//! legal/safety removals. Every action is one transaction that also stores the full signed
//! object in `private.admin_actions` (the replay guard) and appends to `public.changes`.

use chrono::{DateTime, SecondsFormat, Utc};
use eatsights_core::schema;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{DbError, Result, add_change};

/// The verified signed admin object being applied.
#[derive(Debug, Clone, Copy)]
pub struct AdminRecord<'a> {
    pub id: &'a str,
    pub action: &'a str,
    pub issuer_key_id: &'a str,
    pub raw: &'a Value,
    pub issued_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AdminOutcome {
    Done(Value),
    /// The same id was applied before; carries the stored raw object and result.
    Duplicate {
        raw: Value,
        result: Value,
    },
}

pub async fn admin_action(pool: &PgPool, id: &str) -> Result<Option<(Value, Value)>> {
    Ok(
        sqlx::query_as("SELECT raw, result FROM private.admin_actions WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// Store the action and commit, or roll back when the id was taken concurrently.
async fn finish(
    mut tx: Transaction<'_, Postgres>,
    pool: &PgPool,
    rec: &AdminRecord<'_>,
    result: Value,
) -> Result<AdminOutcome> {
    let fresh: Option<String> = sqlx::query_scalar(
        "INSERT INTO private.admin_actions (id, action, issuer_key_id, raw, result, issued_at)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING RETURNING id",
    )
    .bind(rec.id)
    .bind(rec.action)
    .bind(rec.issuer_key_id)
    .bind(rec.raw)
    .bind(&result)
    .bind(rec.issued_at)
    .fetch_optional(&mut *tx)
    .await?;
    if fresh.is_none() {
        tx.rollback().await?;
        return match admin_action(pool, rec.id).await? {
            Some((raw, result)) => Ok(AdminOutcome::Duplicate { raw, result }),
            None => Err(DbError::Conflict(format!("{} is being applied", rec.id))),
        };
    }
    tx.commit().await?;
    Ok(AdminOutcome::Done(result))
}

fn table_of(entity_type: &str) -> Result<&'static str> {
    match entity_type {
        "place" => Ok("places"),
        "menu_item" => Ok("menu_items"),
        other => Err(DbError::BadReference(format!(
            "entity_type {other} cannot be merged"
        ))),
    }
}

fn invalid(def: &str, errs: Vec<String>) -> DbError {
    DbError::BadReference(format!("resulting {def} is invalid: {}", errs.join("; ")))
}

// ------------------------------------------------------------------ OSM link

/// A person confirms an OSM object for a place: `state: linked` with `id` and `verified_at`.
pub async fn link_osm(
    pool: &PgPool,
    rec: &AdminRecord<'_>,
    place_id: &str,
    osm_id: &str,
    now: DateTime<Utc>,
) -> Result<AdminOutcome> {
    let mut tx = pool.begin().await?;
    let doc: Option<Value> =
        sqlx::query_scalar("SELECT doc FROM public.places WHERE id = $1 FOR UPDATE")
            .bind(place_id)
            .fetch_optional(&mut *tx)
            .await?;
    let mut doc = doc.ok_or_else(|| DbError::NotFound(place_id.to_owned()))?;
    let mut osm = json!({
        "state": "linked",
        "id": osm_id,
        "verified_at": now.format("%Y-%m-%d").to_string(),
    });
    if let Some(c) = doc.pointer("/osm/checked_at") {
        osm["checked_at"] = c.clone();
    }
    schema::validate_def("osm_link", &osm).map_err(|e| invalid("osm_link", e))?;
    doc["osm"] = osm.clone();
    schema::validate_def("place", &doc).map_err(|e| invalid("place", e))?;
    sqlx::query("UPDATE public.places SET doc = $2, updated_at = $3 WHERE id = $1")
        .bind(place_id)
        .bind(&doc)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    add_change(&mut tx, "place", place_id, "osm").await?;
    finish(tx, pool, rec, json!({ "place_id": place_id, "osm": osm })).await
}

// ------------------------------------------------------------------ merges

#[derive(Debug, Clone)]
struct MergeRow {
    id: String,
    doc: Value,
    merged_into: Option<String>,
    created_at: DateTime<Utc>,
    place_id: Option<String>,
}

async fn lock_row(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    id: &str,
) -> Result<Option<MergeRow>> {
    let place_col = if table == "menu_items" {
        "place_id"
    } else {
        "id"
    };
    let row = sqlx::query(&format!(
        "SELECT id, doc, merged_into, created_at, {place_col} AS place_id
         FROM public.{table} WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match row {
        Some(r) => Some(MergeRow {
            id: r.try_get("id")?,
            doc: r.try_get("doc")?,
            merged_into: r.try_get("merged_into")?,
            created_at: r.try_get("created_at")?,
            place_id: r.try_get("place_id")?,
        }),
        None => None,
    })
}

fn is_linked(r: &MergeRow) -> bool {
    r.doc.pointer("/osm/state").and_then(Value::as_str) == Some("linked")
}

/// SCHEMA.md: the survivor is the record with an OSM link, otherwise the older one
/// (ties: the smaller id).
fn default_survivor<'a>(a: &'a MergeRow, b: &'a MergeRow) -> &'a MergeRow {
    match (is_linked(a), is_linked(b)) {
        (true, false) => a,
        (false, true) => b,
        _ => {
            if (b.created_at, &b.id) < (a.created_at, &a.id) {
                b
            } else {
                a
            }
        }
    }
}

async fn set_merged(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    def: &str,
    row: &MergeRow,
    into: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    let mut doc = row.doc.clone();
    if let Some(o) = doc.as_object_mut() {
        match into {
            Some(s) => {
                o.insert("record_state".into(), json!("merged"));
                o.insert("merged_into".into(), json!(s));
            }
            None => {
                o.insert("record_state".into(), json!("active"));
                o.remove("merged_into");
            }
        }
    }
    schema::validate_def(def, &doc).map_err(|e| invalid(def, e))?;
    sqlx::query(&format!(
        "UPDATE public.{table} SET doc = $2, record_state = $3, merged_into = $4, updated_at = $5 WHERE id = $1"
    ))
    .bind(&row.id)
    .bind(&doc)
    .bind(if into.is_some() { "merged" } else { "active" })
    .bind(into)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(DbError::classify)?;
    Ok(())
}

/// Requested merge: two records, an optional explicit survivor and an optional suggestion.
#[derive(Debug, Clone, Copy)]
pub struct MergeRequest<'a> {
    pub entity_type: &'a str,
    pub a_id: &'a str,
    pub b_id: &'a str,
    pub survivor_id: Option<&'a str>,
    pub suggestion_id: Option<i64>,
}

/// Confirm a merge. The loser becomes `merged` with `merged_into` pointing at the survivor's
/// final survivor (chains are flattened, cycles rejected). Records that pointed at the loser
/// are re-pointed. Nothing is deleted.
pub async fn merge(
    pool: &PgPool,
    rec: &AdminRecord<'_>,
    m: &MergeRequest<'_>,
    now: DateTime<Utc>,
) -> Result<AdminOutcome> {
    let table = table_of(m.entity_type)?;
    if m.a_id == m.b_id {
        return Err(DbError::Conflict(
            "cannot merge a record into itself".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    // Lock in id order to avoid deadlocks between concurrent merges.
    let (first, second) = if m.a_id < m.b_id {
        (m.a_id, m.b_id)
    } else {
        (m.b_id, m.a_id)
    };
    let r1 = lock_row(&mut tx, table, first)
        .await?
        .ok_or_else(|| DbError::NotFound(first.to_owned()))?;
    let r2 = lock_row(&mut tx, table, second)
        .await?
        .ok_or_else(|| DbError::NotFound(second.to_owned()))?;
    let survivor = match m.survivor_id {
        Some(s) if s == r1.id => &r1,
        Some(s) if s == r2.id => &r2,
        Some(s) => {
            return Err(DbError::BadReference(format!(
                "survivor {s} is not one of the merged records"
            )));
        }
        None => default_survivor(&r1, &r2),
    };
    let loser = if survivor.id == r1.id { &r2 } else { &r1 };
    if loser.merged_into.is_some() {
        return Err(DbError::Conflict(format!(
            "{} is already merged; undo that merge first",
            loser.id
        )));
    }
    // Follow the survivor's redirects to its final survivor.
    let mut final_id = survivor.id.clone();
    let mut next = survivor.merged_into.clone();
    let mut hops = 0;
    while let Some(n) = next {
        if n == loser.id {
            return Err(DbError::Conflict(format!(
                "merging {} into {} would create a cycle",
                loser.id, survivor.id
            )));
        }
        hops += 1;
        if hops > 32 {
            return Err(DbError::Conflict("redirect chain too long".into()));
        }
        next = sqlx::query_scalar::<_, Option<String>>(&format!(
            "SELECT merged_into FROM public.{table} WHERE id = $1"
        ))
        .bind(&n)
        .fetch_one(&mut *tx)
        .await?;
        final_id = n;
    }
    if final_id == loser.id {
        return Err(DbError::Conflict("merge would create a cycle".into()));
    }
    if m.entity_type == "menu_item" {
        let final_place: Option<String> =
            sqlx::query_scalar("SELECT place_id FROM public.menu_items WHERE id = $1")
                .bind(&final_id)
                .fetch_optional(&mut *tx)
                .await?;
        if final_place != loser.place_id {
            return Err(DbError::Conflict(
                "menu items can only be merged within one place".into(),
            ));
        }
    }
    set_merged(&mut tx, table, m.entity_type, loser, Some(&final_id), now).await?;
    // Flatten: whatever pointed at the loser now points at the final survivor.
    let pointing: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT id FROM public.{table} WHERE merged_into = $1 ORDER BY id"
    ))
    .bind(&loser.id)
    .fetch_all(&mut *tx)
    .await?;
    for id in &pointing {
        if let Some(row) = lock_row(&mut tx, table, id).await? {
            set_merged(&mut tx, table, m.entity_type, &row, Some(&final_id), now).await?;
            add_change(&mut tx, m.entity_type, id, "merge").await?;
        }
    }
    add_change(&mut tx, m.entity_type, &loser.id, "merge").await?;
    add_change(&mut tx, m.entity_type, &final_id, "merge").await?;
    let (lo, hi) = if m.a_id < m.b_id {
        (m.a_id, m.b_id)
    } else {
        (m.b_id, m.a_id)
    };
    sqlx::query(
        "UPDATE private.merge_suggestions SET status = 'confirmed', decided_at = $5, decided_by = $6
         WHERE status = 'open' AND (id = $4 OR (entity_type = $1 AND a_id = $2 AND b_id = $3))",
    )
    .bind(m.entity_type)
    .bind(lo)
    .bind(hi)
    .bind(m.suggestion_id)
    .bind(now)
    .bind(rec.id)
    .execute(&mut *tx)
    .await?;
    let result = json!({
        "entity_type": m.entity_type,
        "merged": loser.id,
        "merged_into": final_id,
        "repointed": pointing,
    });
    finish(tx, pool, rec, result).await
}

/// Undo a merge: clear `merged_into` and make the record active again.
pub async fn unmerge(
    pool: &PgPool,
    rec: &AdminRecord<'_>,
    entity_type: &str,
    id: &str,
    now: DateTime<Utc>,
) -> Result<AdminOutcome> {
    let table = table_of(entity_type)?;
    let mut tx = pool.begin().await?;
    let row = lock_row(&mut tx, table, id)
        .await?
        .ok_or_else(|| DbError::NotFound(id.to_owned()))?;
    let Some(former) = row.merged_into.clone() else {
        return Err(DbError::Conflict(format!("{id} is not merged")));
    };
    set_merged(&mut tx, table, entity_type, &row, None, now).await?;
    add_change(&mut tx, entity_type, id, "unmerge").await?;
    add_change(&mut tx, entity_type, &former, "unmerge").await?;
    let result = json!({ "entity_type": entity_type, "unmerged": id, "was_merged_into": former });
    finish(tx, pool, rec, result).await
}

/// Dismiss an open duplicate suggestion.
pub async fn dismiss_suggestion(
    pool: &PgPool,
    rec: &AdminRecord<'_>,
    suggestion_id: i64,
    now: DateTime<Utc>,
) -> Result<AdminOutcome> {
    let mut tx = pool.begin().await?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM private.merge_suggestions WHERE id = $1 FOR UPDATE")
            .bind(suggestion_id)
            .fetch_optional(&mut *tx)
            .await?;
    match status.as_deref() {
        None => return Err(DbError::NotFound(format!("suggestion {suggestion_id}"))),
        Some("open") => {}
        Some(s) => {
            return Err(DbError::Conflict(format!(
                "suggestion {suggestion_id} is {s}"
            )));
        }
    }
    sqlx::query(
        "UPDATE private.merge_suggestions SET status = 'dismissed', decided_at = $2, decided_by = $3 WHERE id = $1",
    )
    .bind(suggestion_id)
    .bind(now)
    .bind(rec.id)
    .execute(&mut *tx)
    .await?;
    let result = json!({ "suggestion_id": suggestion_id, "status": "dismissed" });
    finish(tx, pool, rec, result).await
}

// ------------------------------------------------------------------ removals

/// Legal or safety removal through the single audited `redact_observation` function.
/// The public log carries only the observation id and the reason code.
pub async fn remove_observation(
    pool: &PgPool,
    rec: &AdminRecord<'_>,
    observation_id: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<AdminOutcome> {
    let mut tx = pool.begin().await?;
    let redacted: Option<bool> = sqlx::query_scalar(
        "SELECT doc ? 'redacted' FROM public.observations WHERE id = $1 LIMIT 1",
    )
    .bind(observation_id)
    .fetch_optional(&mut *tx)
    .await?;
    match redacted {
        None => return Err(DbError::NotFound(observation_id.to_owned())),
        Some(true) => {
            return Err(DbError::Conflict(format!(
                "{observation_id} is already removed"
            )));
        }
        Some(false) => {}
    }
    let ok: bool = sqlx::query_scalar("SELECT public.redact_observation($1, $2)")
        .bind(observation_id)
        .bind(reason)
        .fetch_one(&mut *tx)
        .await?;
    if !ok {
        return Err(DbError::NotFound(observation_id.to_owned()));
    }
    let result = json!({
        "observation_id": observation_id,
        "reason": reason,
        "removed_at": now.to_rfc3339_opts(SecondsFormat::Secs, true),
    });
    finish(tx, pool, rec, result).await
}

// ------------------------------------------------------------------ worker helpers

/// Properties with observations or facts on any of the subject keys.
pub async fn subject_properties(pool: &PgPool, subject_keys: &[String]) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT property FROM public.observation_ids WHERE subject_key = ANY($1)
         UNION SELECT property FROM public.facts WHERE subject_key = ANY($1)
         ORDER BY 1",
    )
    .bind(subject_keys)
    .fetch_all(pool)
    .await?)
}

/// Delete a derived fact (facts are recomputable; the deletion is in the change feed).
pub async fn delete_fact(pool: &PgPool, subject_key: &str, property: &str) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let n = sqlx::query("DELETE FROM public.facts WHERE subject_key = $1 AND property = $2")
        .bind(subject_key)
        .bind(property)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n > 0 {
        add_change(
            &mut tx,
            "fact",
            &format!("{subject_key}|{property}"),
            "delete",
        )
        .await?;
    }
    tx.commit().await?;
    Ok(n > 0)
}
