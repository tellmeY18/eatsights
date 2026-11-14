//! Integration tests against a real Postgres. Set `TEST_DATABASE_URL` (a superuser
//! connection); each test creates and drops its own database. Skipped when unset.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use chrono::{SecondsFormat, Utc};
use eatsights_api::{ApiConfig, AppState};
use eatsights_core::crypto::{self, Ed25519SigningKey as SigningKey};
use eatsights_core::ids::{
    AttestationId, DishId, EntitySubmissionId, MenuItemId, ObservationId, PlaceId, SectionId,
};
use eatsights_core::registry::Registry;
use eatsights_db::Db;
use poem::Endpoint;
use poem::test::TestClient;
use serde_json::{Value, json};
use sqlx::Connection;

struct TestDb {
    db: Db,
    admin_url: String,
    name: String,
}

impl TestDb {
    async fn new() -> Option<Self> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        let name = format!("es_test_{}", ulid_lower());
        let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&mut c)
            .await
            .unwrap();
        let (base, _) = url.rsplit_once('/').unwrap();
        let db = Db::connect(&format!("{base}/{name}"), None, 4)
            .await
            .unwrap();
        db.migrate().await.unwrap();
        Some(Self {
            db,
            admin_url: url,
            name,
        })
    }

    async fn drop(self) {
        self.db.write.close().await;
        let mut c = sqlx::PgConnection::connect(&self.admin_url).await.unwrap();
        sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.name
        ))
        .execute(&mut c)
        .await
        .unwrap();
    }
}

