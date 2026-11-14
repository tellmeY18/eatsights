//! Queries used by the background worker (aggregation, menu documents).

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{Result, add_change};

#[derive(Debug, Clone, PartialEq)]
pub struct ChangeRow {
    pub seq: i64,
    pub entity_type: String,
    pub entity_id: String,
    pub op: String,
}

pub async fn cursor(pool: &PgPool, name: &str) -> Result<i64> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT seq FROM private.worker_cursors WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await?
            .unwrap_or(0),
    )
}

pub async fn set_cursor(pool: &PgPool, name: &str, seq: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO private.worker_cursors (name, seq, updated_at) VALUES ($1, $2, now())
         ON CONFLICT (name) DO UPDATE SET seq = GREATEST(worker_cursors.seq, EXCLUDED.seq), updated_at = now()",
    )
    .bind(name)
    .bind(seq)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn changes_after(pool: &PgPool, after: i64, limit: i64) -> Result<Vec<ChangeRow>> {
    let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT seq, entity_type, entity_id, op FROM public.changes WHERE seq > $1 ORDER BY seq LIMIT $2",
    )
    .bind(after)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(seq, entity_type, entity_id, op)| ChangeRow {
            seq,
            entity_type,
            entity_id,
            op,
        })
        .collect())
}

/// (subject_key, property, place_id) of the given observation ids.
pub async fn pairs_for_observations(
    pool: &PgPool,
    ids: &[String],
) -> Result<Vec<(String, String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT DISTINCT subject_key, property, place_id FROM public.observation_ids WHERE id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?)
}

/// Place of a section or menu item.
pub async fn place_of(pool: &PgPool, entity_type: &str, id: &str) -> Result<Option<String>> {
    let sql = match entity_type {
        "section" => "SELECT place_id FROM public.sections WHERE id = $1",
        "menu_item" => "SELECT place_id FROM public.menu_items WHERE id = $1",
        "place" => "SELECT id FROM public.places WHERE id = $1",
        _ => return Ok(None),
    };
    Ok(sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

/// Where a subject's facts live: follows `merged_into` (one hop per call, up to 8).
pub async fn resolve_subject(pool: &PgPool, subject_key: &str) -> Result<String> {
    let mut key = subject_key.to_owned();
    for _ in 0..8 {
        let Some((scope, id)) = key.split_once(':') else {
            break;
        };
        let sql = match scope {
            "place" => "SELECT merged_into FROM public.places WHERE id = $1",
            "menu_item" => "SELECT merged_into FROM public.menu_items WHERE id = $1",
            _ => break,
        };
        let next: Option<Option<String>> = sqlx::query_scalar(sql)
            .bind(id)
            .fetch_optional(pool)
            .await?;
        match next.flatten() {
            Some(n) => key = format!("{scope}:{n}"),
            None => break,
        }
    }
    Ok(key)
}

/// Subject keys whose observations count for `subject_key` (itself plus merged-in records).
pub async fn merged_sources(pool: &PgPool, subject_key: &str) -> Result<Vec<String>> {
    let mut out = vec![subject_key.to_owned()];
    if let Some((scope, id)) = subject_key.split_once(':') {
        let sql = match scope {
            "place" => Some("SELECT id FROM public.places WHERE merged_into = $1"),
            "menu_item" => Some("SELECT id FROM public.menu_items WHERE merged_into = $1"),
            _ => None,
        };
        if let Some(sql) = sql {
            let ids: Vec<String> = sqlx::query_scalar(sql).bind(id).fetch_all(pool).await?;
            out.extend(ids.into_iter().map(|i| format!("{scope}:{i}")));
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObsRow {
    pub doc: Value,
    pub class: String,
    pub pseudonym: String,
    pub observed_at: DateTime<Utc>,
}

pub async fn observations_for(
    pool: &PgPool,
    subject_keys: &[String],
    property: &str,
) -> Result<Vec<ObsRow>> {
    let rows: Vec<(Value, String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT doc, class, pseudonym, observed_at FROM public.observations
         WHERE subject_key = ANY($1) AND property = $2 AND NOT (doc ? 'redacted')",
    )
    .bind(subject_keys)
    .bind(property)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(doc, class, pseudonym, observed_at)| ObsRow {
            doc,
            class,
            pseudonym,
            observed_at,
        })
        .collect())
}

/// Upsert a fact; returns true when its content (ignoring `computed_at`) changed.
pub async fn upsert_fact(
    pool: &PgPool,
    subject_key: &str,
    property: &str,
    ids: (Option<&str>, Option<&str>, Option<&str>),
    doc: &Value,
    now: DateTime<Utc>,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let n = sqlx::query(
        "INSERT INTO public.facts (subject_key, property, place_id, menu_item_id, dish_id, doc, computed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (subject_key, property) DO UPDATE SET doc = EXCLUDED.doc, computed_at = EXCLUDED.computed_at,
           place_id = EXCLUDED.place_id, menu_item_id = EXCLUDED.menu_item_id, dish_id = EXCLUDED.dish_id
         WHERE (facts.doc - 'computed_at') IS DISTINCT FROM (EXCLUDED.doc - 'computed_at')",
    )
    .bind(subject_key)
    .bind(property)
    .bind(ids.0)
    .bind(ids.1)
    .bind(ids.2)
    .bind(doc)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n > 0 {
        add_change(
            &mut tx,
            "fact",
            &format!("{subject_key}|{property}"),
            "upsert",
        )
        .await?;
    }
    tx.commit().await?;
    Ok(n > 0)
}

/// Store a menu document; returns true when it changed (ignoring `built_at`).
pub async fn store_menu_doc(
    pool: &PgPool,
    place_id: &str,
    doc: &Value,
    now: DateTime<Utc>,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let n = sqlx::query(
        "INSERT INTO public.menu_docs (place_id, doc, built_at) VALUES ($1, $2, $3)
         ON CONFLICT (place_id) DO UPDATE SET doc = EXCLUDED.doc, built_at = EXCLUDED.built_at
         WHERE (menu_docs.doc - 'built_at') IS DISTINCT FROM (EXCLUDED.doc - 'built_at')",
    )
    .bind(place_id)
    .bind(doc)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n > 0 {
        add_change(&mut tx, "menu", place_id, "rebuild").await?;
    }
    tx.commit().await?;
    Ok(n > 0)
}
