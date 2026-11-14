//! Signed admin endpoints (AGENTS.md 6.3): OSM links, merges and removals.
//!
//! Same scheme as every other write: Ed25519 over the RFC 8785 form of the object without
//! `signature`. The issuer must be an active root or verifier key. The object is
//!
//! ```json
//! { "id": "es_<ULID>", "action": "...", "params": { ... },
//!   "issued_at": "<RFC 3339>", "issuer_key_id": "<key_id>", "signature": "<86>" }
//! ```
//!
//! Replay protection: `issued_at` must be within `ADMIN_WINDOW` of server time and the `id`
//! is unique (`private.admin_actions`). Resending the identical object returns the stored
//! result with status `duplicate`; a different object with a used id is a 409.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use eatsights_core::crypto;
use eatsights_core::ids::{is_valid_key_id, is_valid_prefixed};
use eatsights_db::DbError;
use eatsights_db::admin::{self, AdminOutcome, AdminRecord, MergeRequest};
use eatsights_db::write;
use poem::Request;
use poem::web::Data;
use poem_openapi::payload::Json;
use poem_openapi::{ApiResponse, Object, OpenApi, Tags};
use serde_json::Value;

use crate::problem::{Problem, ProblemResp};
use crate::state::AppState;

/// Accepted clock difference for `issued_at` (both directions).
pub const ADMIN_WINDOW: Duration = Duration::minutes(5);

/// Reason codes for removals (no free text in public data).
pub const REMOVAL_REASONS: [&str; 2] = ["legal", "safety"];

#[derive(Tags)]
pub enum AdminTags {
    /// Actions signed by the root or a verifier key.
    Admin,
}

/// Result of an admin action.
#[derive(Debug, Clone, Object)]
pub struct AdminResult {
    pub id: String,
    pub action: String,
    /// `accepted` or `duplicate` (the same signed object was applied before).
    pub status: String,
    pub result: Value,
}

#[derive(ApiResponse)]
pub enum AdminResp {
    /// Applied (or already applied).
    #[oai(status = 200)]
    Ok(Json<AdminResult>),
}

fn p(status: u16, slug: &str, title: &str, detail: impl Into<String>) -> ProblemResp {
    ProblemResp::new(Problem::new(status, slug, title, Some(detail.into())))
}

fn bad(detail: impl Into<String>) -> ProblemResp {
    p(422, "invalid-admin-action", "Invalid admin action", detail)
}

fn db_err(e: DbError) -> ProblemResp {
    match e {
        DbError::Conflict(d) => p(409, "conflict", "Conflict", d),
        DbError::NotFound(d) => p(404, "not-found", "Not found", d),
        DbError::BadReference(d) => p(422, "invalid-reference", "Invalid reference", d),
        other => ProblemResp::internal(other),
    }
}

/// A verified admin object.
struct Verified {
    id: String,
    action: String,
    issuer_key_id: String,
    params: serde_json::Map<String, Value>,
    issued_at: DateTime<Utc>,
}

/// `Ok(Err(resp))` answers a resent identical object from the log.
async fn verify(
    st: &AppState,
    body: &Value,
    allowed: &[&str],
) -> Result<Result<Verified, AdminResp>, ProblemResp> {
    let obj = body
        .as_object()
        .ok_or_else(|| bad("body must be an object"))?;
    const FIELDS: [&str; 6] = [
        "id",
        "action",
        "params",
        "issued_at",
        "issuer_key_id",
        "signature",
    ];
    if let Some(k) = obj.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(bad(format!("unknown field `{k}`")));
    }
    if let Some(k) = FIELDS.iter().find(|k| !obj.contains_key(**k)) {
        return Err(bad(format!("missing field `{k}`")));
    }
    let s = |k: &str| obj.get(k).and_then(Value::as_str);
    let id = s("id")
        .filter(|i| is_valid_prefixed("es_", i))
        .ok_or_else(|| bad("id must be es_ + ULID"))?;
    let action = s("action").unwrap_or_default();
    if !allowed.contains(&action) {
        return Err(bad(format!(
            "action must be one of {} on this endpoint",
            allowed.join(", ")
        )));
    }
    let params = obj
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("params must be an object"))?;
    let issued_at = s("issued_at")
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| bad("issued_at must be RFC 3339"))?;
    let issuer = s("issuer_key_id")
        .filter(|k| is_valid_key_id(k))
        .ok_or_else(|| bad("issuer_key_id is invalid"))?;
    let key = write::get_key(&st.db.write, issuer)
        .await
        .map_err(db_err)?
        .ok_or_else(|| {
            p(
                401,
                "unknown-key",
                "Unknown key",
                "issuer key is not registered",
            )
        })?;
    let vk = crypto::parse_public_key(&key.public_key).map_err(ProblemResp::internal)?;
    crypto::verify_object(body, &vk)
        .map_err(|e| p(401, "bad-signature", "Bad signature", e.to_string()))?;
    if key.status != "active" || !matches!(key.level.as_str(), "root" | "verifier") {
        return Err(p(
            403,
            "forbidden",
            "Forbidden",
            "admin actions need an active root or verifier key",
        ));
    }
    if st.limit_key.check_key(&key.key_id).is_err() {
        return Err(p(
            429,
            "rate-limited",
            "Too many requests",
            "per-key rate limit",
        ));
    }
    // Replay: an id seen before is answered from the log (identical object) or refused.
    if let Some((raw, result)) = admin::admin_action(&st.db.write, id)
        .await
        .map_err(db_err)?
    {
        if &raw == body {
            return Ok(Err(AdminResp::Ok(Json(AdminResult {
                id: id.to_owned(),
                action: action.to_owned(),
                status: "duplicate".into(),
                result,
            }))));
        }
        return Err(p(409, "replay", "Replay", "this id was already used"));
    }
    let now = Utc::now();
    if issued_at > now + ADMIN_WINDOW || issued_at < now - ADMIN_WINDOW {
        return Err(p(
            422,
            "issued-at-window",
            "Outside the time window",
            "issued_at must be within 5 minutes of server time",
        ));
    }
    Ok(Ok(Verified {
        id: id.to_owned(),
        action: action.to_owned(),
        issuer_key_id: key.key_id,
        params: params.clone(),
        issued_at,
    }))
}