fn ulid_lower() -> String {
    PlaceId::new().as_str()[3..].to_lowercase()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn sign(v: Value, k: &SigningKey) -> Value {
    crypto::sign_object(&v, k).unwrap()
}

fn kid(k: &SigningKey) -> String {
    crypto::key_id(&k.verifying_key())
}

struct World {
    t: TestDb,
    state: Arc<AppState>,
    root: SigningKey,
    a: SigningKey,
    b: SigningKey,
    place: String,
    item: String,
    dish: String,
}

async fn post(app: &impl Endpoint, path: &str, body: Value) -> Value {
    let cli = TestClient::new(app);
    let r = cli.post(path).body_json(&body).send().await;
    r.0.into_body().into_json().await.unwrap()
}

async fn setup() -> Option<(World, impl Endpoint)> {
    let t = TestDb::new().await?;
    eatsights_core::schema::init().unwrap();
    let reg: &'static Registry = Registry::embedded().unwrap();
    let root = crypto::generate_signing_key();
    let (a, b) = (
        crypto::generate_signing_key(),
        crypto::generate_signing_key(),
    );
    eatsights_db::write::upsert_root_key(
        &t.db.write,
        &kid(&root),
        &crypto::encode_public_key(&root.verifying_key()),
        4,
    )
    .await
    .unwrap();
    let cfg = ApiConfig {
        pseudonym_salt: b"test-salt-test-salt-test-salt-0123456789".to_vec(),
        ..ApiConfig::default()
    };
    let seq = Arc::new(AtomicI64::new(0));
    let state = Arc::new(AppState::new(t.db.clone(), reg, cfg, seq));
    let app = eatsights_api::app(state.clone());

    for k in [&a, &b] {
        let body = sign(
            json!({"type": "key_registration", "algorithm": "ed25519",
                   "public_key": crypto::encode_public_key(&k.verifying_key()), "created_at": now()}),
            k,
        );
        let r = post(&app, "/v1/keys", body).await;
        assert_eq!(r["level"], "unverified");
    }
    let vouches: Vec<Value> = [&a, &b]
        .iter()
        .map(|k| {
            sign(
                json!({"id": AttestationId::new().as_str(), "type": "vouch", "issuer_key_id": kid(&root),
                       "subject_key_id": kid(k), "level_granted": "member", "issued_at": now()}),
                &root,
            )
        })
        .collect();
    let r = post(&app, "/v1/attestations", json!(vouches)).await;
    assert_eq!(r["results"][1]["status"], "accepted", "{r}");

    let (pl, sc, di, mi) = (
        PlaceId::new(),
        SectionId::new(),
        DishId::new(),
        MenuItemId::new(),
    );
    let es = |etype: &str, e: Value| {
        sign(
            json!({"id": EntitySubmissionId::new().as_str(), "schema_version": "0.4", "op": "create",
                   "entity_type": etype, "entity": e, "submitted_at": now(),
                   "contributor": {"key_id": kid(&a)}, "license": "CC0-1.0"}),
            &a,
        )
    };
    let batch = json!([
        es(
            "place",
            json!({"id": pl.as_str(), "names": {"en": "Test Hotel"}, "category": "small_hotel",
                           "location": {"lat": 9.98, "lng": 76.28}, "city": "ernakulam"})
        ),
        es(
            "section",
            json!({"id": sc.as_str(), "place_id": pl.as_str(), "labels": {"en": "Meals"}})
        ),
        es(
            "dish",
            json!({"id": di.as_str(), "labels": {"en": "Porotta"}})
        ),
        es(
            "menu_item",
            json!({"id": mi.as_str(), "place_id": pl.as_str(), "section_id": sc.as_str(),
                               "dish_id": di.as_str(), "labels": {"en": "Porotta"}})
        ),
    ]);
    let r = post(&app, "/v1/entities", batch).await;
    for i in 0..4 {
        assert_eq!(r["results"][i]["status"], "accepted", "{r}");
    }
    let obs = |k: &SigningKey| {
        sign(
            json!({"id": ObservationId::new().as_str(), "schema_version": "0.3", "kind": "value",
                   "property": "price.amount",
                   "subject": {"scope": "menu_item", "place_id": pl.as_str(), "menu_item_id": mi.as_str()},
                   "value": {"amount_minor": 1200, "currency": "INR", "fulfilment": "dine_in"},
                   "observed_at": now(), "submitted_at": now(), "method": "receipt",
                   "source_type": "customer", "license": "CC0-1.0", "contributor": {"key_id": kid(k)}}),
            k,
        )
    };
    let r = post(&app, "/v1/submissions", json!([obs(&a), obs(&b)])).await;
    assert_eq!(r["results"][0]["status"], "accepted", "{r}");
    assert_eq!(r["results"][1]["status"], "accepted", "{r}");

    let cfg = eatsights_worker::aggregate::AggConfig::default_config().unwrap();
    while eatsights_worker::runner::step(&t.db, reg, &cfg)
        .await
        .unwrap()
        > 0
    {}
    // Refresh the in-memory seq so caches see the worker output.
    let s = eatsights_db::read::latest_seq(&t.db.read).await.unwrap();
    state.seq.store(s, std::sync::atomic::Ordering::Release);

    Some((
        World {
            t,
            state,
            root,
            a,
            b,
            place: pl.into_string(),
            item: mi.into_string(),
            dish: di.into_string(),
        },
        app,
    ))
}

#[tokio::test]
async fn privacy_golden_and_reads() {
    let Some((w, app)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let cli = TestClient::new(&app);
    let urls = [
        "/v1/places?city=ernakulam".to_owned(),
        format!("/v1/places/{}", w.place),
        format!("/v1/places/{}/menu", w.place),
        format!("/v1/menu-items/{}", w.item),
        "/v1/dishes".to_owned(),
        format!("/v1/dishes/{}", w.dish),
        format!("/v1/facts?place_id={}", w.place),
        format!("/v1/observations?place_id={}", w.place),
        "/v1/registry/properties".to_owned(),
        "/v1/attestations?type=vendor_of".to_owned(),
        "/v1/keys/published".to_owned(),
        "/v1/changes?after=0&limit=500".to_owned(),
    ];
    let secrets = [kid(&w.a), kid(&w.b)];
    for u in &urls {
        let r = cli.get(u).send().await;
        r.assert_status_is_ok();
        let body = r.0.into_body().into_string().await.unwrap();
        for s in &secrets {
            assert!(!body.contains(s.as_str()), "{u} leaks a contributor key_id");
        }
        assert!(!body.contains("\"signature\""), "{u} leaks a signature");
        assert!(!body.contains("vouch"), "{u} leaks the vouch graph");
    }

    // Golden shape of a public observation.
    let r = cli
        .get(format!("/v1/observations?menu_item_id={}", w.item))
        .send()
        .await;
    let v: Value = r.0.into_body().into_json().await.unwrap();
    let o = &v["items"][0];
    let mut keys: Vec<&str> = o.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "contributor",
            "id",
            "kind",
            "license",
            "method",
            "observed_at",
            "property",
            "schema_version",
            "source_type",
            "subject",
            "submitted_at",
            "value"
        ]
    );
    let mut ck: Vec<&str> = o["contributor"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    ck.sort();
    assert_eq!(ck, ["class", "pseudonym"]);
    eatsights_core::schema::validate_def("observation", o).unwrap();

    // Facts are confirmed by two distinct members and valid against the schema.
    let r = cli.get(format!("/v1/menu-items/{}", w.item)).send().await;
    let v: Value = r.0.into_body().into_json().await.unwrap();
    assert_eq!(v["facts"][0]["status"], "confirmed", "{v}");
    eatsights_core::schema::validate_def("fact", &v["facts"][0]).unwrap();

    // Only the root key is published.
    let r = cli.get("/v1/keys/published").send().await;
    let v: Value = r.0.into_body().into_json().await.unwrap();
    assert_eq!(v["items"][0]["key_id"], kid(&w.root));

    // ETag / 304.
    let r = cli.get(format!("/v1/places/{}/menu", w.place)).send().await;
    let etag =
        r.0.headers()
            .get("etag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
    let r = cli
        .get(format!("/v1/places/{}/menu", w.place))
        .header("If-None-Match", etag)
        .send()
        .await;
    r.assert_status(poem::http::StatusCode::NOT_MODIFIED);

    // Problem+json for an unknown place.
    let r = cli
        .get("/v1/places/pl_00000000000000000000000000")
        .send()
        .await;
    r.assert_status(poem::http::StatusCode::NOT_FOUND);
    r.assert_content_type("application/problem+json");

    let _ = &w.state;
    w.t.drop().await;
}

#[tokio::test]
async fn append_only_and_redaction() {
    let Some((w, _app)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let pool = &w.t.db.write;
    let id: String = sqlx::query_scalar("SELECT id FROM public.observations LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap();

    // Trigger blocks even the owner.
    for sql in [
        "UPDATE public.observations SET class = 'root' WHERE id = $1",
        "DELETE FROM public.observations WHERE id = $1",
        "UPDATE private.submissions SET key_id = 'x' WHERE id = $1",
        "DELETE FROM private.submissions WHERE id = $1",
    ] {
        assert!(
            sqlx::query(sql).bind(&id).execute(pool).await.is_err(),
            "{sql} succeeded"
        );
    }
    // Application roles lack the privilege.
    for role in ["eatsights_write", "eatsights_worker", "eatsights_read"] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(&format!("SET LOCAL ROLE {role}"))
            .execute(&mut *tx)
            .await
            .unwrap();
        let r = sqlx::query("DELETE FROM public.observations WHERE id = $1")
            .bind(&id)
            .execute(&mut *tx)
            .await;
        assert!(r.is_err(), "{role} could delete");
        tx.rollback().await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(&format!("SET LOCAL ROLE {role}"))
            .execute(&mut *tx)
            .await
            .unwrap();
        let r = sqlx::query("UPDATE public.observations SET class = 'x' WHERE id = $1")
            .bind(&id)
            .execute(&mut *tx)
            .await;
        assert!(r.is_err(), "{role} could update");
        tx.rollback().await.unwrap();
    }
    // The read role cannot see private data.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE eatsights_read")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        sqlx::query("SELECT 1 FROM private.keys")
            .execute(&mut *tx)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();

    // The single audited removal path.
    let ok: bool = sqlx::query_scalar("SELECT public.redact_observation($1, 'legal request')")
        .bind(&id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(ok);
    let doc: Value = sqlx::query_scalar("SELECT doc FROM public.observations WHERE id = $1")
        .bind(&id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(doc["redacted"], true);
    assert!(doc.get("value").is_none());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM public.removals WHERE entity_id = $1")
        .bind(&id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // And the trigger is back on afterwards.
    assert!(
        sqlx::query("UPDATE public.observations SET class = 'x' WHERE id = $1")
            .bind(&id)
            .execute(pool)
            .await
            .is_err()
    );
    w.t.drop().await;
}

#[tokio::test]
async fn worker_is_idempotent_and_writes_are_checked() {
    let Some((w, app)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let db = &w.t.db;
    let reg = Registry::embedded().unwrap();
    let cfg = eatsights_worker::aggregate::AggConfig::default_config().unwrap();
    // Re-running from scratch changes nothing.
    let before = eatsights_db::read::latest_seq(&db.read).await.unwrap();
    eatsights_db::work::set_cursor(&db.write, "aggregate-replay", 0)
        .await
        .unwrap();
    sqlx::query("DELETE FROM private.worker_cursors WHERE name = 'aggregate'")
        .execute(&db.write)
        .await
        .unwrap();
    while eatsights_worker::runner::step(db, reg, &cfg).await.unwrap() > 0 {}
    let after = eatsights_db::read::latest_seq(&db.read).await.unwrap();
    assert_eq!(before, after, "replaying the feed produced new changes");

    // Unknown key, tampered signature, wrong license.
    let stranger = crypto::generate_signing_key();
    let base = json!({"id": ObservationId::new().as_str(), "schema_version": "0.3", "kind": "value",
        "property": "price.amount",
        "subject": {"scope": "menu_item", "place_id": w.place, "menu_item_id": w.item},
        "value": {"amount_minor": 1, "currency": "INR", "fulfilment": "dine_in"},
        "observed_at": now(), "submitted_at": now(), "method": "receipt",
        "source_type": "customer", "license": "CC0-1.0"});
    let mut s1 = base.clone();
    s1["contributor"] = json!({"key_id": kid(&stranger)});
    let s1 = sign(s1, &stranger);
    let mut s2 = base.clone();
    s2["contributor"] = json!({"key_id": kid(&w.a)});
    let mut s2 = sign(s2, &w.a);
    s2["value"]["amount_minor"] = json!(2);
    let mut s3 = base.clone();
    s3["id"] = json!(ObservationId::new().as_str());
    s3["license"] = json!("CC-BY-4.0");
    s3["contributor"] = json!({"key_id": kid(&w.a)});
    let s3 = sign(s3, &w.a);
    let mut s4 = base.clone();
    s4["id"] = json!(ObservationId::new().as_str());
    s4["source_type"] = json!("vendor");
    s4["contributor"] = json!({"key_id": kid(&w.b)});
    let s4 = sign(s4, &w.b);
    let r = post(&app, "/v1/submissions", json!([s1, s2, s3, s4])).await;
    let statuses: Vec<_> = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["problem"]["status"].as_u64())
        .collect();
    assert_eq!(
        statuses,
        [Some(401), Some(401), Some(422), Some(403)],
        "{r}"
    );

    // An unvouched key cannot edit; a member can.
    let edit = |k: &SigningKey, name: &str| {
        sign(
            json!({"id": EntitySubmissionId::new().as_str(), "schema_version": "0.4", "op": "edit",
                   "entity_type": "dish", "entity": {"id": w.dish, "labels": {"en": name}},
                   "submitted_at": now(), "contributor": {"key_id": kid(k)}, "license": "CC0-1.0"}),
            k,
        )
    };
    let c = crypto::generate_signing_key();
    let reg_body = sign(
        json!({"type": "key_registration", "algorithm": "ed25519",
               "public_key": crypto::encode_public_key(&c.verifying_key()), "created_at": now()}),
        &c,
    );
    post(&app, "/v1/keys", reg_body).await;
    let r = post(
        &app,
        "/v1/entities",
        json!([edit(&c, "X"), edit(&w.b, "Parotta")]),
    )
    .await;
    assert_eq!(r["results"][0]["problem"]["status"], 403, "{r}");
    assert_eq!(r["results"][1]["status"], "accepted", "{r}");

    // Batch limit.
    let big: Vec<Value> = (0..101).map(|_| json!({})).collect();
    let cli = TestClient::new(&app);
    let r = cli
        .post("/v1/submissions")
        .body_json(&json!(big))
        .send()
        .await;
    r.assert_status(poem::http::StatusCode::PAYLOAD_TOO_LARGE);
    w.t.drop().await;
}

// ------------------------------------------------------------------ trust (M4)

async fn new_key(app: &impl Endpoint) -> SigningKey {
    let k = crypto::generate_signing_key();
    let body = sign(
        json!({"type": "key_registration", "algorithm": "ed25519",
               "public_key": crypto::encode_public_key(&k.verifying_key()), "created_at": now()}),
        &k,
    );
    let r = post(app, "/v1/keys", body).await;
    assert_eq!(r["level"], "unverified", "{r}");
    k
}

fn att(issuer: &SigningKey, kind: &str, subject: &str, extra: Value) -> Value {
    let mut v = json!({"id": AttestationId::new().as_str(), "type": kind,
        "issuer_key_id": kid(issuer), "subject_key_id": subject, "issued_at": now()});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    sign(v, issuer)
}

fn vouch(issuer: &SigningKey, subject: &SigningKey, level: &str) -> Value {
    att(
        issuer,
        "vouch",
        &kid(subject),
        json!({"level_granted": level}),
    )
}

/// Post one attestation; returns (status, problem slug).
async fn attest(app: &impl Endpoint, a: &Value) -> (String, String) {
    let r = post(app, "/v1/attestations", json!([a])).await;
    let x = &r["results"][0];
    (
        x["status"].as_str().unwrap_or_default().to_owned(),
        x["problem"]["type"]
            .as_str()
            .unwrap_or_default()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned(),
    )
}

async fn standing(w: &World, k: &SigningKey) -> (String, Option<i32>, String) {
    let k = eatsights_db::write::get_key(&w.t.db.write, &kid(k))
        .await
        .unwrap()
        .unwrap();
    (k.level, k.depth, k.status)
}

async fn sync_seq(w: &World) {
    let s = eatsights_db::read::latest_seq(&w.t.db.read).await.unwrap();
    w.state.seq.store(s, std::sync::atomic::Ordering::Release);
}

async fn published(w: &World, app: &impl Endpoint) -> Vec<String> {
    sync_seq(w).await;
    let cli = TestClient::new(app);
    let v: Value = cli
        .get("/v1/keys/published")
        .send()
        .await
        .0
        .into_body()
        .into_json()
        .await
        .unwrap();
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["key_id"].as_str().unwrap().to_owned())
        .collect()
}

fn accepted(r: (String, String)) {
    assert_eq!(r.0, "accepted", "{r:?}");
}

fn rejected(r: (String, String), slug: &str) {
    assert_eq!((r.0.as_str(), r.1.as_str()), ("rejected", slug));
}

#[tokio::test]
async fn trust_vouch_rules_and_depth() {
    let Some((w, app)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let v = new_key(&app).await;
    let v2 = new_key(&app).await;
    let t = new_key(&app).await;
    let m = new_key(&app).await;
    let x = new_key(&app).await;
    accepted(attest(&app, &vouch(&w.root, &v, "verifier")).await);
    assert_eq!(standing(&w, &v).await.0, "verifier");
    assert!(published(&w, &app).await.contains(&kid(&v)));
    // verifier by root only; issuer strictly above
    rejected(attest(&app, &vouch(&v, &v2, "verifier")).await, "forbidden");
    accepted(attest(&app, &vouch(&v, &t, "trusted")).await);
    rejected(attest(&app, &vouch(&t, &x, "trusted")).await, "forbidden");
    accepted(attest(&app, &vouch(&t, &m, "member")).await);
    assert_eq!(
        standing(&w, &m).await,
        ("member".into(), Some(3), "active".into())
    );
    // members (w.a) cannot vouch; unverified cannot either
    rejected(attest(&app, &vouch(&w.a, &x, "member")).await, "forbidden");
    rejected(attest(&app, &vouch(&x, &w.a, "member")).await, "forbidden");
    // a vouch never lowers: root grants v2 verifier, then v-level trusted vouch keeps it
    accepted(attest(&app, &vouch(&w.root, &v2, "verifier")).await);
    accepted(attest(&app, &vouch(&v, &v2, "trusted")).await);
    assert_eq!(standing(&w, &v2).await.0, "verifier");
    // a lower path does not deepen: t keeps the shortest chain at its level
    accepted(attest(&app, &vouch(&w.root, &t, "trusted")).await);
    assert_eq!(standing(&w, &t).await.1, Some(1));

    // Depth limit 2: a depth-2 trusted issuer cannot vouch (subject depth 3).
    let reg = Registry::embedded().unwrap();
    let mut cfg = w.state.cfg.clone();
    cfg.trust.max_depth = 2;
    let shallow = eatsights_api::app(Arc::new(AppState::new(
        w.t.db.clone(),
        reg,
        cfg,
        Arc::new(AtomicI64::new(0)),
    )));
    let t2 = new_key(&app).await;
    accepted(attest(&shallow, &vouch(&v, &t2, "trusted")).await);
    assert_eq!(standing(&w, &t2).await.1, Some(2));
    rejected(
        attest(&shallow, &vouch(&t2, &x, "member")).await,
        "depth-exceeded",
    );
    assert_eq!(standing(&w, &x).await.0, "unverified");
    w.t.drop().await;
}

#[tokio::test]
async fn trust_revoke_cascade_and_strikes() {
    let Some((w, app)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let v = new_key(&app).await;
    let t1 = new_key(&app).await;
    let t2 = new_key(&app).await;
    let m = new_key(&app).await;
    let x = new_key(&app).await;
    accepted(attest(&app, &vouch(&w.root, &v, "verifier")).await);
    let vt1 = vouch(&v, &t1, "trusted");
    let vt2 = vouch(&v, &t2, "trusted");
    accepted(attest(&app, &vt1).await);
    accepted(attest(&app, &vt2).await);
    accepted(attest(&app, &vouch(&t1, &m, "member")).await);
    let revoke = |by: &SigningKey, target: &Value| {
        att(
            by,
            "revoke",
            target["subject_key_id"].as_str().unwrap(),
            json!({"revokes": target["id"]}),
        )
    };
    // a member may not revoke someone else's vouch; subject must match
    rejected(attest(&app, &revoke(&w.a, &vt1)).await, "forbidden");
    let mut wrong = revoke(&w.root, &vt1);
    wrong["subject_key_id"] = json!(kid(&t2));
    let wrong = sign(
        {
            let mut o = wrong.clone();
            o.as_object_mut().unwrap().remove("signature");
            o
        },
        &w.root,
    );
    rejected(attest(&app, &wrong).await, "subject-mismatch");
    // root revokes v's vouch for t1: strike 1, t1 and its member m lose standing
    accepted(attest(&app, &revoke(&w.root, &vt1)).await);
    assert_eq!(
        standing(&w, &t1).await,
        ("unverified".into(), None, "active".into())
    );
    assert_eq!(standing(&w, &m).await.0, "unverified");
    assert_eq!(standing(&w, &v).await.0, "verifier");
    rejected(
        attest(&app, &revoke(&w.root, &vt1)).await,
        "already-revoked",
    );
    // second strike: v demoted to trusted, unpublished, in cooldown
    accepted(attest(&app, &revoke(&w.root, &vt2)).await);
    assert_eq!(standing(&w, &v).await.0, "trusted");
    assert!(!published(&w, &app).await.contains(&kid(&v)));
    rejected(
        attest(&app, &vouch(&v, &x, "member")).await,
        "vouch-cooldown",
    );
    let strikes: i64 = sqlx::query_scalar("SELECT count(*) FROM private.strikes WHERE key_id = $1")
        .bind(kid(&v))
        .fetch_one(&w.t.db.write)
        .await
        .unwrap();
    assert_eq!(strikes, 2);
    // an issuer revoking its own vouch is not a strike
    let own = vouch(&w.root, &x, "member");
    accepted(attest(&app, &own).await);
    accepted(attest(&app, &revoke(&w.root, &own)).await);
    assert_eq!(standing(&w, &x).await.0, "unverified");
    // a new root vouch issued after the demotion lifts the cap
    accepted(attest(&app, &vouch(&w.root, &v, "verifier")).await);
    assert_eq!(standing(&w, &v).await.0, "verifier");
    w.t.drop().await;
}

#[tokio::test]
async fn trust_vendor_and_rotate() {
    let Some((w, app)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let cli = TestClient::new(&app);
    let v = new_key(&app).await;
    let vendor = new_key(&app).await;
    accepted(attest(&app, &vouch(&w.root, &v, "verifier")).await);
    let place = json!({"place_id": w.place});
    // members cannot bind; expiry beyond 365 days is rejected
    rejected(
        attest(&app, &att(&w.a, "vendor_of", &kid(&vendor), place.clone())).await,
        "forbidden",
    );
    let far = (Utc::now() + chrono::Duration::days(400)).to_rfc3339_opts(SecondsFormat::Secs, true);
    rejected(
        attest(
            &app,
            &att(
                &v,
                "vendor_of",
                &kid(&vendor),
                json!({"place_id": w.place, "expires_at": far}),
            ),
        )
        .await,
        "invalid-expiry",
    );
    let binding = att(&v, "vendor_of", &kid(&vendor), place.clone());
    accepted(attest(&app, &binding).await);
    let list = |q: &str| {
        let u = format!("/v1/attestations?type=vendor_of&place_id={}{q}", w.place);
        let cli = &cli;
        let w = &w;
        async move {
            sync_seq(w).await;
            let v: Value = cli
                .get(u)
                .send()
                .await
                .0
                .into_body()
                .into_json()
                .await
                .unwrap();
            v["items"].as_array().unwrap().clone()
        }
    };
    let items = list("").await;
    assert_eq!(items.len(), 1);
    assert!(items[0]["expires_at"].is_string(), "default expiry is set");
    assert_eq!(items[0]["active"], true);

    let vobs = |k: &SigningKey| {
        sign(
            json!({"id": ObservationId::new().as_str(), "schema_version": "0.3", "kind": "value",
                   "property": "price.amount",
                   "subject": {"scope": "menu_item", "place_id": w.place, "menu_item_id": w.item},
                   "value": {"amount_minor": 1300, "currency": "INR", "fulfilment": "dine_in"},
                   "observed_at": now(), "submitted_at": now(), "method": "in_person",
                   "source_type": "vendor", "license": "CC0-1.0", "contributor": {"key_id": kid(k)}}),
            k,
        )
    };
    let o = vobs(&vendor);
    let r = post(&app, "/v1/submissions", json!([o])).await;
    assert_eq!(r["results"][0]["status"], "accepted", "{r}");
    let r = cli
        .get(format!("/v1/observations?menu_item_id={}", w.item))
        .send()
        .await;
    let all: Value = r.0.into_body().into_json().await.unwrap();
    assert!(
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == o["id"] && x["contributor"]["class"] == "vendor")
    );

    // rotate the vendor key: binding follows, old key is rejected
    let vendor2 = new_key(&app).await;
    rejected(
        attest(&app, &att(&vendor, "rotate", &kid(&w.a), json!({}))).await,
        "subject-not-fresh",
    );
    accepted(attest(&app, &att(&vendor, "rotate", &kid(&vendor2), json!({}))).await);
    assert_eq!(standing(&w, &vendor).await.2, "rotated");
    let r = post(&app, "/v1/submissions", json!([vobs(&vendor)])).await;
    assert_eq!(r["results"][0]["problem"]["status"], 403, "{r}");
    let r = post(&app, "/v1/submissions", json!([vobs(&vendor2)])).await;
    assert_eq!(r["results"][0]["status"], "accepted", "{r}");
    assert_eq!(list("").await[0]["holder_key_id"], kid(&vendor2));

    // revoke the binding: vendor submissions are rejected; listing hides it by default
    accepted(
        attest(
            &app,
            &att(
                &v,
                "revoke",
                &kid(&vendor),
                json!({"revokes": binding["id"]}),
            ),
        )
        .await,
    );
    let r = post(&app, "/v1/submissions", json!([vobs(&vendor2)])).await;
    assert_eq!(
        r["results"][0]["problem"]["type"]
            .as_str()
            .map(|t| t.ends_with("not-vendor")),
        Some(true),
        "{r}"
    );
    assert!(list("").await.is_empty());
    let inactive = list("&include_inactive=true").await;
    assert_eq!(inactive.len(), 1);
    assert_eq!(inactive[0]["active"], false);

    // a member rotates: the new key inherits level and depth; a verifier's
    // successor is published instead of the old key
    let a2 = new_key(&app).await;
    accepted(attest(&app, &att(&w.a, "rotate", &kid(&a2), json!({}))).await);
    assert_eq!(
        standing(&w, &a2).await,
        ("member".into(), Some(1), "active".into())
    );
    let v2 = new_key(&app).await;
    accepted(attest(&app, &att(&v, "rotate", &kid(&v2), json!({}))).await);
    let pubs = published(&w, &app).await;
    assert!(pubs.contains(&kid(&v2)) && !pubs.contains(&kid(&v)));
    // root cannot rotate by attestation
    let r3 = new_key(&app).await;
    rejected(
        attest(&app, &att(&w.root, "rotate", &kid(&r3), json!({}))).await,
        "root-rotation",
    );

    // privacy: no vouch/revoke/rotate graph, no member keys, no signatures in public reads
    let s = eatsights_db::read::latest_seq(&w.t.db.read).await.unwrap();
    w.state.seq.store(s, std::sync::atomic::Ordering::Release);
    let urls = [
        "/v1/attestations?type=vendor_of&include_inactive=true".to_owned(),
        "/v1/keys/published".to_owned(),
        "/v1/changes?after=0&limit=500".to_owned(),
        format!("/v1/observations?place_id={}", w.place),
    ];
    for u in &urls {
        let body = cli
            .get(u)
            .send()
            .await
            .0
            .into_body()
            .into_string()
            .await
            .unwrap();
        for secret in [kid(&w.a), kid(&a2), kid(&w.b)] {
            assert!(!body.contains(secret.as_str()), "{u} leaks a member key id");
        }
        for word in ["\"signature\"", "vouch", "rotate", "\"revokes\"", "strike"] {
            assert!(!body.contains(word), "{u} leaks {word}");
        }
    }
    let r = cli.get("/v1/attestations?type=vouch").send().await;
    r.assert_status(poem::http::StatusCode::BAD_REQUEST);
    w.t.drop().await;
}
