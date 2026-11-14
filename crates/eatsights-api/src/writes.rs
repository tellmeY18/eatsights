//! Signed writes (AGENTS.md 6.2 and section 7).

use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use eatsights_core::crypto;
use eatsights_core::ids::{is_valid_key_id, is_valid_prefixed};
use eatsights_core::schema;
use eatsights_core::submission::{
    ContributorClass, SourceType, check_submission, to_public_observation,
};
use eatsights_core::trust::{self, Level};
use eatsights_db::DbError;
use eatsights_db::write::{
    self, KeyRow, NewAttestation, NewEntitySubmission, NewObservation, Outcome,
};
use poem::Request;
use poem::web::Data;
use poem_openapi::payload::Json;
use poem_openapi::{ApiResponse, Object, OpenApi};
use serde_json::{Map, Value, json};

use crate::problem::{Problem, ProblemResp};
use crate::reads::ApiTags;
use crate::state::{AppState, Metrics, client_ip};

pub const MAX_BATCH: usize = 100;

/// Result of one item in a batch.
#[derive(Debug, Clone, Object)]
pub struct ItemResult {
    pub index: u32,
    #[oai(skip_serializing_if_is_none)]
    pub id: Option<String>,
    /// `accepted`, `duplicate` or `rejected`.
    pub status: String,
    #[oai(skip_serializing_if_is_none)]
    pub problem: Option<Problem>,
}

#[derive(Debug, Clone, Object)]
pub struct BatchResult {
    pub results: Vec<ItemResult>,
}

#[derive(ApiResponse)]
pub enum BatchResp {
    /// Per-item results (the batch is not all-or-nothing).
    #[oai(status = 200)]
    Ok(Json<BatchResult>),
}

#[derive(Debug, Clone, Object)]
pub struct KeyRegistered {
    pub key_id: String,
    pub level: String,
    pub status: String,
}

#[derive(ApiResponse)]
pub enum KeyResp {
    /// Registered.
    #[oai(status = 201)]
    Created(Json<KeyRegistered>),
    /// Already registered.
    #[oai(status = 200)]
    Existing(Json<KeyRegistered>),
}

fn p(status: u16, slug: &str, title: &str, detail: impl Into<String>) -> Problem {
    Problem::new(status, slug, title, Some(detail.into()))
}

fn str_of<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(Value::as_str)
}