fn param<'a>(v: &'a Verified, k: &str) -> Option<&'a str> {
    v.params.get(k).and_then(Value::as_str)
}

fn only(v: &Verified, keys: &[&str]) -> Result<(), ProblemResp> {
    match v.params.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(bad(format!("unknown param `{k}`"))),
        None => Ok(()),
    }
}

fn respond(v: &Verified, out: AdminOutcome) -> AdminResp {
    let (status, result) = match out {
        AdminOutcome::Done(r) => ("accepted", r),
        AdminOutcome::Duplicate { result, .. } => ("duplicate", result),
    };
    AdminResp::Ok(Json(AdminResult {
        id: v.id.clone(),
        action: v.action.clone(),
        status: status.into(),
        result,
    }))
}

fn is_osm_id(s: &str) -> bool {
    let Some((kind, n)) = s.split_once('/') else {
        return false;
    };
    matches!(kind, "node" | "way" | "relation")
        && !n.is_empty()
        && n.len() <= 19
        && n.bytes().all(|b| b.is_ascii_digit())
}

fn entity_prefix(entity_type: &str) -> Result<&'static str, ProblemResp> {
    match entity_type {
        "place" => Ok("pl_"),
        "menu_item" => Ok("mi_"),
        _ => Err(bad("entity_type must be place or menu_item")),
    }
}

pub struct AdminApi;

#[OpenApi(prefix_path = "/v1/admin", tag = "AdminTags::Admin")]
impl AdminApi {
    /// Confirm an OSM object for a place (`action: "link_osm"`, params `place_id`, `osm_id`
    /// such as `node/123`). The place becomes `linked` with `verified_at`. People confirm;
    /// the system never links on its own.
    #[oai(path = "/osm-links", method = "post", operation_id = "admin_osm_link")]
    async fn osm_link(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        body: Json<Value>,
    ) -> Result<AdminResp, ProblemResp> {
        st.check_ip(st.ip(req))?;
        let v = match verify(&st, &body.0, &["link_osm"]).await? {
            Ok(v) => v,
            Err(dup) => return Ok(dup),
        };
        only(&v, &["place_id", "osm_id"])?;
        let place = param(&v, "place_id")
            .filter(|i| is_valid_prefixed("pl_", i))
            .ok_or_else(|| bad("params.place_id must be a place id"))?;
        let osm_id = param(&v, "osm_id")
            .filter(|i| is_osm_id(i))
            .ok_or_else(|| bad("params.osm_id must be node/<n>, way/<n> or relation/<n>"))?;
        let rec = record(&v, &body.0);
        let out = admin::link_osm(&st.db.write, &rec, place, osm_id, Utc::now())
            .await
            .map_err(db_err)?;
        Ok(respond(&v, out))
    }

