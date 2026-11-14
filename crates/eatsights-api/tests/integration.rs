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
        // Migrations touch cluster-wide roles; run them one at a time.
        static MIGRATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        {
            let _g = MIGRATE.lock().await;
            db.migrate().await.unwrap();
        }
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

fn gunzip_lines(bytes: &[u8]) -> Vec<Value> {
    use std::io::Read;
    let mut s = String::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_string(&mut s)
        .unwrap();
    s.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn exports_are_public_consistent_and_served() {
    let Some((w, _)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("es_export_{}", ulid_lower()));
    let reg = w.state.registry;
    let entries: Vec<(String, Value)> = reg
        .iter()
        .filter_map(|p| reg.get_raw(&p.id).map(|r| (p.id.clone(), r.clone())))
        .collect();
    eatsights_db::write::sync_registry(&w.t.db.write, reg.version(), reg.hash(), &entries)
        .await
        .unwrap();
    let mut cfg = w.state.cfg.clone();
    cfg.export_dir = dir.clone();
    let seq = Arc::new(AtomicI64::new(0));
    let state = Arc::new(AppState::new(w.t.db.clone(), w.state.registry, cfg, seq));
    let app = eatsights_api::app(state);
    let cli = TestClient::new(&app);
    cli.get("/v1/export")
        .send()
        .await
        .assert_status(poem::http::StatusCode::NOT_FOUND);

    let mut m = None;
    for _ in 0..9 {
        m = Some(
            eatsights_worker::export::write_snapshot(&w.t.db.read, &dir, 7)
                .await
                .unwrap(),
        );
    }
    let m = m.unwrap();
    let snaps = std::fs::read_dir(&dir)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir())
        .count();
    assert_eq!(snaps, 7, "keeps the latest 7 snapshots");
    assert_eq!(
        m.changes_seq,
        eatsights_db::read::latest_seq(&w.t.db.read).await.unwrap()
    );

    let ids = [kid(&w.a), kid(&w.b), kid(&w.root)];
    for f in &m.files {
        let bytes = std::fs::read(dir.join(&m.snapshot).join(&f.name)).unwrap();
        assert_eq!(bytes.len() as u64, f.bytes);
        assert_eq!(crypto::sha256_hex(&bytes), f.sha256, "{}", f.name);
        let lines = gunzip_lines(&bytes);
        assert_eq!(lines.len() as u64, f.records, "{}", f.name);
        let text = serde_json::to_string(&lines).unwrap();
        assert!(!text.contains("\"signature\""), "{}", f.name);
        for id in &ids {
            assert!(!text.contains(id.as_str()), "{} leaks a key id", f.name);
        }
        let expect = match f.name.as_str() {
            "places.ndjson.gz"
            | "sections.ndjson.gz"
            | "menu_items.ndjson.gz"
            | "dishes.ndjson.gz"
            | "facts.ndjson.gz" => 1,
            "observations.ndjson.gz" => 2,
            "vendor_of.ndjson.gz" => 0,
            "registry.ndjson.gz" => 30,
            other => panic!("unexpected file {other}"),
        };
        assert_eq!(f.records, expect, "{}", f.name);
    }

    let r = cli.get("/v1/export").send().await;
    r.assert_status_is_ok();
    let v: Value = serde_json::from_str(&r.0.into_body().into_string().await.unwrap()).unwrap();
    assert_eq!(v["changes_seq"], m.changes_seq);
    assert_eq!(v["files"].as_array().unwrap().len(), 8);

    let r = cli.get("/v1/export/places").send().await;
    r.assert_status_is_ok();
    r.assert_content_type("application/gzip");
    let etag =
        r.0.headers()
            .get("etag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
    let body = r.0.into_body().into_vec().await.unwrap();
    assert_eq!(format!("\"{}\"", crypto::sha256_hex(&body)), etag);
    assert_eq!(gunzip_lines(&body)[0]["id"], w.place);
    cli.get("/v1/export/places.ndjson.gz")
        .header("if-none-match", &etag)
        .send()
        .await
        .assert_status(poem::http::StatusCode::NOT_MODIFIED);
    for bad in ["manifest.json", "..%2Flatest", "%2E%2E", "keys", "latest"] {
        cli.get(format!("/v1/export/{bad}"))
            .send()
            .await
            .assert_status(poem::http::StatusCode::NOT_FOUND);
    }
    let _ = std::fs::remove_dir_all(&dir);
    w.t.drop().await;
}

#[tokio::test]
async fn stale_sweep_ages_facts_without_new_observations() {
    let Some((w, app)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let db = &w.t.db;
    let reg = Registry::embedded().unwrap();
    let cfg = eatsights_worker::aggregate::AggConfig::default_config().unwrap();
    let obs = |k: &SigningKey| {
        sign(
            json!({"id": ObservationId::new().as_str(), "schema_version": "0.3", "kind": "value",
                   "property": "availability.observed",
                   "subject": {"scope": "menu_item", "place_id": w.place, "menu_item_id": w.item},
                   "value": {"state": "available"},
                   "observed_at": now(), "submitted_at": now(), "method": "in_person",
                   "source_type": "customer", "license": "CC0-1.0", "contributor": {"key_id": kid(k)}}),
            k,
        )
    };
    let r = post(&app, "/v1/submissions", json!([obs(&w.a), obs(&w.b)])).await;
    assert_eq!(r["results"][1]["status"], "accepted", "{r}");
    while eatsights_worker::runner::step(db, reg, &cfg).await.unwrap() > 0 {}
    let status = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT doc->>'status' FROM public.facts WHERE property = 'availability.observed'",
        )
        .fetch_one(&db.read)
        .await
        .unwrap()
    };
    assert_ne!(status().await, "stale");

    // Nothing is due now: no writes.
    let s0 = eatsights_db::read::latest_seq(&db.read).await.unwrap();
    assert_eq!(
        eatsights_worker::sweep::sweep(db, reg, &cfg).await.unwrap(),
        0
    );
    assert_eq!(eatsights_db::read::latest_seq(&db.read).await.unwrap(), s0);

    // 7 h later (half-life 2 h, stale after 3 half-lives) the fact goes stale.
    let later = Utc::now() + chrono::Duration::hours(7);
    let n = eatsights_worker::sweep::sweep_at(db, reg, &cfg, later)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(status().await, "stale");
    let s1 = eatsights_db::read::latest_seq(&db.read).await.unwrap();
    assert!(s1 > s0);
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT entity_type FROM public.changes WHERE seq > $1 ORDER BY seq")
            .bind(s0)
            .fetch_all(&db.read)
            .await
            .unwrap();
    assert_eq!(kinds, ["fact", "menu"]);

    // Idempotent: a second pass finds nothing to do.
    assert_eq!(
        eatsights_worker::sweep::sweep_at(db, reg, &cfg, later)
            .await
            .unwrap(),
        0
    );
    assert_eq!(eatsights_db::read::latest_seq(&db.read).await.unwrap(), s1);
    w.t.drop().await;
}

#[tokio::test]
async fn change_feed_is_an_index_only_scan() {
    let Some((w, _)) = setup().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let db = &w.t.db;
    sqlx::query("VACUUM ANALYZE public.changes")
        .execute(&db.write)
        .await
        .unwrap();
    let mut c = db.read.acquire().await.unwrap();
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *c)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN SELECT seq, entity_type, entity_id, op, at FROM public.changes WHERE seq > 3 ORDER BY seq LIMIT 50",
    )
    .fetch_all(&mut *c)
    .await
    .unwrap();
    drop(c);
    assert!(
        plan.iter()
            .any(|l| l.contains("Index Only Scan using changes_feed")),
        "{plan:?}"
    );
    w.t.drop().await;
}