fn parse_time(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn items(body: Value) -> Result<Vec<Value>, ProblemResp> {
    let items = match body {
        Value::Array(a) => a,
        o @ Value::Object(_) => vec![o],
        _ => {
            return Err(ProblemResp::bad(
                "invalid-batch",
                "body must be a JSON array of items",
            ));
        }
    };
    if items.is_empty() || items.len() > MAX_BATCH {
        return Err(ProblemResp::new(p(
            413,
            "batch-too-large",
            "Batch size",
            format!("a batch holds 1 to {MAX_BATCH} items"),
        )));
    }
    Ok(items)
}

fn db_problem(e: DbError) -> Problem {
    match e {
        DbError::Conflict(d) => p(409, "conflict", "Conflict", d),
        DbError::NotFound(d) => p(404, "not-found", "Not found", d),
        DbError::BadReference(d) => p(422, "invalid-reference", "Invalid reference", d),
        other => {
            tracing::error!(error = %other, "write failed");
            Problem::new(500, "internal", "Internal error", None)
        }
    }
}

impl AppState {
    fn ip(&self, req: &Request) -> Option<IpAddr> {
        let peer = req.remote_addr().as_socket_addr().map(|a| a.ip());
        let xff = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok());
        client_ip(peer, xff, &self.cfg.trusted_proxies)
    }

    fn check_ip(&self, ip: Option<IpAddr>) -> Result<(), ProblemResp> {
        match ip {
            Some(ip) if self.limit_ip.check_key(&ip).is_err() => Err(ProblemResp::new(p(
                429,
                "rate-limited",
                "Too many requests",
                "per-address rate limit",
            ))),
            _ => Ok(()),
        }
    }

    /// Steps 2-3: look up the key, verify the signature, require `active`, rate-limit.
    async fn authenticate(&self, doc: &Value, key_id: &str) -> Result<KeyRow, Problem> {
        let key = write::get_key(&self.db.write, key_id)
            .await
            .map_err(db_problem)?
            .ok_or_else(|| {
                p(
                    401,
                    "unknown-key",
                    "Unknown key",
                    "register the key with POST /v1/keys first",
                )
            })?;
        let vk = crypto::parse_public_key(&key.public_key)
            .map_err(|_| Problem::new(500, "internal", "Internal error", None))?;
        crypto::verify_object(doc, &vk)
            .map_err(|e| p(401, "bad-signature", "Bad signature", e.to_string()))?;
        if key.status != "active" {
            return Err(p(
                403,
                "key-inactive",
                "Key not active",
                format!("key status is {}", key.status),
            ));
        }
        if self.limit_key.check_key(&key.key_id).is_err() {
            return Err(p(
                429,
                "rate-limited",
                "Too many requests",
                "per-key rate limit",
            ));
        }
        Ok(key)
    }

    fn record(&self, r: &ItemResult) {
        let c = match r.status.as_str() {
            "accepted" => &self.metrics.items_accepted,
            "duplicate" => &self.metrics.items_duplicate,
            _ => &self.metrics.items_rejected,
        };
        Metrics::inc(c);
    }

    async fn submit_one(&self, sub: &Value) -> Result<&'static str, Problem> {
        // 1. envelope
        schema::validate_def("submission", sub).map_err(|e| {
            p(
                422,
                "invalid-submission",
                "Invalid submission",
                e.join("; "),
            )
        })?;
        let id = str_of(sub, "/id").unwrap_or_default();
        let key_id = str_of(sub, "/contributor/key_id").unwrap_or_default();
        // 2-3. signature, key, rate limit
        let key = self.authenticate(sub, key_id).await?;
        // 6 (early). idempotency: an already stored id returns its original result
        if write::submission_result(&self.db.write, id)
            .await
            .map_err(db_problem)?
            .is_some()
        {
            return Ok("duplicate");
        }
        // 4-5. registry and time checks
        let now = Utc::now();
        let checked = check_submission(sub, self.registry, now, &self.cfg.check)
            .map_err(|e| p(e.status(), e.slug(), e.title(), e.to_string()))?;
        // 7. class
        let class = match checked.source_type {
            SourceType::Vendor => {
                let place = checked.place_id.as_deref().ok_or_else(|| {
                    p(
                        403,
                        "not-vendor",
                        "Not a vendor",
                        "vendor observations need a place",
                    )
                })?;
                if !write::has_vendor_binding(&self.db.write, &key.key_id, place, now)
                    .await
                    .map_err(db_problem)?
                {
                    return Err(p(
                        403,
                        "not-vendor",
                        "Not a vendor",
                        "key holds no current vendor_of for this place",
                    ));
                }
                ContributorClass::Vendor
            }
            SourceType::Import => ContributorClass::Import,
            SourceType::Customer => match key.level.as_str() {
                "member" => ContributorClass::Member,
                "trusted" => ContributorClass::Trusted,
                "verifier" | "root" => ContributorClass::Verifier,
                _ => ContributorClass::Unverified,
            },
        };
        // 8. pseudonym
        let pseudonym = crypto::pseudonym(&self.cfg.pseudonym_salt, &key.key_id, &checked.anchor)
            .map_err(|_| Problem::new(500, "internal", "Internal error", None))?;
        let public = to_public_observation(sub, class, &pseudonym);
        let scope = checked.scope.as_str();
        let subject_key = eatsights_db::subject_key(
            scope,
            checked.place_id.as_deref(),
            checked.menu_item_id.as_deref(),
            checked.dish_id.as_deref(),
        )
        .unwrap_or_default();
        let result = json!({ "id": checked.id, "status": "accepted" });
        // 9. one transaction
        let outcome = write::insert_observation(
            &self.db.write,
            &NewObservation {
                id: &checked.id,
                property: &checked.property,
                scope,
                place_id: checked.place_id.as_deref(),
                menu_item_id: checked.menu_item_id.as_deref(),
                dish_id: checked.dish_id.as_deref(),
                subject_key: &subject_key,
                public_doc: &public,
                pseudonym: &pseudonym,
                salt_version: self.cfg.pseudonym_salt_version,
                class: class.as_str(),
                observed_at: checked.observed_at,
                submitted_at: checked.submitted_at,
                raw: sub,
                key_id: &key.key_id,
                result: &result,
            },
        )
        .await
        .map_err(db_problem)?;
        Ok(match outcome {
            Outcome::Accepted => "accepted",
            Outcome::Duplicate(_) => "duplicate",
        })
    }

    async fn entity_one(&self, es: &Value) -> Result<&'static str, Problem> {
        let bad = |d: &str| {
            p(
                422,
                "invalid-entity-submission",
                "Invalid entity submission",
                d,
            )
        };
        let obj = es
            .as_object()
            .ok_or_else(|| bad("item must be an object"))?;
        const FIELDS: [&str; 9] = [
            "id",
            "schema_version",
            "op",
            "entity_type",
            "entity",
            "submitted_at",
            "contributor",
            "license",
            "signature",
        ];
        if let Some(k) = obj.keys().find(|k| !FIELDS.contains(&k.as_str())) {
            return Err(bad(&format!("unknown field `{k}`")));
        }
        if let Some(k) = FIELDS.iter().find(|k| !obj.contains_key(**k)) {
            return Err(bad(&format!("missing field `{k}`")));
        }
        let id = str_of(es, "/id")
            .filter(|i| is_valid_prefixed("es_", i))
            .ok_or_else(|| bad("id must be es_ + ULID"))?;
        if str_of(es, "/schema_version") != Some("0.4") {
            return Err(bad("schema_version must be \"0.4\""));
        }
        let op = str_of(es, "/op")
            .filter(|o| *o == "create" || *o == "edit")
            .ok_or_else(|| bad("op must be create or edit"))?;
        let (etype, prefix) = match str_of(es, "/entity_type") {
            Some("place") => ("place", "pl_"),
            Some("section") => ("section", "sc_"),
            Some("menu_item") => ("menu_item", "mi_"),
            Some("dish") => ("dish", "di_"),
            _ => return Err(bad("entity_type must be place, section, menu_item or dish")),
        };
        if str_of(es, "/license") != Some("CC0-1.0") {
            return Err(p(
                422,
                "invalid-license",
                "Invalid licence",
                "license must be CC0-1.0",
            ));
        }
        let submitted_at = parse_time(str_of(es, "/submitted_at"))
            .ok_or_else(|| bad("submitted_at must be RFC 3339"))?;
        let contributor = es
            .get("contributor")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("contributor must be an object"))?;
        if contributor.len() != 1 {
            return Err(bad("contributor holds only key_id"));
        }
        let key_id = contributor
            .get("key_id")
            .and_then(Value::as_str)
            .filter(|k| is_valid_key_id(k))
            .ok_or_else(|| bad("contributor.key_id is invalid"))?;
        if str_of(es, "/signature").is_none_or(|s| s.len() != 86) {
            return Err(bad("signature must be 86 base64url characters"));
        }
        let entity = es
            .get("entity")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("entity must be an object"))?;
        let entity_id = entity
            .get("id")
            .and_then(Value::as_str)
            .filter(|i| is_valid_prefixed(prefix, i))
            .ok_or_else(|| bad(&format!("entity.id must be {prefix} + ULID")))?;

        let key = self.authenticate(es, key_id).await?;
        if write::entity_submission_exists(&self.db.write, id)
            .await
            .map_err(db_problem)?
        {
            return Ok("duplicate");
        }
        let now = Utc::now();
        if submitted_at > now + self.cfg.check.future_allowance
            || submitted_at < now - self.cfg.check.submitted_max_lag
        {
            return Err(p(
                422,
                "submitted-at-skew",
                "Clock skew",
                "submitted_at is too far from server time",
            ));
        }
        let server_fields: &[&str] = match etype {
            "place" => &["osm", "record_state", "merged_into", "created_at"],
            "menu_item" => &["record_state", "merged_into", "created_at"],
            _ => &[],
        };
        if let Some(f) = server_fields.iter().find(|f| entity.contains_key(**f)) {
            return Err(p(
                422,
                "server-controlled-field",
                "Server-controlled field",
                format!("`{f}` is set by the server"),
            ));
        }
        let existing = write::entity_for_edit(&self.db.write, etype, entity_id)
            .await
            .map_err(db_problem)?;
        let mut doc: Map<String, Value> = entity.clone();
        let now_s = now.to_rfc3339_opts(SecondsFormat::Secs, true);
        match (op, &existing) {
            ("create", _) => match etype {
                "place" => {
                    doc.insert("osm".into(), json!({ "state": "unchecked" }));
                    doc.insert("record_state".into(), json!("active"));
                    doc.insert("created_at".into(), json!(now_s));
                }
                "menu_item" => {
                    doc.insert("record_state".into(), json!("active"));
                    doc.insert("created_at".into(), json!(now_s));
                }
                _ => {}
            },
            (_, None) => {
                return Err(p(
                    404,
                    "not-found",
                    "Not found",
                    format!("{entity_id} does not exist"),
                ));
            }
            (_, Some(cur)) => {
                for f in server_fields {
                    if let Some(v) = cur.get(*f) {
                        doc.insert((*f).to_owned(), v.clone());
                    }
                }
                if matches!(etype, "section" | "menu_item")
                    && cur.get("place_id") != doc.get("place_id")
                {
                    return Err(p(
                        422,
                        "immutable-field",
                        "Immutable field",
                        "place_id cannot change",
                    ));
                }
                let place = match etype {
                    "place" => Some(entity_id),
                    "section" | "menu_item" => cur.get("place_id").and_then(Value::as_str),
                    _ => None,
                };
                let ranked = matches!(
                    key.level.as_str(),
                    "member" | "trusted" | "verifier" | "root"
                );
                let vendor = match place {
                    Some(pl) => write::has_vendor_binding(&self.db.write, &key.key_id, pl, now)
                        .await
                        .map_err(db_problem)?,
                    None => false,
                };
                if !ranked && !vendor {
                    return Err(p(
                        403,
                        "forbidden",
                        "Forbidden",
                        "edits need level member or above, or vendor_of for the place",
                    ));
                }
            }
        }
        let doc = Value::Object(doc);
        schema::validate_def(etype, &doc)
            .map_err(|e| p(422, "invalid-entity", "Invalid entity", e.join("; ")))?;
        let outcome = write::write_entity(
            &self.db.write,
            &NewEntitySubmission {
                id,
                raw: es,
                key_id: &key.key_id,
                entity_type: etype,
                entity_id,
                op,
            },
            &doc,
            op == "create",
            now,
        )
        .await
        .map_err(db_problem)?;
        Ok(match outcome {
            Outcome::Accepted => "accepted",
            Outcome::Duplicate(_) => "duplicate",
        })
    }

    async fn attestation_one(&self, a: &Value) -> Result<&'static str, Problem> {
        schema::validate_def("attestation", a).map_err(|e| {
            p(
                422,
                "invalid-attestation",
                "Invalid attestation",
                e.join("; "),
            )
        })?;
        let forbidden = |d: &str| p(403, "forbidden", "Forbidden", d);
        let id = str_of(a, "/id").unwrap_or_default();
        let kind = str_of(a, "/type").unwrap_or_default();
        let issuer_id = str_of(a, "/issuer_key_id").unwrap_or_default();
        let subject_id = str_of(a, "/subject_key_id").unwrap_or_default();
        let issuer = self.authenticate(a, issuer_id).await?;
        if write::get_attestation(&self.db.write, id)
            .await
            .map_err(db_problem)?
            .is_some()
        {
            return Ok("duplicate");
        }
        let now = Utc::now();
        let issued_at = parse_time(str_of(a, "/issued_at")).unwrap_or(now);
        if issued_at > now + Duration::minutes(5) {
            return Err(p(
                422,
                "issued-in-future",
                "Issued in the future",
                "issued_at is in the future",
            ));
        }
        let issuer_level = Level::parse(&issuer.level).unwrap_or(Level::Unverified);
        let cfg = &self.cfg.trust;
        if kind != "revoke" && subject_id == issuer.key_id {
            return Err(p(
                422,
                "self-attestation",
                "Self attestation",
                "issuer and subject must differ",
            ));
        }
        let mut revokes = None;
        let mut strike_against = None;
        let mut expires_at = parse_time(str_of(a, "/expires_at"));
        let subject = if kind == "revoke" {
            None
        } else {
            let s = write::get_key(&self.db.write, subject_id)
                .await
                .map_err(db_problem)?
                .filter(|s| s.status == "active")
                .ok_or_else(|| {
                    p(
                        422,
                        "unknown-subject-key",
                        "Unknown subject key",
                        "subject key is not registered or not active",
                    )
                })?;
            Some(s)
        };
        match kind {
            "vouch" => {
                let granted = str_of(a, "/level_granted")
                    .and_then(Level::parse)
                    .unwrap_or(Level::Member);
                if !trust::can_grant(issuer_level, granted) {
                    return Err(forbidden(&format!(
                        "a {} key cannot grant {}",
                        issuer_level.as_str(),
                        granted.as_str()
                    )));
                }
                if issuer.vouch_cooldown_until.is_some_and(|t| t > now) {
                    return Err(p(
                        403,
                        "vouch-cooldown",
                        "Vouch cooldown",
                        "the issuer may not vouch until its cooldown ends",
                    ));
                }
                if issuer.depth.unwrap_or(i32::MAX).saturating_add(1) > cfg.max_depth {
                    return Err(p(
                        422,
                        "depth-exceeded",
                        "Chain too deep",
                        format!("the subject would be deeper than {}", cfg.max_depth),
                    ));
                }
            }
            "vendor_of" => {
                if issuer_level < Level::Verifier {
                    return Err(forbidden("only verifier and root keys bind vendors"));
                }
                let max = issued_at + Duration::days(cfg.vendor_of_max_days);
                match expires_at {
                    None => expires_at = Some(max),
                    Some(e) if e <= issued_at || e > max => {
                        return Err(p(
                            422,
                            "invalid-expiry",
                            "Invalid expiry",
                            format!(
                                "expires_at must be after issued_at and at most {} days later",
                                cfg.vendor_of_max_days
                            ),
                        ));
                    }
                    Some(_) => {}
                }
            }
            "revoke" => {
                let target = str_of(a, "/revokes").unwrap_or_default();
                let t = write::get_attestation(&self.db.write, target)
                    .await
                    .map_err(db_problem)?
                    .ok_or_else(|| {
                        p(
                            404,
                            "not-found",
                            "Not found",
                            "revoked attestation not found",
                        )
                    })?;
                if t.kind != "vouch" && t.kind != "vendor_of" {
                    return Err(p(
                        422,
                        "not-revocable",
                        "Not revocable",
                        format!("a {} attestation cannot be revoked", t.kind),
                    ));
                }
                if t.revoked {
                    return Err(p(
                        409,
                        "already-revoked",
                        "Already revoked",
                        "the attestation is already revoked",
                    ));
                }
                if t.subject_key_id != subject_id {
                    return Err(p(
                        422,
                        "subject-mismatch",
                        "Subject mismatch",
                        "subject_key_id must equal the revoked attestation's subject",
                    ));
                }
                let own = t.issuer_key_id == issuer.key_id;
                if !own && issuer_level < Level::Verifier {
                    return Err(forbidden("only the issuer, a verifier or root may revoke"));
                }
                if t.kind == "vouch" && !own {
                    strike_against = Some(t.issuer_key_id);
                }
                revokes = Some(target);
            }
            _ => {
                // rotate: the old key (issuer) hands over to a fresh key (subject).
                if issuer_level == Level::Root {
                    return Err(p(
                        422,
                        "root-rotation",
                        "Root rotation",
                        "the root key is rotated by configuration",
                    ));
                }
                if subject.as_ref().is_some_and(|s| s.level != "unverified") {
                    return Err(p(
                        409,
                        "subject-not-fresh",
                        "Subject not fresh",
                        "the successor key must be newly registered (unverified)",
                    ));
                }
            }
        }
        let outcome = write::insert_attestation(
            &self.db.write,
            &NewAttestation {
                id,
                kind,
                issuer_key_id: &issuer.key_id,
                subject_key_id: subject_id,
                raw: a,
                place_id: str_of(a, "/place_id"),
                revokes,
                issued_at,
                expires_at,
                strike_against: strike_against.as_deref(),
            },
            cfg,
        )
        .await
        .map_err(db_problem)?;
        Ok(match outcome {
            Outcome::Accepted => "accepted",
            Outcome::Duplicate(_) => "duplicate",
        })
    }
}

