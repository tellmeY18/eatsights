//! M5 integration tests: admin signatures and replay, merges, duplicates, removals,
//! `ready_to_map` and the OSM checker against a local mock Overpass server.
//! Needs `TEST_DATABASE_URL` (superuser); skipped when unset.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use eatsights_api::{ApiConfig, AppState};
use eatsights_core::crypto::{self, Ed25519SigningKey as SigningKey};
use eatsights_core::ids::{AttestationId, EntitySubmissionId, MenuItemId, ObservationId, PlaceId};
use eatsights_core::registry::Registry;
use eatsights_db::Db;
use eatsights_worker::{dedupe, osm};
use poem::Endpoint;
use poem::test::TestClient;
use serde_json::{Value, json};
use sqlx::Connection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct World {
    db: Db,
    admin_url: String,
    name: String,
    reg: &'static Registry,
    state: Arc<AppState>,
    root: SigningKey,
    a: SigningKey,
    b: SigningKey,
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

async fn send(app: &impl Endpoint, path: &str, body: &Value) -> (u16, Value) {
    let cli = TestClient::new(app);
    let r = cli.post(path).body_json(body).send().await;
    let status = r.0.status().as_u16();
    let v = r.0.into_body().into_json().await.unwrap_or(Value::Null);
    (status, v)
}

async fn get(app: &impl Endpoint, path: &str) -> (u16, Option<String>, Value) {
    let cli = TestClient::new(app);
    let r = cli.get(path).send().await;
    let status = r.0.status().as_u16();
    let loc =
        r.0.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
    let v = r.0.into_body().into_json().await.unwrap_or(Value::Null);
    (status, loc, v)
}

impl World {
    async fn new() -> Option<(Self, impl Endpoint)> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        let name = format!("es_m5_{}", PlaceId::new().as_str()[3..].to_lowercase());
        let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&mut c)
            .await
            .unwrap();
        let (base, _) = url.rsplit_once('/').unwrap();
        let db = Db::connect(&format!("{base}/{name}"), None, 4)
            .await
            .unwrap();
        {
            // Role creation in the first migration is cluster-wide; do not race it.
            static MIGRATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let _g = MIGRATE.lock().await;
            db.migrate().await.unwrap();
        }
        eatsights_core::schema::init().unwrap();
        let reg: &'static Registry = Registry::embedded().unwrap();
        let root = crypto::generate_signing_key();
        let (a, b) = (
            crypto::generate_signing_key(),
            crypto::generate_signing_key(),
        );
        eatsights_db::write::upsert_root_key(
            &db.write,
            &kid(&root),
            &crypto::encode_public_key(&root.verifying_key()),
        )
        .await
        .unwrap();
        let cfg = ApiConfig {
            pseudonym_salt: b"test-salt-test-salt-test-salt-0123456789".to_vec(),
            ..ApiConfig::default()
        };
        let state = Arc::new(AppState::new(
            db.clone(),
            reg,
            cfg,
            Arc::new(AtomicI64::new(0)),
        ));
        let app = eatsights_api::app(state.clone());
        for k in [&a, &b] {
            let body = sign(
                json!({"type": "key_registration", "algorithm": "ed25519",
                       "public_key": crypto::encode_public_key(&k.verifying_key()), "created_at": now()}),
                k,
            );
            send(&app, "/v1/keys", &body).await;
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
        let (_, r) = send(&app, "/v1/attestations", &json!(vouches)).await;
        assert_eq!(r["results"][1]["status"], "accepted", "{r}");
        Some((
            Self {
                db,
                admin_url: url,
                name,
                reg,
                state,
                root,
                a,
                b,
            },
            app,
        ))
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

    async fn sync_seq(&self) {
        let s = eatsights_db::read::latest_seq(&self.db.read).await.unwrap();
        self.state.seq.store(s, Ordering::Release);
    }

    async fn work(&self) {
        let cfg = eatsights_worker::aggregate::AggConfig::default_config().unwrap();
        while eatsights_worker::runner::step(&self.db, self.reg, &cfg)
            .await
            .unwrap()
            > 0
        {}
        while osm::ready_step(&self.db, true).await.unwrap() > 0 {}
        self.sync_seq().await;
    }

    async fn place(
        &self,
        app: &impl Endpoint,
        name: &str,
        cat: &str,
        lat: f64,
        lng: f64,
    ) -> String {
        let id = PlaceId::new().into_string();
        self.entity(
            app,
            "place",
            json!({"id": id, "names": {"en": name}, "category": cat,
                   "location": {"lat": lat, "lng": lng}, "city": "ernakulam"}),
        )
        .await;
        id
    }

    async fn entity(&self, app: &impl Endpoint, etype: &str, e: Value) {
        let es = sign(
            json!({"id": EntitySubmissionId::new().as_str(), "schema_version": "0.4", "op": "create",
                   "entity_type": etype, "entity": e, "submitted_at": now(),
                   "contributor": {"key_id": kid(&self.a)}, "license": "CC0-1.0"}),
            &self.a,
        );
        let (_, r) = send(app, "/v1/entities", &json!([es])).await;
        assert_eq!(r["results"][0]["status"], "accepted", "{r}");
    }

    /// `status.operating = active` on a place from key `k`. Returns the observation id.
    async fn operating(&self, app: &impl Endpoint, place: &str, k: &SigningKey) -> String {
        let id = ObservationId::new().into_string();
        let o = sign(
            json!({"id": id, "schema_version": "0.3", "kind": "value", "property": "status.operating",
                   "subject": {"scope": "place", "place_id": place}, "value": {"state": "active"},
                   "observed_at": now(), "submitted_at": now(), "method": "in_person",
                   "source_type": "customer", "license": "CC0-1.0", "contributor": {"key_id": kid(k)}}),
            k,
        );
        let (_, r) = send(app, "/v1/submissions", &json!([o])).await;
        assert_eq!(r["results"][0]["status"], "accepted", "{r}");
        id
    }

    fn admin(&self, action: &str, params: Value) -> Value {
        sign(
            json!({"id": EntitySubmissionId::new().as_str(), "action": action, "params": params,
                   "issued_at": now(), "issuer_key_id": kid(&self.root)}),
            &self.root,
        )
    }
}

async fn fact(db: &Db, key: &str, prop: &str) -> Option<Value> {
    sqlx::query_scalar("SELECT doc FROM public.facts WHERE subject_key = $1 AND property = $2")
        .bind(key)
        .bind(prop)
        .fetch_optional(&db.read)
        .await
        .unwrap()
}

#[tokio::test]
async fn admin_signature_and_replay() {
    let Some((w, app)) = World::new().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let p = w.place(&app, "Replay Cafe", "cafe", 9.95, 76.25).await;
    let params = json!({"place_id": p, "osm_id": "node/42"});

    // Ordinary member key: forbidden.
    let by_member = sign(
        json!({"id": EntitySubmissionId::new().as_str(), "action": "link_osm", "params": params,
               "issued_at": now(), "issuer_key_id": kid(&w.a)}),
        &w.a,
    );
    let (s, r) = send(&app, "/v1/admin/osm-links", &by_member).await;
    assert_eq!(s, 403, "{r}");

    // Tampered.
    let mut t = w.admin("link_osm", params.clone());
    t["params"]["osm_id"] = json!("node/43");
    let (s, _) = send(&app, "/v1/admin/osm-links", &t).await;
    assert_eq!(s, 401);

    // Outside the time window.
    let old = sign(
        json!({"id": EntitySubmissionId::new().as_str(), "action": "link_osm", "params": params,
               "issued_at": "2026-01-01T00:00:00Z", "issuer_key_id": kid(&w.root)}),
        &w.root,
    );
    let (s, _) = send(&app, "/v1/admin/osm-links", &old).await;
    assert_eq!(s, 422);

    // Wrong action for the endpoint, unknown field.
    let (s, _) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("link_osm", params.clone()),
    )
    .await;
    assert_eq!(s, 422);
    let mut extra = w.admin("link_osm", params.clone());
    extra["x"] = json!(1);
    let (s, _) = send(&app, "/v1/admin/osm-links", &extra).await;
    assert_eq!(s, 422);

    // Accepted, then an identical resend is a duplicate, a different body with the id is a replay.
    let ok = w.admin("link_osm", params.clone());
    let (s, r) = send(&app, "/v1/admin/osm-links", &ok).await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["status"], "accepted");
    assert_eq!(r["result"]["osm"]["state"], "linked");
    let (s, r) = send(&app, "/v1/admin/osm-links", &ok).await;
    assert_eq!((s, r["status"].as_str()), (200, Some("duplicate")));
    let mut other = ok.clone();
    other["params"]["osm_id"] = json!("way/7");
    let other = sign(
        {
            let mut o = other;
            o.as_object_mut().unwrap().remove("signature");
            o
        },
        &w.root,
    );
    let (s, _) = send(&app, "/v1/admin/osm-links", &other).await;
    assert_eq!(s, 409);

    w.sync_seq().await;
    let (_, _, doc) = get(&app, &format!("/v1/places/{p}")).await;
    assert_eq!(doc["osm"]["state"], "linked");
    assert_eq!(doc["osm"]["id"], "node/42");
    eatsights_core::schema::validate_def("place", &doc).unwrap();

    // The full signed object is kept privately; nothing about it is public except the change.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM private.admin_actions")
        .fetch_one(&w.db.read)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let (_, _, feed) = get(&app, "/v1/changes?after=0&limit=500").await;
    assert!(!feed.to_string().contains("\"signature\""));
    w.drop().await;
}

