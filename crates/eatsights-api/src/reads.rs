//! Public, cacheable read endpoints (AGENTS.md 6.1).

use std::future::Future;
use std::io::Write;
use std::sync::Arc;

use bytes::Bytes;
use eatsights_core::geo;
use eatsights_core::ids::is_valid_prefixed;
use eatsights_core::registry::Scope;
use eatsights_db::read::{self, EntityTable, FactQuery, Near, ObservationQuery, Page, PlaceQuery};
use poem::web::Data;
use poem::{IntoResponse, Request};
use poem_openapi::param::{Path, Query};
use poem_openapi::payload::Payload;
use poem_openapi::registry::MetaSchemaRef;
use poem_openapi::{ApiResponse, OpenApi, Tags};
use serde_json::{Value, json};

use crate::problem::ProblemResp;
use crate::state::{AppState, Cached, Metrics};

#[derive(Tags)]
pub enum ApiTags {
    /// Places, menus, dishes and facts.
    Data,
    /// Observations, attestations, keys and the change feed.
    Feed,
    /// The property registry.
    Registry,
    /// Signed writes.
    Write,
}

/// JSON bytes, rendered once and served from the in-process cache.
pub struct RawJson(pub Bytes);

impl Payload for RawJson {
    const CONTENT_TYPE: &'static str = "application/json; charset=utf-8";
    fn schema_ref() -> MetaSchemaRef {
        <Value as poem_openapi::types::Type>::schema_ref()
    }
}

impl IntoResponse for RawJson {
    fn into_response(self) -> poem::Response {
        poem::Response::builder()
            .content_type(Self::CONTENT_TYPE)
            .body(self.0)
    }
}

#[derive(ApiResponse)]
pub enum ReadResp {
    /// The document. Cacheable; weak `ETag` from the change sequence.
    #[oai(status = 200)]
    Ok(
        RawJson,
        #[oai(header = "ETag")] String,
        #[oai(header = "Cache-Control")] String,
        #[oai(header = "Vary")] String,
        #[oai(header = "Content-Encoding")] Option<String>,
    ),
    /// Unchanged since the given `If-None-Match`.
    #[oai(status = 304)]
    NotModified(
        #[oai(header = "ETag")] String,
        #[oai(header = "Cache-Control")] String,
    ),
    /// The record was merged; follow `Location` to the survivor.
    #[oai(status = 301)]
    Moved(#[oai(header = "Location")] String),
}

pub enum Fail {
    Resp(ReadResp),
    Problem(ProblemResp),
}

impl From<ProblemResp> for Fail {
    fn from(p: ProblemResp) -> Self {
        Fail::Problem(p)
    }
}

impl From<eatsights_db::DbError> for Fail {
    fn from(e: eatsights_db::DbError) -> Self {
        Fail::Problem(e.into())
    }
}

fn cache_control(max_age: u64) -> String {
    format!("public, max-age={max_age}, stale-while-revalidate={max_age}, stale-if-error=86400")
}

fn gzip(bytes: &[u8]) -> Bytes {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    if enc.write_all(bytes).is_err() {
        return Bytes::new();
    }
    enc.finish().map(Bytes::from).unwrap_or_default()
}

/// Serve a GET through the ETag check and the in-process cache.
pub async fn serve<F, Fut>(
    st: &AppState,
    req: &Request,
    max_age: u64,
    build: F,
) -> Result<ReadResp, ProblemResp>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value, Fail>>,
{
    let seq = st.current_seq();
    let etag = format!("W/\"{seq}\"");
    let cc = cache_control(max_age);
    if let Some(inm) = req
        .headers()
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        && inm.split(',').any(|t| t.trim() == etag || t.trim() == "*")
    {
        Metrics::inc(&st.metrics.not_modified);
        return Ok(ReadResp::NotModified(etag, cc));
    }
    let key = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    let entry = match st.cache.get(&key) {
        Some(c) if c.seq == seq => {
            Metrics::inc(&st.metrics.cache_hits);
            c
        }
        _ => {
            Metrics::inc(&st.metrics.cache_misses);
            let value = match build().await {
                Ok(v) => v,
                Err(Fail::Resp(r)) => return Ok(r),
                Err(Fail::Problem(p)) => return Err(p),
            };
            let body = serde_json::to_vec(&value).map_err(ProblemResp::internal)?;
            let c = Arc::new(Cached {
                seq,
                gzip: gzip(&body),
                body: Bytes::from(body),
            });
            st.cache.insert(key, c.clone());
            c
        }
    };
    let wants_gzip = req
        .headers()
        .get("accept-encoding")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("gzip"));
    let vary = "Accept-Encoding".to_owned();
    Ok(if wants_gzip && !entry.gzip.is_empty() {
        ReadResp::Ok(
            RawJson(entry.gzip.clone()),
            etag,
            cc,
            vary,
            Some("gzip".into()),
        )
    } else {
        ReadResp::Ok(RawJson(entry.body.clone()), etag, cc, vary, None)
    })
}

