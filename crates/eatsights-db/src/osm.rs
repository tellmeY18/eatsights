//! Queries for the OSM checker, `ready_to_map` and the duplicate detector.
//!
//! Only our own data is read or written here. The OSM checker stores `state`,
//! `checked_at`, candidate ids, distances and name scores, never OSM tags, names or coordinates.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{Result, add_change};

/// A place to check against OSM: our id, pin and names (all values of `names` and `aliases`).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceToCheck {
    pub id: String,
    pub lat: f64,
    pub lng: f64,
    pub names: Vec<String>,
    pub osm: Value,
}

fn names_of(doc: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = doc.get("names").and_then(Value::as_object) {
        out.extend(m.values().filter_map(Value::as_str).map(str::to_owned));
    }
    if let Some(a) = doc.get("aliases").and_then(Value::as_array) {
        for m in a.iter().filter_map(Value::as_object) {
            out.extend(m.values().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    out
}

/// Active places that are `unchecked`, or `unmapped` with `checked_at` before `recheck_before`.
pub async fn places_due(
    pool: &PgPool,
    recheck_before: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<PlaceToCheck>> {
    let rows: Vec<(String, f64, f64, Value)> = sqlx::query_as(
        "SELECT id, lat, lng, doc FROM public.places
         WHERE record_state = 'active'
           AND (doc #>> '{osm,state}' = 'unchecked'
             OR (doc #>> '{osm,state}' = 'unmapped' AND (doc #>> '{osm,checked_at}')::timestamptz < $1))
         ORDER BY created_at, id LIMIT $2",
    )
    .bind(recheck_before)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, lat, lng, doc)| PlaceToCheck {
            id,
            lat,
            lng,
            names: names_of(&doc),
            osm: doc.get("osm").cloned().unwrap_or(Value::Null),
        })
        .collect())
}

/// Replace a place's `osm` object if it still equals `expected` (so a concurrent admin
/// link is never overwritten). Appends a change. Returns true when written.
pub async fn set_osm(pool: &PgPool, place_id: &str, expected: &Value, osm: &Value) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let n = sqlx::query(
        "UPDATE public.places SET doc = jsonb_set(doc, '{osm}', $3), updated_at = now()
         WHERE id = $1 AND doc -> 'osm' = $2 AND doc -> 'osm' IS DISTINCT FROM $3",
    )
    .bind(place_id)
    .bind(expected)
    .bind(osm)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n > 0 {
        add_change(&mut tx, "place", place_id, "osm").await?;
    }
    tx.commit().await?;
    Ok(n > 0)
}

/// What `ready_to_map` depends on: the place document, its record state and its
/// `status.operating` fact (if any).
#[derive(Debug, Clone, PartialEq)]
pub struct ReadyInputs {
    pub doc: Value,
    pub record_state: String,
    pub operating_fact: Option<Value>,
}

pub async fn ready_inputs(pool: &PgPool, place_id: &str) -> Result<Option<ReadyInputs>> {
    let row: Option<(Value, String, Option<Value>)> = sqlx::query_as(
        "SELECT p.doc, p.record_state, f.doc FROM public.places p
         LEFT JOIN public.facts f ON f.subject_key = 'place:' || p.id AND f.property = 'status.operating'
         WHERE p.id = $1",
    )
    .bind(place_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(doc, record_state, operating_fact)| ReadyInputs {
        doc,
        record_state,
        operating_fact,
    }))
}

/// Set `osm.ready_to_map` (only when it changes). Appends a change.
pub async fn set_ready_to_map(pool: &PgPool, place_id: &str, ready: bool) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let n = sqlx::query(
        "UPDATE public.places SET doc = jsonb_set(doc, '{osm,ready_to_map}', to_jsonb($2::boolean)),
           updated_at = now()
         WHERE id = $1 AND (doc #> '{osm,ready_to_map}') IS DISTINCT FROM to_jsonb($2::boolean)",
    )
    .bind(place_id)
    .bind(ready)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n > 0 {
        add_change(&mut tx, "place", place_id, "ready_to_map").await?;
    }
    tx.commit().await?;
    Ok(n > 0)
}

/// Places currently flagged `ready_to_map: true` (re-checked periodically for expiry).
pub async fn ready_places(pool: &PgPool) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM public.places WHERE doc #> '{osm,ready_to_map}' = 'true'::jsonb",
    )
    .fetch_all(pool)
    .await?)
}

// ------------------------------------------------------------------ duplicates

/// One active place as seen by the duplicate detector.
#[derive(Debug, Clone, PartialEq)]
pub struct DedupePlace {
    pub id: String,
    pub category: String,
    pub lat: f64,
    pub lng: f64,
}