fn result(index: usize, id: Option<String>, r: Result<&'static str, Problem>) -> ItemResult {
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    match r {
        Ok(status) => ItemResult {
            index,
            id,
            status: status.to_owned(),
            problem: None,
        },
        Err(problem) => ItemResult {
            index,
            id,
            status: "rejected".to_owned(),
            problem: Some(problem),
        },
    }
}

pub struct WriteApi;

#[OpenApi(prefix_path = "/v1")]
impl WriteApi {
    /// Register an Ed25519 public key (proof of possession). New keys are `unverified`.
    #[oai(
        path = "/keys",
        method = "post",
        tag = "ApiTags::Write",
        operation_id = "register_key"
    )]
    async fn register_key(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        body: Json<Value>,
    ) -> Result<KeyResp, ProblemResp> {
        let ip = st.ip(req);
        if let Some(ip) = ip
            && st.limit_keyreg.check_key(&ip).is_err()
        {
            return Err(ProblemResp::new(p(
                429,
                "rate-limited",
                "Too many requests",
                "key registration limit",
            )));
        }
        let b = &body.0;
        let bad = |d: &str| {
            ProblemResp::new(p(
                422,
                "invalid-key-registration",
                "Invalid key registration",
                d,
            ))
        };
        let obj = b.as_object().ok_or_else(|| bad("body must be an object"))?;
        const FIELDS: [&str; 5] = ["type", "algorithm", "public_key", "created_at", "signature"];
        if obj.len() != FIELDS.len() || FIELDS.iter().any(|f| !obj.contains_key(*f)) {
            return Err(bad(
                "fields must be exactly type, algorithm, public_key, created_at, signature",
            ));
        }
        if str_of(b, "/type") != Some("key_registration")
            || str_of(b, "/algorithm") != Some("ed25519")
        {
            return Err(bad("type must be key_registration and algorithm ed25519"));
        }
        let created_at = parse_time(str_of(b, "/created_at"))
            .ok_or_else(|| bad("created_at must be RFC 3339"))?;
        let pk = str_of(b, "/public_key").unwrap_or_default();
        let vk = crypto::parse_public_key(pk).map_err(|e| bad(&e.to_string()))?;
        crypto::verify_object(b, &vk).map_err(|e| {
            ProblemResp::new(p(401, "bad-signature", "Bad signature", e.to_string()))
        })?;
        let key_id = crypto::key_id(&vk);
        let fresh = write::register_key(
            &st.db.write,
            &key_id,
            &crypto::encode_public_key(&vk),
            created_at,
        )
        .await?;
        let k = write::get_key(&st.db.write, &key_id)
            .await?
            .ok_or_else(|| ProblemResp::internal("key vanished"))?;
        let out = Json(KeyRegistered {
            key_id,
            level: k.level,
            status: k.status,
        });
        Ok(if fresh {
            KeyResp::Created(out)
        } else {
            KeyResp::Existing(out)
        })
    }

    /// Submit up to 100 signed observations. Per-item results.
    #[oai(
        path = "/submissions",
        method = "post",
        tag = "ApiTags::Write",
        operation_id = "submissions"
    )]
    async fn submissions(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        body: Json<Value>,
    ) -> Result<BatchResp, ProblemResp> {
        st.check_ip(st.ip(req))?;
        let items = items(body.0)?;
        let mut results = Vec::with_capacity(items.len());
        for (i, sub) in items.iter().enumerate() {
            let id = str_of(sub, "/id").map(str::to_owned);
            let r = result(i, id, st.submit_one(sub).await);
            st.record(&r);
            results.push(r);
        }
        Ok(BatchResp::Ok(Json(BatchResult { results })))
    }

    /// Create or edit places, sections, menu items and dishes (provisional v0.4 proposal).
    #[oai(
        path = "/entities",
        method = "post",
        tag = "ApiTags::Write",
        operation_id = "entities"
    )]
    async fn entities(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        body: Json<Value>,
    ) -> Result<BatchResp, ProblemResp> {
        st.check_ip(st.ip(req))?;
        let items = items(body.0)?;
        let mut results = Vec::with_capacity(items.len());
        for (i, es) in items.iter().enumerate() {
            let id = str_of(es, "/id").map(str::to_owned);
            let r = result(i, id, st.entity_one(es).await);
            st.record(&r);
            results.push(r);
        }
        Ok(BatchResp::Ok(Json(BatchResult { results })))
    }
}

/// POST /v1/attestations (routed from `ReadApi` so GET and POST share one path entry).
pub async fn post_attestations(
    st: &AppState,
    req: &Request,
    body: Value,
) -> Result<BatchResp, ProblemResp> {
    st.check_ip(st.ip(req))?;
    let items = items(body)?;
    let mut results = Vec::with_capacity(items.len());
    for (i, a) in items.iter().enumerate() {
        let id = str_of(a, "/id").map(str::to_owned);
        let r = result(i, id, st.attestation_one(a).await);
        st.record(&r);
        results.push(r);
    }
    Ok(BatchResp::Ok(Json(BatchResult { results })))
}