fn limit(l: Option<i64>) -> i64 {
    l.unwrap_or(50).clamp(1, 500)
}

fn page_json(p: Page) -> Value {
    json!({ "items": p.items, "next_cursor": p.next_cursor })
}

fn check_id(prefix: &str, id: &str, what: &str) -> Result<(), ProblemResp> {
    if is_valid_prefixed(prefix, id) {
        Ok(())
    } else {
        Err(ProblemResp::bad("invalid-id", format!("invalid {what} id")))
    }
}

fn opt_id(prefix: &str, id: &Option<String>, what: &str) -> Result<(), ProblemResp> {
    match id {
        Some(i) => check_id(prefix, i, what),
        None => Ok(()),
    }
}

const ALL_SCOPES: &[Scope] = &[Scope::Place, Scope::MenuItem, Scope::Dish];

pub struct ReadApi;

#[OpenApi(prefix_path = "/v1")]
impl ReadApi {
    /// Search places by geohash cell, a quantised circle, city, category or name.
    #[allow(clippy::too_many_arguments)]
    #[oai(
        path = "/places",
        method = "get",
        tag = "ApiTags::Data",
        operation_id = "places"
    )]
    async fn places(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        /// Geohash cell (1-9 characters). Preferred: shared, cacheable URLs.
        cell: Query<Option<String>>,
        /// `lat,lng`; quantised server-side (3 decimals).
        near: Query<Option<String>>,
        /// Metres, default 1000, max 2000; rounded up to 100 m.
        radius: Query<Option<f64>>,
        city: Query<Option<String>>,
        category: Query<Option<String>>,
        /// Name search (trigram, accent-insensitive).
        q: Query<Option<String>>,
        cursor: Query<Option<String>>,
        limit: Query<Option<i64>>,
    ) -> Result<ReadResp, ProblemResp> {
        if let Some(c) = &cell.0
            && (!geo::is_valid(c) || c.len() > 9)
        {
            return Err(ProblemResp::bad(
                "invalid-cell",
                "cell must be a geohash of 1-9 characters",
            ));
        }
        let near = match &near.0 {
            Some(n) => {
                let (a, b) = n
                    .split_once(',')
                    .ok_or_else(|| ProblemResp::bad("invalid-near", "near must be lat,lng"))?;
                let lat: f64 = a
                    .trim()
                    .parse()
                    .map_err(|_| ProblemResp::bad("invalid-near", "bad lat"))?;
                let lng: f64 = b
                    .trim()
                    .parse()
                    .map_err(|_| ProblemResp::bad("invalid-near", "bad lng"))?;
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
                    return Err(ProblemResp::bad("invalid-near", "coordinates out of range"));
                }
                let r = radius.0.unwrap_or(1000.0);
                if !r.is_finite() || r <= 0.0 {
                    return Err(ProblemResp::bad(
                        "invalid-radius",
                        "radius must be positive",
                    ));
                }
                Some(Near {
                    lat: (lat * 1000.0).round() / 1000.0,
                    lng: (lng * 1000.0).round() / 1000.0,
                    radius_m: ((r.min(2000.0) / 100.0).ceil() * 100.0).max(100.0),
                })
            }
            None => None,
        };
        let pq = PlaceQuery {
            cell: cell.0,
            near,
            city: city.0,
            category: category.0,
            q: q.0.filter(|s| !s.trim().is_empty()),
            after: cursor.0,
            limit: self::limit(limit.0),
        };
        let st = *st;
        serve(st, req, st.max_age(&[Scope::Place], None), || async {
            Ok(page_json(read::search_places(&st.db.read, &pq).await?))
        })
        .await
    }

    /// One place. Merged places redirect (301) to the survivor unless `follow=false`.
    #[oai(
        path = "/places/:id",
        method = "get",
        tag = "ApiTags::Data",
        operation_id = "place"
    )]
    async fn place(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        id: Path<String>,
        /// `false` returns the merged record (with `merged_into`) instead of redirecting.
        follow: Query<Option<bool>>,
    ) -> Result<ReadResp, ProblemResp> {
        check_id("pl_", &id.0, "place")?;
        let st = *st;
        let follow = follow.0.unwrap_or(true);
        serve(st, req, st.max_age(&[Scope::Place], None), || async {
            let p = read::get_place(&st.db.read, &id.0)
                .await?
                .ok_or_else(|| ProblemResp::not_found("place"))?;
            if let (true, Some(m)) = (follow, &p.merged_into) {
                return Err(Fail::Resp(ReadResp::Moved(format!("/v1/places/{m}"))));
            }
            Ok(p.doc)
        })
        .await
    }

    /// A place's menu: sections, menu items and current facts (pre-built document).
    #[oai(
        path = "/places/:id/menu",
        method = "get",
        tag = "ApiTags::Data",
        operation_id = "menu"
    )]
    async fn menu(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        id: Path<String>,
    ) -> Result<ReadResp, ProblemResp> {
        check_id("pl_", &id.0, "place")?;
        let st = *st;
        serve(
            st,
            req,
            st.max_age(&[Scope::Place, Scope::MenuItem], None),
            || async {
                if let Some(d) = read::menu_doc(&st.db.read, &id.0).await? {
                    return Ok(d);
                }
                Ok(read::build_menu_doc(&st.db.read, &id.0, chrono::Utc::now())
                    .await?
                    .ok_or_else(|| ProblemResp::not_found("place"))?)
            },
        )
        .await
    }

    /// One menu item with all its facts.
    #[oai(
        path = "/menu-items/:id",
        method = "get",
        tag = "ApiTags::Data",
        operation_id = "menu_item"
    )]
    async fn menu_item(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        id: Path<String>,
    ) -> Result<ReadResp, ProblemResp> {
        check_id("mi_", &id.0, "menu item")?;
        let st = *st;
        serve(st, req, st.max_age(&[Scope::MenuItem], None), || async {
            let item = read::get_doc(&st.db.read, EntityTable::MenuItems, &id.0)
                .await?
                .ok_or_else(|| ProblemResp::not_found("menu item"))?;
            let facts = read::list_facts(
                &st.db.read,
                &FactQuery {
                    menu_item_id: Some(id.0.clone()),
                    limit: 500,
                    ..Default::default()
                },
            )
            .await?;
            Ok(json!({ "item": item, "facts": facts.items }))
        })
        .await
    }

    /// List or search dishes.
    #[oai(
        path = "/dishes",
        method = "get",
        tag = "ApiTags::Data",
        operation_id = "dishes"
    )]
    async fn dishes(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        q: Query<Option<String>>,
        cursor: Query<Option<String>>,
        limit: Query<Option<i64>>,
    ) -> Result<ReadResp, ProblemResp> {
        let st = *st;
        let lim = self::limit(limit.0);
        serve(st, req, st.max_age(&[Scope::Dish], None), || async {
            Ok(page_json(
                read::list_dishes(
                    &st.db.read,
                    q.0.as_deref().filter(|s| !s.trim().is_empty()),
                    cursor.0.as_deref(),
                    lim,
                )
                .await?,
            ))
        })
        .await
    }

    /// One dish with its facts.
    #[oai(
        path = "/dishes/:id",
        method = "get",
        tag = "ApiTags::Data",
        operation_id = "dish"
    )]
    async fn dish(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        id: Path<String>,
    ) -> Result<ReadResp, ProblemResp> {
        check_id("di_", &id.0, "dish")?;
        let st = *st;
        serve(st, req, st.max_age(&[Scope::Dish], None), || async {
            let dish = read::get_doc(&st.db.read, EntityTable::Dishes, &id.0)
                .await?
                .ok_or_else(|| ProblemResp::not_found("dish"))?;
            let facts = read::list_facts(
                &st.db.read,
                &FactQuery {
                    dish_id: Some(id.0.clone()),
                    limit: 500,
                    ..Default::default()
                },
            )
            .await?;
            Ok(json!({ "dish": dish, "facts": facts.items }))
        })
        .await
    }

    /// Facts, filtered by subject and property.
    #[allow(clippy::too_many_arguments)]
    #[oai(
        path = "/facts",
        method = "get",
        tag = "ApiTags::Data",
        operation_id = "facts"
    )]
    async fn facts(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        place_id: Query<Option<String>>,
        menu_item_id: Query<Option<String>>,
        dish_id: Query<Option<String>>,
        property: Query<Option<String>>,
        cursor: Query<Option<String>>,
        limit: Query<Option<i64>>,
    ) -> Result<ReadResp, ProblemResp> {
        opt_id("pl_", &place_id.0, "place")?;
        opt_id("mi_", &menu_item_id.0, "menu item")?;
        opt_id("di_", &dish_id.0, "dish")?;
        let st = *st;
        let max_age = st.max_age(ALL_SCOPES, property.0.as_deref());
        let fq = FactQuery {
            place_id: place_id.0,
            menu_item_id: menu_item_id.0,
            dish_id: dish_id.0,
            property: property.0,
            after: cursor.0,
            limit: self::limit(limit.0),
        };
        serve(st, req, max_age, || async {
            Ok(page_json(read::list_facts(&st.db.read, &fq).await?))
        })
        .await
    }

    /// Public observations (class and pseudonym only, never keys or signatures).
    #[allow(clippy::too_many_arguments)]
    #[oai(
        path = "/observations",
        method = "get",
        tag = "ApiTags::Feed",
        operation_id = "observations"
    )]
    async fn observations(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        place_id: Query<Option<String>>,
        menu_item_id: Query<Option<String>>,
        dish_id: Query<Option<String>>,
        property: Query<Option<String>>,
        cursor: Query<Option<String>>,
        limit: Query<Option<i64>>,
    ) -> Result<ReadResp, ProblemResp> {
        opt_id("pl_", &place_id.0, "place")?;
        opt_id("mi_", &menu_item_id.0, "menu item")?;
        opt_id("di_", &dish_id.0, "dish")?;
        let st = *st;
        let max_age = st.max_age(ALL_SCOPES, property.0.as_deref());
        let oq = ObservationQuery {
            place_id: place_id.0,
            menu_item_id: menu_item_id.0,
            dish_id: dish_id.0,
            property: property.0,
            after: cursor.0,
            limit: self::limit(limit.0),
        };
        serve(st, req, max_age, || async {
            Ok(page_json(read::list_observations(&st.db.read, &oq).await?))
        })
        .await
    }

    /// The property registry (embedded `properties.json`).
    #[oai(
        path = "/registry/properties",
        method = "get",
        tag = "ApiTags::Registry"
    )]
    async fn registry(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
    ) -> Result<ReadResp, ProblemResp> {
        let st = *st;
        serve(st, req, 3600, || async {
            let items: Vec<Value> = st
                .registry
                .iter()
                .filter_map(|p| st.registry.get_raw(&p.id).cloned())
                .collect();
            Ok(json!({
                "registry_version": st.registry.version(),
                "registry_hash": st.registry.hash(),
                "items": items,
            }))
        })
        .await
    }

    /// One registry property.
    #[oai(
        path = "/registry/properties/:id",
        method = "get",
        tag = "ApiTags::Registry"
    )]
    async fn registry_property(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        id: Path<String>,
    ) -> Result<ReadResp, ProblemResp> {
        let st = *st;
        serve(st, req, 3600, || async {
            Ok(st
                .registry
                .get_raw(&id.0)
                .cloned()
                .ok_or_else(|| ProblemResp::not_found("property"))?)
        })
        .await
    }

    /// Public attestations. Only `type=vendor_of` is public.
    #[oai(
        path = "/attestations",
        method = "get",
        tag = "ApiTags::Feed",
        operation_id = "attestations"
    )]
    async fn attestations(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        #[oai(name = "type")] kind: Query<Option<String>>,
        place_id: Query<Option<String>>,
        cursor: Query<Option<String>>,
        limit: Query<Option<i64>>,
    ) -> Result<ReadResp, ProblemResp> {
        if kind.0.as_deref().is_some_and(|k| k != "vendor_of") {
            return Err(ProblemResp::bad(
                "not-public",
                "only type=vendor_of attestations are public",
            ));
        }
        opt_id("pl_", &place_id.0, "place")?;
        let st = *st;
        let lim = self::limit(limit.0);
        serve(st, req, 60, || async {
            Ok(page_json(
                read::list_vendor_bindings(
                    &st.db.read,
                    place_id.0.as_deref(),
                    cursor.0.as_deref(),
                    lim,
                )
                .await?,
            ))
        })
        .await
    }

    /// Signed attestations: `vouch`, `vendor_of`, `revoke` (MVP: issued by root or verifier keys).
    #[oai(
        path = "/attestations",
        method = "post",
        tag = "ApiTags::Write",
        operation_id = "post_attestations"
    )]
    async fn post_attestations(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        body: poem_openapi::payload::Json<Value>,
    ) -> Result<crate::writes::BatchResp, ProblemResp> {
        crate::writes::post_attestations(&st, req, body.0).await
    }

    /// Root and verifier keys.
    #[oai(
        path = "/keys/published",
        method = "get",
        tag = "ApiTags::Feed",
        operation_id = "published_keys"
    )]
    async fn published_keys(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
    ) -> Result<ReadResp, ProblemResp> {
        let st = *st;
        serve(st, req, 300, || async {
            Ok(json!({ "items": read::published_keys(&st.db.read).await? }))
        })
        .await
    }

    /// Change feed: every public write in commit order, as `(seq, entity_type, entity_id, op, at)`.
    ///
    /// `seq` is global and strictly increasing. Page with `after=<next_after>` (default 0,
    /// `limit` default 50, max 500) until `items` is empty. Entity types: `place`, `section`,
    /// `menu_item`, `dish`, `observation`, `fact` (id `<scope>:<id>|<property>`), `menu`
    /// (pre-built menu of a place), `vendor_binding`, `key` (published keys), `registry` and
    /// others added later;
    /// ignore unknown types. Re-fetch the entity to get its new state.
    ///
    /// Cheap by design: one index-only range scan on `seq` with a limit. To start, load the
    /// latest snapshot from `GET /v1/export` and continue from `after=<changes_seq>`.
    #[oai(
        path = "/changes",
        method = "get",
        tag = "ApiTags::Feed",
        operation_id = "changes"
    )]
    async fn changes(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        after: Query<Option<i64>>,
        limit: Query<Option<i64>>,
    ) -> Result<ReadResp, ProblemResp> {
        let st = *st;
        let after = after.0.unwrap_or(0).max(0);
        let lim = self::limit(limit.0);
        serve(st, req, 5, || async {
            let items = read::changes(&st.db.read, after, lim).await?;
            let next = items
                .last()
                .and_then(|v| v.get("seq"))
                .and_then(Value::as_i64)
                .unwrap_or(after);
            Ok(json!({ "items": items, "next_after": next }))
        })
        .await
    }
}