pub async fn active_place(pool: &PgPool, id: &str) -> Result<Option<DedupePlace>> {
    let row: Option<(String, String, f64, f64)> = sqlx::query_as(
        "SELECT id, category, lat, lng FROM public.places WHERE id = $1 AND record_state = 'active'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id, category, lat, lng)| DedupePlace {
        id,
        category,
        lat,
        lng,
    }))
}

/// A nearby place of the same category: distance in metres and the best trigram
/// similarity between any pair of `names` values (after `unaccent` + lower-case).
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    pub id: String,
    pub distance_m: f64,
    pub similarity: f64,
}

/// Same-category active places in the given geohash cells within `max_m` metres.
/// One index range scan per cell (`COLLATE "C"` btree, LATERAL + OFFSET 0 fence; ADR-0001).
pub async fn neighbours(
    pool: &PgPool,
    p: &DedupePlace,
    cells: &[String],
    max_m: f64,
) -> Result<Vec<Neighbour>> {
    let rows: Vec<(String, f64, f64)> = sqlx::query_as(
        "WITH me AS (SELECT doc FROM public.places WHERE id = $1),
         near AS (
           SELECT q.id, q.doc,
             2 * 6371008.8 * asin(sqrt(
               power(sin(radians(q.lat - $3) / 2), 2) +
               cos(radians($3)) * cos(radians(q.lat)) * power(sin(radians(q.lng - $4) / 2), 2))) AS d
           FROM unnest($5::text[]) AS c(cell)
           CROSS JOIN LATERAL (
             SELECT id, doc, lat, lng FROM public.places
             WHERE geohash >= c.cell AND geohash < c.cell || '~'
               AND record_state = 'active' AND category = $2 AND id <> $1
             OFFSET 0) q
         )
         SELECT near.id, near.d,
           (SELECT coalesce(max(similarity(public.es_fold(a.value), public.es_fold(b.value))), 0)::float8
              FROM me, jsonb_each_text(me.doc -> 'names') a, jsonb_each_text(near.doc -> 'names') b)
         FROM near WHERE near.d <= $6",
    )
    .bind(&p.id)
    .bind(&p.category)
    .bind(p.lat)
    .bind(p.lng)
    .bind(cells)
    .bind(max_m)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, distance_m, similarity)| Neighbour {
            id,
            distance_m,
            similarity,
        })
        .collect())
}

/// Record a suggestion for the unordered pair. Decided suggestions are left alone.
pub async fn upsert_suggestion(
    pool: &PgPool,
    entity_type: &str,
    a: &str,
    b: &str,
    score: f64,
    distance_m: f64,
) -> Result<()> {
    let (a, b) = if a < b { (a, b) } else { (b, a) };
    sqlx::query(
        "INSERT INTO private.merge_suggestions (entity_type, a_id, b_id, score, distance_m)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (entity_type, a_id, b_id) DO UPDATE SET score = EXCLUDED.score, distance_m = EXCLUDED.distance_m
         WHERE merge_suggestions.status = 'open'",
    )
    .bind(entity_type)
    .bind(a)
    .bind(b)
    .bind(score)
    .bind(distance_m)
    .execute(pool)
    .await?;
    Ok(())
}

/// One stored suggestion (private; for the maintainer's CLI and admin actions only).
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub id: i64,
    pub entity_type: String,
    pub a_id: String,
    pub b_id: String,
    pub score: f64,
    pub distance_m: Option<f64>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

type SuggestionRow = (
    i64,
    String,
    String,
    String,
    f64,
    Option<f64>,
    String,
    DateTime<Utc>,
);

fn suggestion(r: SuggestionRow) -> Suggestion {
    Suggestion {
        id: r.0,
        entity_type: r.1,
        a_id: r.2,
        b_id: r.3,
        score: r.4,
        distance_m: r.5,
        status: r.6,
        created_at: r.7,
    }
}

const SUGGESTION_COLS: &str = "id, entity_type, a_id, b_id, score, distance_m, status, created_at";

pub async fn list_suggestions(
    pool: &PgPool,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<Suggestion>> {
    let rows: Vec<SuggestionRow> = sqlx::query_as(&format!(
        "SELECT {SUGGESTION_COLS} FROM private.merge_suggestions
         WHERE ($1::text IS NULL OR status = $1) ORDER BY score DESC, id LIMIT $2"
    ))
    .bind(status)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(suggestion).collect())
}

pub async fn get_suggestion(pool: &PgPool, id: i64) -> Result<Option<Suggestion>> {
    let row: Option<SuggestionRow> = sqlx::query_as(&format!(
        "SELECT {SUGGESTION_COLS} FROM private.merge_suggestions WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(suggestion))
}