#[tokio::test]
async fn merge_undo_and_cycles() {
    let Some((w, app)) = World::new().await else {
        return;
    };
    let p1 = w.place(&app, "Merge One", "cafe", 9.95, 76.25).await;
    let p2 = w.place(&app, "Merge Two", "cafe", 9.95, 76.25).await;
    let p3 = w.place(&app, "Merge Three", "cafe", 9.95, 76.25).await;
    w.operating(&app, &p2, &w.a).await;
    w.operating(&app, &p2, &w.b).await;
    w.work().await;
    assert!(
        fact(&w.db, &format!("place:{p2}"), "status.operating")
            .await
            .is_some()
    );

    // Default survivor: the older record (p1).
    let (s, r) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("merge", json!({"a_id": p2, "b_id": p1})),
    )
    .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["result"]["merged"], p2);
    assert_eq!(r["result"]["merged_into"], p1);
    w.work().await;
    let (s, loc, _) = get(&app, &format!("/v1/places/{p2}")).await;
    assert_eq!(
        (s, loc.as_deref()),
        (301, Some(format!("/v1/places/{p1}").as_str()))
    );
    let (_, _, merged) = get(&app, &format!("/v1/places/{p2}?follow=false")).await;
    eatsights_core::schema::validate_def("place", &merged).unwrap();
    assert_eq!(merged["record_state"], "merged");
    // Aggregation follows the redirect.
    let f = fact(&w.db, &format!("place:{p1}"), "status.operating")
        .await
        .unwrap();
    assert_eq!(f["status"], "confirmed");
    assert!(
        fact(&w.db, &format!("place:{p2}"), "status.operating")
            .await
            .is_none()
    );

    // Merging an already merged loser is refused.
    let (s, _) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("merge", json!({"a_id": p2, "b_id": p3, "survivor_id": p3})),
    )
    .await;
    assert_eq!(s, 409);
    // p1 into p3 (explicit survivor): p2 is re-pointed to p3 (no chains).
    let (s, r) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("merge", json!({"a_id": p1, "b_id": p3, "survivor_id": p3})),
    )
    .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["result"]["repointed"], json!([p2]));
    w.work().await;
    let (_, loc, _) = get(&app, &format!("/v1/places/{p2}")).await;
    assert_eq!(loc.as_deref(), Some(format!("/v1/places/{p3}").as_str()));
    assert!(
        fact(&w.db, &format!("place:{p3}"), "status.operating")
            .await
            .is_some()
    );
    assert!(
        fact(&w.db, &format!("place:{p1}"), "status.operating")
            .await
            .is_none()
    );

    // Cycle: merging p3 into p1 (which redirects to p3) is refused.
    let (s, r) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("merge", json!({"a_id": p3, "b_id": p1, "survivor_id": p1})),
    )
    .await;
    assert_eq!(s, 409, "{r}");

    // Undo p2: active again, its facts come back, the survivor loses them.
    let (s, r) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("unmerge", json!({"id": p2})),
    )
    .await;
    assert_eq!(s, 200, "{r}");
    w.work().await;
    let (s, _, doc) = get(&app, &format!("/v1/places/{p2}")).await;
    assert_eq!(s, 200);
    assert_eq!(doc["record_state"], "active");
    assert!(doc.get("merged_into").is_none());
    assert!(
        fact(&w.db, &format!("place:{p2}"), "status.operating")
            .await
            .is_some()
    );
    assert!(
        fact(&w.db, &format!("place:{p3}"), "status.operating")
            .await
            .is_none()
    );
    let (s, _) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("unmerge", json!({"id": p2})),
    )
    .await;
    assert_eq!(s, 409);

    // Menu items merge within a place only.
    let (m1, m2, m3) = (MenuItemId::new(), MenuItemId::new(), MenuItemId::new());
    for (m, pl) in [(&m1, &p2), (&m2, &p2), (&m3, &p3)] {
        w.entity(
            &app,
            "menu_item",
            json!({"id": m.as_str(), "place_id": pl, "labels": {"en": "Chai"}}),
        )
        .await;
    }
    let (s, r) = send(
        &app,
        "/v1/admin/merges",
        &w.admin(
            "merge",
            json!({"entity_type": "menu_item", "a_id": m1.as_str(), "b_id": m3.as_str()}),
        ),
    )
    .await;
    assert_eq!(s, 409, "{r}");
    let (s, r) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("merge", json!({"entity_type": "menu_item", "a_id": m2.as_str(), "b_id": m1.as_str(), "survivor_id": m1.as_str()})),
    )
    .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["result"]["merged_into"], m1.as_str());
    let doc: Value = sqlx::query_scalar("SELECT doc FROM public.menu_items WHERE id = $1")
        .bind(m2.as_str())
        .fetch_one(&w.db.read)
        .await
        .unwrap();
    eatsights_core::schema::validate_def("menu_item", &doc).unwrap();
    assert_eq!(doc["merged_into"], m1.as_str());

    // Nothing was deleted.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM public.places")
        .fetch_one(&w.db.read)
        .await
        .unwrap();
    assert_eq!(n, 3);
    w.drop().await;
}

