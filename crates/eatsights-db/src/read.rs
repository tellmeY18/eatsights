//! Public read queries. Every function here touches the `public` schema only.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, QueryBuilder, Row};

use crate::Result;

/// One page of results; `next_cursor` is `None` on the last page.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
}

fn page(mut rows: Vec<(String, Value)>, limit: i64) -> Page {
    let more = rows.len() as i64 > limit;
    rows.truncate(limit.max(0) as usize);
    let next_cursor = if more {
        rows.last().map(|(c, _)| c.clone())
    } else {
        None
    };
    Page {
        items: rows.into_iter().map(|(_, v)| v).collect(),
        next_cursor,
    }
}

/// A circle, already quantised by the caller.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Near {
    pub lat: f64,
    pub lng: f64,
    pub radius_m: f64,
}

#[derive(Debug, Clone, Default)]
pub struct PlaceQuery {
    pub cell: Option<String>,
    pub near: Option<Near>,
    pub city: Option<String>,
    pub category: Option<String>,
    pub q: Option<String>,
    pub after: Option<String>,
    pub limit: i64,
}

const HAVERSINE: &str = "2 * 6371000 * asin(sqrt(power(sin(radians(lat - {lat}) / 2), 2) + cos(radians({lat})) * cos(radians(lat)) * power(sin(radians(lng - {lng}) / 2), 2)))";

pub async fn search_places(pool: &PgPool, q: &PlaceQuery) -> Result<Page> {
    // Geo filters per ADR-0001: C-collation range scans, one LATERAL index scan per cell.
    let mut b = match (&q.cell, q.near) {
        (_, Some(n)) => {
            let cells = eatsights_core::geo::cells_3x3(n.lat, n.lng, 5);
            let mut b = QueryBuilder::new("SELECT p.id, p.doc FROM unnest(");
            b.push_bind(cells).push(
                "::text[]) AS c CROSS JOIN LATERAL (SELECT * FROM public.places WHERE geohash >= c AND geohash < c || '~' OFFSET 0) p WHERE record_state = 'active'",
            );
            b
        }
        (Some(cell), None) => {
            let mut b = QueryBuilder::new(
                "SELECT id, doc FROM public.places WHERE record_state = 'active'",
            );
            b.push(" AND geohash >= ").push_bind(cell.clone());
            b.push(" AND geohash < ").push_bind(format!("{cell}~"));
            b
        }
        _ => QueryBuilder::new("SELECT id, doc FROM public.places WHERE record_state = 'active'"),
    };
    if let Some(n) = q.near {
        let expr = HAVERSINE
            .replace("{lat}", &format!("{:.7}::float8", n.lat))
            .replace("{lng}", &format!("{:.7}::float8", n.lng));
        b.push(format!(" AND {expr} <= ")).push_bind(n.radius_m);
    }
    if let Some(c) = &q.city {
        b.push(" AND city = ").push_bind(c.clone());
    }
    if let Some(c) = &q.category {
        b.push(" AND category = ").push_bind(c.clone());
    }
    if let Some(text) = &q.q {
        b.push(" AND (search_text % public.es_fold(")
            .push_bind(text.clone())
            .push(") OR strpos(search_text, public.es_fold(")
            .push_bind(text.clone())
            .push(")) > 0)");
    }
    if let Some(a) = &q.after {
        b.push(" AND id > ").push_bind(a.clone());
    }
    b.push(" ORDER BY id LIMIT ").push_bind(q.limit + 1);
    let rows = b.build().fetch_all(pool).await?;
    let rows = rows
        .into_iter()
        .map(|r| Ok((r.try_get::<String, _>("id")?, r.try_get::<Value, _>("doc")?)))
        .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;
    Ok(page(rows, q.limit))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaceRow {
    pub doc: Value,
    pub record_state: String,
    pub merged_into: Option<String>,
}

pub async fn get_place(pool: &PgPool, id: &str) -> Result<Option<PlaceRow>> {
    let row = sqlx::query("SELECT doc, record_state, merged_into FROM public.places WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(match row {
        Some(r) => Some(PlaceRow {
            doc: r.try_get("doc")?,
            record_state: r.try_get("record_state")?,
            merged_into: r.try_get("merged_into")?,
        }),
        None => None,
    })
}

/// The pre-built menu document stored by the worker.
pub async fn menu_doc(pool: &PgPool, place_id: &str) -> Result<Option<Value>> {
    Ok(
        sqlx::query_scalar("SELECT doc FROM public.menu_docs WHERE place_id = $1")
            .bind(place_id)
            .fetch_optional(pool)
            .await?,
    )
}