    /// Merges. `action: "merge"` with params `entity_type` (`place` default, or `menu_item`)
    /// and either `a_id` + `b_id` or `suggestion_id`; optional `survivor_id` (default: the
    /// OSM-linked record, else the older). `action: "unmerge"` with `entity_type`, `id`.
    /// `action: "dismiss"` with `suggestion_id`. Nothing is deleted.
    #[oai(path = "/merges", method = "post", operation_id = "admin_merges")]
    async fn merges(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        body: Json<Value>,
    ) -> Result<AdminResp, ProblemResp> {
        st.check_ip(st.ip(req))?;
        let v = match verify(&st, &body.0, &["merge", "unmerge", "dismiss"]).await? {
            Ok(v) => v,
            Err(dup) => return Ok(dup),
        };
        let rec = record(&v, &body.0);
        let now = Utc::now();
        let suggestion = match v.params.get("suggestion_id") {
            None => None,
            Some(s) => Some(
                s.as_i64()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| bad("params.suggestion_id must be a positive integer"))?,
            ),
        };
        let out = match v.action.as_str() {
            "merge" => {
                only(
                    &v,
                    &[
                        "entity_type",
                        "a_id",
                        "b_id",
                        "survivor_id",
                        "suggestion_id",
                    ],
                )?;
                let (etype, a, b) = match (param(&v, "a_id"), param(&v, "b_id"), suggestion) {
                    (Some(a), Some(b), _) => (
                        param(&v, "entity_type").unwrap_or("place").to_owned(),
                        a.to_owned(),
                        b.to_owned(),
                    ),
                    (None, None, Some(sid)) => {
                        let s = eatsights_db::osm::get_suggestion(&st.db.write, sid)
                            .await
                            .map_err(db_err)?
                            .ok_or_else(|| ProblemResp::not_found("suggestion"))?;
                        (s.entity_type, s.a_id, s.b_id)
                    }
                    _ => return Err(bad("give a_id and b_id, or suggestion_id")),
                };
                let prefix = entity_prefix(&etype)?;
                let survivor = param(&v, "survivor_id");
                if ![Some(a.as_str()), Some(b.as_str()), survivor]
                    .into_iter()
                    .flatten()
                    .all(|i| is_valid_prefixed(prefix, i))
                {
                    return Err(bad(format!("ids must be {prefix} + ULID")));
                }
                admin::merge(
                    &st.db.write,
                    &rec,
                    &MergeRequest {
                        entity_type: &etype,
                        a_id: &a,
                        b_id: &b,
                        survivor_id: survivor,
                        suggestion_id: suggestion,
                    },
                    now,
                )
                .await
            }
            "unmerge" => {
                only(&v, &["entity_type", "id"])?;
                let etype = param(&v, "entity_type").unwrap_or("place");
                let prefix = entity_prefix(etype)?;
                let id = param(&v, "id")
                    .filter(|i| is_valid_prefixed(prefix, i))
                    .ok_or_else(|| bad(format!("params.id must be {prefix} + ULID")))?;
                admin::unmerge(&st.db.write, &rec, etype, id, now).await
            }
            _ => {
                only(&v, &["suggestion_id"])?;
                let sid = suggestion.ok_or_else(|| bad("params.suggestion_id is required"))?;
                admin::dismiss_suggestion(&st.db.write, &rec, sid, now).await
            }
        }
        .map_err(db_err)?;
        Ok(respond(&v, out))
    }

    /// Legal or safety removal of an observation (`action: "remove_observation"`, params
    /// `observation_id`, `reason`: `legal` or `safety`). The content is replaced by a
    /// tombstone; the public removal log holds only the id and the reason code.
    #[oai(path = "/removals", method = "post", operation_id = "admin_removals")]
    async fn removals(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        body: Json<Value>,
    ) -> Result<AdminResp, ProblemResp> {
        st.check_ip(st.ip(req))?;
        let v = match verify(&st, &body.0, &["remove_observation"]).await? {
            Ok(v) => v,
            Err(dup) => return Ok(dup),
        };
        only(&v, &["observation_id", "reason"])?;
        let obs = param(&v, "observation_id")
            .filter(|i| is_valid_prefixed("ob_", i))
            .ok_or_else(|| bad("params.observation_id must be ob_ + ULID"))?;
        let reason = param(&v, "reason")
            .filter(|r| REMOVAL_REASONS.contains(r))
            .ok_or_else(|| bad("params.reason must be legal or safety"))?;
        let rec = record(&v, &body.0);
        let out = admin::remove_observation(&st.db.write, &rec, obs, reason, Utc::now())
            .await
            .map_err(db_err)?;
        Ok(respond(&v, out))
    }
}

fn record<'a>(v: &'a Verified, raw: &'a Value) -> AdminRecord<'a> {
    AdminRecord {
        id: &v.id,
        action: &v.action,
        issuer_key_id: &v.issuer_key_id,
        raw,
        issued_at: v.issued_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osm_ids() {
        assert!(is_osm_id("node/1"));
        assert!(is_osm_id("relation/123456"));
        assert!(!is_osm_id("node/"));
        assert!(!is_osm_id("area/1"));
        assert!(!is_osm_id("node/1a"));
    }
}