#[tokio::test]
async fn duplicate_detector_and_suggestions() {
    let Some((w, app)) = World::new().await else {
        return;
    };
    // ~22 m apart, similar names: suggested.
    let a = w
        .place(&app, "Ammini Chaya Kada", "tea_stall", 9.9312, 76.2673)
        .await;
    let b = w
        .place(&app, "Ammini Chaaya Kada", "tea_stall", 9.9314, 76.2673)
        .await;
    // ~111 m away, different name: not suggested.
    let _c = w
        .place(&app, "Biryani Point", "tea_stall", 9.9322, 76.2673)
        .await;
    // ~111 m, identical name (>= 0.7): suggested with b and a.
    let d = w
        .place(&app, "Ammini Chaya Kada", "tea_stall", 9.9322, 76.2673)
        .await;
    // Same spot, different category: not suggested.
    let _e = w
        .place(&app, "Ammini Chaya Kada", "bakery", 9.9312, 76.2673)
        .await;
    // Far away: not suggested.
    let _f = w
        .place(&app, "Ammini Chaya Kada", "tea_stall", 9.9512, 76.2673)
        .await;

    let cfg = dedupe::DedupeConfig::default();
    while dedupe::step(&w.db, &cfg).await.unwrap() > 0 {}
    // Idempotent.
    while dedupe::step(&w.db, &cfg).await.unwrap() > 0 {}
    let s = eatsights_db::osm::list_suggestions(&w.db.read, Some("open"), 100)
        .await
        .unwrap();
    let pairs: Vec<(String, String)> = s.iter().map(|s| (s.a_id.clone(), s.b_id.clone())).collect();
    let pair = |x: &str, y: &str| {
        if x < y {
            (x.to_owned(), y.to_owned())
        } else {
            (y.to_owned(), x.to_owned())
        }
    };
    assert!(pairs.contains(&pair(&a, &b)), "{pairs:?}");
    assert!(pairs.contains(&pair(&a, &d)), "{pairs:?}");
    assert_eq!(pairs.len(), 3, "{pairs:?}"); // a-b, a-d, b-d
    // Never auto-merged.
    let merged: i64 =
        sqlx::query_scalar("SELECT count(*) FROM public.places WHERE record_state = 'merged'")
            .fetch_one(&w.db.read)
            .await
            .unwrap();
    assert_eq!(merged, 0);

    // Confirm one by suggestion id, dismiss another.
    let ab = s
        .iter()
        .find(|x| (x.a_id.clone(), x.b_id.clone()) == pair(&a, &b))
        .unwrap();
    let ad = s
        .iter()
        .find(|x| (x.a_id.clone(), x.b_id.clone()) == pair(&a, &d))
        .unwrap();
    let (st, r) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("merge", json!({"suggestion_id": ab.id})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, _) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("dismiss", json!({"suggestion_id": ad.id})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = send(
        &app,
        "/v1/admin/merges",
        &w.admin("dismiss", json!({"suggestion_id": ad.id})),
    )
    .await;
    assert_eq!(st, 409);
    let s = eatsights_db::osm::get_suggestion(&w.db.read, ab.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.status, "confirmed");

    // Suggestions are private: no public endpoint shows them.
    w.sync_seq().await;
    for u in ["/v1/changes?after=0&limit=500", "/v1/places?city=ernakulam"] {
        let (_, _, v) = get(&app, u).await;
        assert!(!v.to_string().contains("suggestion"));
    }
    w.drop().await;
}

#[tokio::test]
async fn removal_and_ready_to_map() {
    let Some((w, app)) = World::new().await else {
        return;
    };
    let p = w.place(&app, "Ready Kada", "tea_stall", 9.97, 76.29).await;
    let o1 = w.operating(&app, &p, &w.a).await;
    w.operating(&app, &p, &w.b).await;
    w.work().await;
    // unchecked: not ready.
    let (_, _, doc) = get(&app, &format!("/v1/places/{p}")).await;
    assert!(doc["osm"].get("ready_to_map").is_none());
    let unmapped = json!({"state": "unmapped", "checked_at": now()});
    assert!(
        eatsights_db::osm::set_osm(&w.db.write, &p, &doc["osm"], &unmapped)
            .await
            .unwrap()
    );
    w.work().await;
    let (_, _, doc) = get(&app, &format!("/v1/places/{p}")).await;
    assert_eq!(doc["osm"]["ready_to_map"], true, "{doc}");
    eatsights_core::schema::validate_def("place", &doc).unwrap();

    // Removal of one confirmation: the fact is no longer confirmed, so not ready.
    let (s, r) = send(
        &app,
        "/v1/admin/removals",
        &w.admin(
            "remove_observation",
            json!({"observation_id": o1, "reason": "spam"}),
        ),
    )
    .await;
    assert_eq!(s, 422, "{r}");
    let (s, r) = send(
        &app,
        "/v1/admin/removals",
        &w.admin(
            "remove_observation",
            json!({"observation_id": o1, "reason": "legal"}),
        ),
    )
    .await;
    assert_eq!(s, 200, "{r}");
    let (s, _) = send(
        &app,
        "/v1/admin/removals",
        &w.admin(
            "remove_observation",
            json!({"observation_id": o1, "reason": "legal"}),
        ),
    )
    .await;
    assert_eq!(s, 409);
    let (s, _) = send(
        &app,
        "/v1/admin/removals",
        &w.admin(
            "remove_observation",
            json!({"observation_id": ObservationId::new().as_str(), "reason": "safety"}),
        ),
    )
    .await;
    assert_eq!(s, 404);
    w.work().await;
    let (_, _, obs) = get(&app, &format!("/v1/observations?place_id={p}")).await;
    assert!(obs.to_string().contains("redacted"));
    let log: Vec<(String, String)> =
        sqlx::query_as("SELECT entity_id, reason FROM public.removals")
            .fetch_all(&w.db.read)
            .await
            .unwrap();
    assert_eq!(log, vec![(o1.clone(), "legal".to_owned())]);
    let (_, _, doc) = get(&app, &format!("/v1/places/{p}")).await;
    assert_eq!(doc["osm"]["ready_to_map"], false, "{doc}");
    w.drop().await;
}

// ------------------------------------------------------------------ mock Overpass

#[derive(Default)]
struct Mock {
    requests: AtomicUsize,
    /// (status, body) served in order; the last one repeats.
    replies: Mutex<Vec<(u16, String)>>,
    user_agents: Mutex<Vec<String>>,
    bodies: Mutex<Vec<String>>,
}

async fn mock_overpass(mock: Arc<Mock>) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let mock = mock.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let len = text[..i]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= i + 4 + len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let ua = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .starts_with("user-agent:")
                            .then(|| l[11..].trim().to_owned())
                    })
                    .unwrap_or_default();
                mock.user_agents.lock().unwrap().push(ua);
                mock.bodies
                    .lock()
                    .unwrap()
                    .push(text.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned());
                mock.requests.fetch_add(1, Ordering::SeqCst);
                let (code, body) = {
                    let mut r = mock.replies.lock().unwrap();
                    if r.len() > 1 {
                        r.remove(0)
                    } else {
                        r[0].clone()
                    }
                };
                let resp = format!(
                    "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    format!("http://{addr}/api/interpreter")
}