/// Build a place's menu document from the tables (4 queries, no N+1).
pub async fn build_menu_doc(
    pool: &PgPool,
    place_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<Value>> {
    let Some(place) = get_place(pool, place_id).await? else {
        return Ok(None);
    };
    let sections: Vec<Value> = sqlx::query_scalar(
        "SELECT doc FROM public.sections WHERE place_id = $1 ORDER BY sort NULLS LAST, id",
    )
    .bind(place_id)
    .fetch_all(pool)
    .await?;
    let items: Vec<(String, Value)> = sqlx::query_as(
        "SELECT id, doc FROM public.menu_items WHERE place_id = $1 AND record_state = 'active' ORDER BY id",
    )
    .bind(place_id)
    .fetch_all(pool)
    .await?;
    let facts: Vec<(Option<String>, Value)> = sqlx::query_as(
        "SELECT menu_item_id, doc FROM public.facts WHERE place_id = $1 ORDER BY subject_key, property",
    )
    .bind(place_id)
    .fetch_all(pool)
    .await?;
    let mut place_facts = Vec::new();
    let mut item_facts: std::collections::HashMap<String, Vec<Value>> = Default::default();
    for (mi, f) in facts {
        match mi {
            Some(mi) => item_facts.entry(mi).or_default().push(f),
            None => place_facts.push(f),
        }
    }
    let items: Vec<Value> = items
        .into_iter()
        .map(
            |(id, doc)| json!({ "item": doc, "facts": item_facts.remove(&id).unwrap_or_default() }),
        )
        .collect();
    Ok(Some(json!({
        "place": place.doc,
        "place_facts": place_facts,
        "sections": sections,
        "items": items,
        "built_at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })))
}

pub async fn get_doc(pool: &PgPool, table: EntityTable, id: &str) -> Result<Option<Value>> {
    let sql = format!("SELECT doc FROM public.{} WHERE id = $1", table.name());
    Ok(sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityTable {
    Places,
    Sections,
    MenuItems,
    Dishes,
}

impl EntityTable {
    pub fn name(self) -> &'static str {
        match self {
            Self::Places => "places",
            Self::Sections => "sections",
            Self::MenuItems => "menu_items",
            Self::Dishes => "dishes",
        }
    }

    pub fn from_entity_type(t: &str) -> Option<Self> {
        match t {
            "place" => Some(Self::Places),
            "section" => Some(Self::Sections),
            "menu_item" => Some(Self::MenuItems),
            "dish" => Some(Self::Dishes),
            _ => None,
        }
    }
}

pub async fn list_dishes(
    pool: &PgPool,
    q: Option<&str>,
    after: Option<&str>,
    limit: i64,
) -> Result<Page> {
    let mut b = QueryBuilder::new("SELECT id, doc FROM public.dishes WHERE true");
    if let Some(text) = q {
        b.push(" AND (search_text % public.es_fold(")
            .push_bind(text.to_owned())
            .push(") OR strpos(search_text, public.es_fold(")
            .push_bind(text.to_owned())
            .push(")) > 0)");
    }
    if let Some(a) = after {
        b.push(" AND id > ").push_bind(a.to_owned());
    }
    b.push(" ORDER BY id LIMIT ").push_bind(limit + 1);
    let rows: Vec<(String, Value)> = b.build_query_as().fetch_all(pool).await?;
    Ok(page(rows, limit))
}

#[derive(Debug, Clone, Default)]
pub struct FactQuery {
    pub place_id: Option<String>,
    pub menu_item_id: Option<String>,
    pub dish_id: Option<String>,
    pub property: Option<String>,
    pub after: Option<String>,
    pub limit: i64,
}

pub async fn list_facts(pool: &PgPool, q: &FactQuery) -> Result<Page> {
    let mut b = QueryBuilder::new(
        "SELECT subject_key || '|' || property AS cursor, doc FROM public.facts WHERE true",
    );
    if let Some(v) = &q.place_id {
        b.push(" AND place_id = ").push_bind(v.clone());
    }
    if let Some(v) = &q.menu_item_id {
        b.push(" AND menu_item_id = ").push_bind(v.clone());
    }
    if let Some(v) = &q.dish_id {
        b.push(" AND dish_id = ").push_bind(v.clone());
    }
    if let Some(v) = &q.property {
        b.push(" AND property = ").push_bind(v.clone());
    }
    if let Some((sk, p)) = q.after.as_deref().and_then(|a| a.split_once('|')) {
        b.push(" AND (subject_key, property) > (")
            .push_bind(sk.to_owned())
            .push(", ")
            .push_bind(p.to_owned())
            .push(")");
    }
    b.push(" ORDER BY subject_key, property LIMIT ")
        .push_bind(q.limit + 1);
    let rows: Vec<(String, Value)> = b.build_query_as().fetch_all(pool).await?;
    Ok(page(rows, q.limit))
}

#[derive(Debug, Clone, Default)]
pub struct ObservationQuery {
    pub place_id: Option<String>,
    pub menu_item_id: Option<String>,
    pub dish_id: Option<String>,
    pub property: Option<String>,
    pub after: Option<String>,
    pub limit: i64,
}

pub async fn list_observations(pool: &PgPool, q: &ObservationQuery) -> Result<Page> {
    let mut b = QueryBuilder::new("SELECT id, doc FROM public.observations WHERE true");
    if let Some(v) = &q.place_id {
        b.push(" AND place_id = ").push_bind(v.clone());
    }
    if let Some(v) = &q.menu_item_id {
        b.push(" AND menu_item_id = ").push_bind(v.clone());
    }
    if let Some(v) = &q.dish_id {
        b.push(" AND dish_id = ").push_bind(v.clone());
    }
    if let Some(v) = &q.property {
        b.push(" AND property = ").push_bind(v.clone());
    }
    if let Some(a) = &q.after {
        b.push(" AND id > ").push_bind(a.clone());
    }
    b.push(" ORDER BY id LIMIT ").push_bind(q.limit + 1);
    let rows: Vec<(String, Value)> = b.build_query_as().fetch_all(pool).await?;
    Ok(page(rows, q.limit))
}

pub async fn list_vendor_bindings(
    pool: &PgPool,
    place_id: Option<&str>,
    include_inactive: bool,
    after: Option<&str>,
    limit: i64,
) -> Result<Page> {
    let mut b = QueryBuilder::new(format!(
        "SELECT id, {VENDOR_BINDING_DOC} FROM public.vendor_bindings WHERE true"
    ));
    if !include_inactive {
        b.push(" AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())");
    }
    if let Some(p) = place_id {
        b.push(" AND place_id = ").push_bind(p.to_owned());
    }
    if let Some(a) = after {
        b.push(" AND id > ").push_bind(a.to_owned());
    }
    b.push(" ORDER BY id LIMIT ").push_bind(limit + 1);
    let rows: Vec<(String, Value)> = b.build_query_as().fetch_all(pool).await?;
    Ok(page(rows, limit))
}

pub async fn published_keys(pool: &PgPool) -> Result<Vec<Value>> {
    Ok(
        sqlx::query_scalar("SELECT doc FROM public.published_keys ORDER BY key_id")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn changes(pool: &PgPool, after: i64, limit: i64) -> Result<Vec<Value>> {
    let rows: Vec<(i64, String, String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT seq, entity_type, entity_id, op, at FROM public.changes WHERE seq > $1 ORDER BY seq LIMIT $2",
    )
    .bind(after)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(seq, t, id, op, at)| {
            json!({ "seq": seq, "entity_type": t, "entity_id": id, "op": op,
                    "at": at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true) })
        })
        .collect())
}

/// Public tables included in exports.
pub const EXPORT_TABLES: &[&str] = &[
    "places",
    "sections",
    "dishes",
    "menu_items",
    "facts",
    "observations",
    "vendor_bindings",
    "published_keys",
];

/// All `doc`s of one public table (whitelisted by [`EXPORT_TABLES`]).
pub async fn export_docs(pool: &PgPool, table: &str) -> Result<Vec<Value>> {
    if !EXPORT_TABLES.contains(&table) {
        return Ok(Vec::new());
    }
    let sql = if table == "vendor_bindings" {
        format!("SELECT {VENDOR_BINDING_DOC} FROM public.vendor_bindings ORDER BY id")
    } else {
        format!("SELECT doc FROM public.{table}")
    };
    Ok(sqlx::query_scalar(&sql).fetch_all(pool).await?)
}

pub async fn latest_seq(pool: &PgPool) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM public.changes")
            .fetch_one(pool)
            .await?,
    )
}

/// Public form of a vendor binding: the attestation without its signature, plus
/// `revoked`, `active` and, after a rotation, the key now holding the binding.
const VENDOR_BINDING_DOC: &str = "doc || jsonb_build_object('revoked', revoked_at IS NOT NULL,
  'active', revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now()))
  || CASE WHEN holder_key_id <> subject_key_id THEN jsonb_build_object('holder_key_id', holder_key_id) ELSE '{}'::jsonb END";