#[tokio::test]
async fn osm_checker_with_mock_overpass() {
    let Some((w, app)) = World::new().await else {
        return;
    };
    let (lat, lng) = (9.9312, 76.2673);
    let p1 = w.place(&app, "Chaya Kada", "tea_stall", lat, lng).await;
    let p2 = w.place(&app, "Nothing Here", "cafe", 10.5, 76.5).await;
    let found = json!({"elements": [
        {"type": "node", "id": 101, "lat": 9.93125, "lon": 76.26731,
         "tags": {"name": "Chaya Kada Secret", "name:ml": "ചായക്കട", "amenity": "cafe", "opening_hours": "Mo-Su 06:00-21:00"}},
        {"type": "way", "id": 202, "center": {"lat": 9.9318, "lon": 76.2673}, "tags": {"name": "Zebra Bakery", "shop": "bakery"}},
        {"type": "node", "id": 303, "lat": 9.95, "lon": 76.30, "tags": {"name": "Far"}}
    ]})
    .to_string();
    let mock = Arc::new(Mock::default());
    *mock.replies.lock().unwrap() = vec![
        (429, "{}".into()),
        (200, found),
        (200, json!({"elements": []}).to_string()),
    ];
    let endpoint = mock_overpass(mock.clone()).await;
    let fast = || {
        let mut c = osm::OsmConfig::new(endpoint.clone());
        c.min_interval = Duration::from_millis(0);
        c
    };
    let mut checker = osm::OsmChecker::new(fast()).unwrap();

    // 429 -> backoff error, place untouched.
    let err = checker.step(&w.db, 1).await.unwrap_err();
    assert!(
        matches!(err, osm::OsmError::Busy { status: 429, .. }),
        "{err}"
    );
    assert!(checker.wait_time() > Duration::from_secs(1));

    // A fresh checker (no backoff): p1 gets two candidates, p2 none.
    let mut checker = osm::OsmChecker::new(fast()).unwrap();
    assert_eq!(checker.step(&w.db, 10).await.unwrap(), 2);
    assert_eq!(checker.requests, 2);
    assert!(
        mock.user_agents
            .lock()
            .unwrap()
            .iter()
            .all(|u| u == &osm::user_agent())
    );
    assert!(
        osm::user_agent().starts_with("eatsights/")
            && osm::user_agent().ends_with("(+https://eatsights.letsimpl.fyi)")
    );
    let body = mock.bodies.lock().unwrap()[1].clone();
    assert!(
        body.starts_with("data=") && body.contains("around"),
        "{body}"
    );
    w.work().await;
    let (_, _, d1) = get(&app, &format!("/v1/places/{p1}")).await;
    let (_, _, d2) = get(&app, &format!("/v1/places/{p2}")).await;
    eatsights_core::schema::validate_def("osm_link", &d1["osm"]).unwrap();
    eatsights_core::schema::validate_def("osm_link", &d2["osm"]).unwrap();
    assert_eq!(d1["osm"]["state"], "candidate", "{d1}");
    let c = d1["osm"]["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0]["id"], "node/101");
    assert!(c[0]["name_score"].as_f64().unwrap() > 0.5);
    assert_eq!(c[1]["id"], "way/202");
    assert_eq!(d2["osm"]["state"], "unmapped");
    assert!(d2["osm"]["checked_at"].is_string());

    // Nothing is due any more; a third place at the same spot is answered from the cache.
    assert_eq!(checker.step(&w.db, 10).await.unwrap(), 0);
    let p3 = w.place(&app, "Chaya Kada Two", "tea_stall", lat, lng).await;
    assert_eq!(checker.step(&w.db, 10).await.unwrap(), 1);
    assert_eq!(mock.requests.load(Ordering::SeqCst), 3);

    // Privacy: no OSM names, tags or coordinates anywhere in our tables.
    let dump: String =
        sqlx::query_scalar("SELECT coalesce(string_agg(doc::text, ' '), '') FROM public.places")
            .fetch_one(&w.db.read)
            .await
            .unwrap();
    for leak in [
        "Secret",
        "ചായക്കട",
        "Zebra",
        "amenity",
        "opening_hours",
        "9.93125",
        "9.9318",
        "76.26731",
    ] {
        assert!(!dump.contains(leak), "OSM data `{leak}` stored");
    }
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns WHERE table_schema IN ('public', 'private')
         AND (column_name ILIKE '%osm%' OR column_name ILIKE '%tag%')",
    )
    .fetch_all(&w.db.read)
    .await
    .unwrap();
    assert!(cols.is_empty(), "{cols:?}");

    // A person confirms a candidate.
    let (s, r) = send(
        &app,
        "/v1/admin/osm-links",
        &w.admin("link_osm", json!({"place_id": p1, "osm_id": "node/101"})),
    )
    .await;
    assert_eq!(s, 200, "{r}");
    w.sync_seq().await;
    let (_, _, d1) = get(&app, &format!("/v1/places/{p1}")).await;
    assert_eq!(d1["osm"]["state"], "linked");
    assert!(d1["osm"].get("candidates").is_none());
    let _ = p3;
    w.drop().await;
}
