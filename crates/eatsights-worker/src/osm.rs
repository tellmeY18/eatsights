//! OSM checker (AGENTS.md 9.2) and `ready_to_map` (ADR-0006 #7).
//!
//! The checker is opt-in: it runs only when `EATSIGHTS_OSM_ENDPOINT` (an Overpass API URL)
//! is set. It asks for food amenities and shops within 100 m of a place's pin. OSM tags are
//! read only in memory to compute a name score and are then dropped: the place stores the
//! state, `checked_at`, candidate ids, distances and name scores, never OSM names, tags or
//! coordinates. A person confirms a candidate through the admin API; the system never links.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use eatsights_core::{geo, schema};
use eatsights_db::osm::{self as q, PlaceToCheck};
use eatsights_db::{Db, DbError, work};
use serde_json::{Value, json};
use tokio::sync::watch;

pub const READY_CURSOR: &str = "ready_to_map";

/// Configuration of the OSM checker.
#[derive(Debug, Clone)]
pub struct OsmConfig {
    /// Overpass API interpreter URL, e.g. `https://overpass-api.de/api/interpreter`.
    pub endpoint: String,
    /// Search radius around the pin in metres.
    pub radius_m: u32,
    /// Minimum time between two requests to the endpoint.
    pub min_interval: Duration,
    /// Re-check `unmapped` places whose `checked_at` is older than this.
    pub recheck_days: i64,
    pub http_timeout: Duration,
    /// Lifetime of a cached response (keyed by the pin rounded to about 11 m).
    pub cache_ttl: Duration,
    pub cache_max_entries: usize,
    /// Candidates kept per place (best first).
    pub max_candidates: usize,
    /// Pause when nothing is due.
    pub idle: Duration,
}

impl OsmConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            radius_m: 100,
            min_interval: Duration::from_secs(10),
            recheck_days: 30,
            http_timeout: Duration::from_secs(30),
            cache_ttl: Duration::from_secs(24 * 3600),
            cache_max_entries: 2000,
            max_candidates: 5,
            idle: Duration::from_secs(300),
        }
    }
}

pub fn user_agent() -> String {
    format!(
        "eatsights/{} (+{})",
        env!("CARGO_PKG_VERSION"),
        eatsights_core::BASE_URL
    )
}

#[derive(Debug, thiserror::Error)]
pub enum OsmError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("http: {0}")]
    Http(String),
    /// 429 or 5xx: back off (optionally for the server's `Retry-After`).
    #[error("endpoint busy (status {status})")]
    Busy {
        status: u16,
        retry_after: Option<Duration>,
    },
    #[error("bad response: {0}")]
    BadResponse(String),
}

// ------------------------------------------------------------------ name score

fn normalise(s: &str) -> String {
    let lower: String = s
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Trigrams as `pg_trgm` builds them: each word padded with two spaces before and one after.
fn trigrams(s: &str) -> BTreeSet<[char; 3]> {
    let mut out = BTreeSet::new();
    for w in normalise(s).split(' ').filter(|w| !w.is_empty()) {
        let padded: Vec<char> = "  ".chars().chain(w.chars()).chain([' ']).collect();
        for t in padded.windows(3) {
            out.insert([t[0], t[1], t[2]]);
        }
    }
    out
}

/// Trigram similarity in [0, 1] (shared / union), like `pg_trgm.similarity`.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (ta, tb) = (trigrams(a), trigrams(b));
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let shared = ta.intersection(&tb).count();
    let union = ta.len() + tb.len() - shared;
    shared as f64 / union as f64
}

/// Best similarity between any of our names and any OSM name (`name`, `name:*`,
/// `alt_name`, `old_name`, `short_name`, `official_name`, `brand`). Rounded to 2 decimals.
pub fn name_score(ours: &[String], tags: Option<&serde_json::Map<String, Value>>) -> f64 {
    let Some(tags) = tags else { return 0.0 };
    let theirs = tags.iter().filter_map(|(k, v)| {
        let is_name = k == "name"
            || k.starts_with("name:")
            || matches!(
                k.as_str(),
                "alt_name" | "old_name" | "short_name" | "official_name" | "brand"
            );
        if is_name { v.as_str() } else { None }
    });
    let mut best = 0.0f64;
    for t in theirs {
        for o in ours {
            best = best.max(similarity(o, t));
        }
    }
    (best * 100.0).round() / 100.0
}

// ------------------------------------------------------------------ Overpass

/// One OSM candidate in the form we keep: id, distance and name score only.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub distance_m: f64,
    pub name_score: f64,
}

/// An element of an Overpass response, held in memory only (cache and scoring).
#[derive(Debug, Clone)]
struct Element {
    id: String,
    lat: f64,
    lng: f64,
    tags: Option<serde_json::Map<String, Value>>,
}

fn overpass_query(lat: f64, lng: f64, radius: u32) -> String {
    let around = format!("around:{radius},{lat:.7},{lng:.7}");
    format!(
        "[out:json][timeout:25];(\
         nwr({around})[amenity~\"^(restaurant|cafe|fast_food|food_court|bar|pub|ice_cream|canteen)$\"];\
         nwr({around})[shop~\"^(bakery|beverages|coffee|tea|confectionery|pastry|deli|food)$\"];\
         );out center tags;"
    )
}

fn parse_elements(body: &[u8]) -> Result<Vec<Element>, OsmError> {
    let v: Value =
        serde_json::from_slice(body).map_err(|e| OsmError::BadResponse(e.to_string()))?;
    let els = v
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| OsmError::BadResponse("no elements array".into()))?;
    let mut out = Vec::new();
    for e in els {
        let kind = e.get("type").and_then(Value::as_str).unwrap_or_default();
        if !matches!(kind, "node" | "way" | "relation") {
            continue;
        }
        let Some(id) = e.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let (lat, lng) = match (e.get("lat"), e.get("lon"), e.get("center")) {
            (Some(la), Some(lo), _) => (la.as_f64(), lo.as_f64()),
            (_, _, Some(c)) => (
                c.get("lat").and_then(Value::as_f64),
                c.get("lon").and_then(Value::as_f64),
            ),
            _ => (None, None),
        };
        let (Some(lat), Some(lng)) = (lat, lng) else {
            continue;
        };
        out.push(Element {
            id: format!("{kind}/{id}"),
            lat,
            lng,
            tags: e.get("tags").and_then(Value::as_object).cloned(),
        });
    }
    Ok(out)
}

fn candidates(place: &PlaceToCheck, els: &[Element], radius: f64, max: usize) -> Vec<Candidate> {
    let mut seen = BTreeSet::new();
    let mut out: Vec<Candidate> = els
        .iter()
        .filter(|e| seen.insert(e.id.clone()))
        .filter_map(|e| {
            let d = geo::distance_m(place.lat, place.lng, e.lat, e.lng);
            (d <= radius).then(|| Candidate {
                id: e.id.clone(),
                distance_m: (d * 10.0).round() / 10.0,
                name_score: name_score(&place.names, e.tags.as_ref()),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.name_score
            .total_cmp(&a.name_score)
            .then(a.distance_m.total_cmp(&b.distance_m))
            .then(a.id.cmp(&b.id))
    });
    out.truncate(max);
    out
}

/// The new `osm` object for a check result. Keeps `ready_to_map` (recomputed separately).
pub fn osm_after_check(current: &Value, found: &[Candidate], now: DateTime<Utc>) -> Value {
    let checked_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut osm = if found.is_empty() {
        json!({ "state": "unmapped", "checked_at": checked_at })
    } else {
        json!({
            "state": "candidate",
            "checked_at": checked_at,
            "candidates": found.iter().map(|c| json!({
                "id": c.id, "distance_m": c.distance_m, "name_score": c.name_score,
            })).collect::<Vec<_>>(),
        })
    };
    if found.is_empty()
        && let Some(r) = current.get("ready_to_map")
    {
        osm["ready_to_map"] = r.clone();
    }
    osm
}

type CacheKey = (i64, i64);

/// The checker: one HTTP client, a request pacer, a response cache and a backoff.
pub struct OsmChecker {
    cfg: OsmConfig,
    client: reqwest::Client,
    last_request: Option<Instant>,
    backoff: Duration,
    blocked_until: Option<Instant>,
    cache: HashMap<CacheKey, (Instant, Vec<Element>)>,
    pub requests: u64,
}

impl OsmChecker {
    pub fn new(cfg: OsmConfig) -> Result<Self, OsmError> {
        let client = reqwest::Client::builder()
            .user_agent(user_agent())
            .timeout(cfg.http_timeout)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| OsmError::Http(e.to_string()))?;
        Ok(Self {
            cfg,
            client,
            last_request: None,
            backoff: Duration::ZERO,
            blocked_until: None,
            cache: HashMap::new(),
            requests: 0,
        })
    }

    fn cache_key(lat: f64, lng: f64) -> CacheKey {
        (
            (lat * 10_000.0).round() as i64,
            (lng * 10_000.0).round() as i64,
        )
    }

    /// Time to wait before the next request is allowed (pacing and backoff).
    pub fn wait_time(&self) -> Duration {
        let now = Instant::now();
        let pace = self
            .last_request
            .map(|t| (t + self.cfg.min_interval).saturating_duration_since(now))
            .unwrap_or_default();
        let block = self
            .blocked_until
            .map(|t| t.saturating_duration_since(now))
            .unwrap_or_default();
        pace.max(block)
    }

    async fn fetch(&mut self, lat: f64, lng: f64) -> Result<Vec<Element>, OsmError> {
        let key = Self::cache_key(lat, lng);
        if let Some((at, els)) = self.cache.get(&key)
            && at.elapsed() < self.cfg.cache_ttl
        {
            return Ok(els.clone());
        }
        let wait = self.wait_time();
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        self.last_request = Some(Instant::now());
        self.requests += 1;
        // Query around the rounded point (+ radius slack) so cached results serve every pin
        // that rounds to the same key.
        let (clat, clng) = (key.0 as f64 / 10_000.0, key.1 as f64 / 10_000.0);
        let query = overpass_query(clat, clng, self.cfg.radius_m + 10);
        let resp = self
            .client
            .post(&self.cfg.endpoint)
            .form(&[("data", query)])
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                self.note_failure(None);
                return Err(OsmError::Http(e.to_string()));
            }
        };
        let status = resp.status().as_u16();
        if status == 429 || status >= 500 {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            self.note_failure(retry_after);
            return Err(OsmError::Busy {
                status,
                retry_after,
            });
        }
        if !(200..300).contains(&status) {
            return Err(OsmError::Http(format!("status {status}")));
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| OsmError::Http(e.to_string()))?;
        let els = parse_elements(&body)?;
        self.backoff = Duration::ZERO;
        self.blocked_until = None;
        if self.cache.len() >= self.cfg.cache_max_entries {
            let ttl = self.cfg.cache_ttl;
            self.cache.retain(|_, (at, _)| at.elapsed() < ttl);
            if self.cache.len() >= self.cfg.cache_max_entries {
                self.cache.clear();
            }
        }
        self.cache.insert(key, (Instant::now(), els.clone()));
        Ok(els)
    }

    fn note_failure(&mut self, retry_after: Option<Duration>) {
        let base = self.cfg.min_interval.max(Duration::from_secs(1));
        self.backoff = if self.backoff.is_zero() {
            base * 6
        } else {
            (self.backoff * 2).min(Duration::from_secs(3600))
        };
        let wait = retry_after.unwrap_or(self.backoff).max(self.backoff);
        self.blocked_until = Some(Instant::now() + wait);
    }

    /// Check one place and store the result. Returns the new `osm` object when written.
    pub async fn check_place(
        &mut self,
        db: &Db,
        place: &PlaceToCheck,
        now: DateTime<Utc>,
    ) -> Result<Option<Value>, OsmError> {
        let els = self.fetch(place.lat, place.lng).await?;
        let found = candidates(
            place,
            &els,
            f64::from(self.cfg.radius_m),
            self.cfg.max_candidates,
        );
        drop(els);
        let osm = osm_after_check(&place.osm, &found, now);
        if let Err(e) = schema::validate_def("osm_link", &osm) {
            return Err(OsmError::BadResponse(e.join("; ")));
        }
        let written = q::set_osm(&db.write, &place.id, &place.osm, &osm).await?;
        if written {
            refresh_ready_to_map(db, &place.id, now).await?;
        }
        Ok(written.then_some(osm))
    }

    /// Check up to `limit` due places. Returns how many were checked.
    pub async fn step(&mut self, db: &Db, limit: i64) -> Result<usize, OsmError> {
        let now = Utc::now();
        let before = now - chrono::Duration::days(self.cfg.recheck_days);
        let due = q::places_due(&db.write, before, limit).await?;
        let mut n = 0;
        for p in &due {
            self.check_place(db, p, Utc::now()).await?;
            n += 1;
        }
        Ok(n)
    }
}

/// Run the OSM checker until `shutdown`.
pub async fn run(db: Db, cfg: OsmConfig, mut shutdown: watch::Receiver<bool>) {
    let idle = cfg.idle;
    let mut checker = match OsmChecker::new(cfg) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "OSM checker disabled");
            return;
        }
    };
    tracing::info!(user_agent = %user_agent(), "OSM checker started");
    loop {
        if *shutdown.borrow() {
            break;
        }
        let pause = match checker.step(&db, 1).await {
            Ok(0) => idle,
            Ok(_) => Duration::ZERO,
            Err(e) => {
                tracing::warn!(error = %e, "OSM check failed");
                checker.wait_time().max(Duration::from_secs(1))
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            Ok(()) = shutdown.changed() => {}
        }
    }
    tracing::info!("OSM checker stopped");
}

// ------------------------------------------------------------------ ready_to_map

/// ADR-0006 #7 as a pure function of the place document, record state and the
/// `status.operating` fact.
pub fn is_ready_to_map(
    doc: &Value,
    record_state: &str,
    fact: Option<&Value>,
    now: DateTime<Utc>,
    max_age_days: i64,
) -> bool {
    let unmapped = doc.pointer("/osm/state").and_then(Value::as_str) == Some("unmapped");
    let named = doc
        .get("names")
        .and_then(Value::as_object)
        .is_some_and(|m| {
            m.values()
                .any(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
        });
    let category = doc.get("category").and_then(Value::as_str).is_some();
    let pin = doc
        .pointer("/location/lat")
        .and_then(Value::as_f64)
        .is_some()
        && doc
            .pointer("/location/lng")
            .and_then(Value::as_f64)
            .is_some();
    let Some(f) = fact else { return false };
    let confirmed = f.get("status").and_then(Value::as_str) == Some("confirmed")
        && f.pointer("/counts/distinct_contributors")
            .and_then(Value::as_u64)
            .is_some_and(|n| n >= 2)
        && f.pointer("/current_value/state").and_then(Value::as_str) == Some("active");
    let recent = f
        .get("last_observed_at")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| t.with_timezone(&Utc) >= now - chrono::Duration::days(max_age_days));
    record_state == "active" && unmapped && named && category && pin && confirmed && recent
}

pub const READY_MAX_AGE_DAYS: i64 = 90;

/// Recompute `osm.ready_to_map` for one place. Writes `true` when it holds; writes `false`
/// only over a previous `true` (absent means false).
pub async fn refresh_ready_to_map(
    db: &Db,
    place_id: &str,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let Some(i) = q::ready_inputs(&db.write, place_id).await? else {
        return Ok(false);
    };
    let ready = is_ready_to_map(
        &i.doc,
        &i.record_state,
        i.operating_fact.as_ref(),
        now,
        READY_MAX_AGE_DAYS,
    );
    let current = i.doc.pointer("/osm/ready_to_map").and_then(Value::as_bool);
    if ready && current != Some(true) {
        q::set_ready_to_map(&db.write, place_id, true).await
    } else if !ready && current == Some(true) {
        q::set_ready_to_map(&db.write, place_id, false).await
    } else {
        Ok(false)
    }
}

/// Follow the change feed for place and `status.operating` fact changes; also re-check
/// flagged places so the 90-day window expires. Returns change rows consumed.
pub async fn ready_step(db: &Db, sweep: bool) -> Result<usize, DbError> {
    let after = work::cursor(&db.write, READY_CURSOR).await?;
    let changes = work::changes_after(&db.write, after, 500).await?;
    let now = Utc::now();
    let mut places = BTreeSet::new();
    for c in &changes {
        match c.entity_type.as_str() {
            "place" if c.op != "ready_to_map" => {
                places.insert(c.entity_id.clone());
            }
            "fact" => {
                if let Some(id) = c
                    .entity_id
                    .strip_prefix("place:")
                    .and_then(|r| r.strip_suffix("|status.operating"))
                {
                    places.insert(id.to_owned());
                }
            }
            _ => {}
        }
    }
    if sweep {
        places.extend(q::ready_places(&db.write).await?);
    }
    for p in &places {
        refresh_ready_to_map(db, p, now).await?;
    }
    if let Some(last) = changes.last() {
        work::set_cursor(&db.write, READY_CURSOR, last.seq).await?;
    }
    Ok(changes.len())
}

/// Run the `ready_to_map` maintainer until `shutdown`.
pub async fn run_ready(
    db: Db,
    mut wake: watch::Receiver<i64>,
    mut shutdown: watch::Receiver<bool>,
    interval: Duration,
) {
    let mut last_sweep: Option<Instant> = None;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let sweep = last_sweep.is_none_or(|t| t.elapsed() > Duration::from_secs(3600));
        match ready_step(&db, sweep).await {
            Ok(n) => {
                if sweep {
                    last_sweep = Some(Instant::now());
                }
                if n == 500 {
                    continue;
                }
            }
            Err(e) => tracing::warn!(error = %e, "ready_to_map step failed"),
        }
        tokio::select! {
            Ok(()) = wake.changed() => {}
            Ok(()) = shutdown.changed() => {}
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn similarity_matches_pg_trgm_shape() {
        assert!((similarity("Chaya Kada", "chaya kada") - 1.0).abs() < 1e-9);
        assert!(similarity("Chaya Kada", "Chaya Kadai") > 0.5);
        assert!(similarity("Chaya Kada", "Burger Hub") < 0.1);
        assert_eq!(similarity("", "x"), 0.0);
        // "word" -> "  w"," wo","wor","ord","rd " (5 trigrams, like pg_trgm)
        assert_eq!(trigrams("word").len(), 5);
    }

    #[test]
    fn name_score_uses_all_name_tags() {
        let tags = json!({"name": "Hotel X", "name:ml": "ചായക്കട", "amenity": "cafe"});
        let s = name_score(&["ചായക്കട".to_owned()], tags.as_object());
        assert_eq!(s, 1.0);
        assert_eq!(name_score(&["cafe".to_owned()], tags.as_object()), 0.0);
    }

    #[test]
    fn osm_object_validates() {
        schema::init().unwrap();
        let now = Utc::now();
        let none = osm_after_check(&json!({"state": "unchecked"}), &[], now);
        schema::validate_def("osm_link", &none).unwrap();
        assert_eq!(none["state"], "unmapped");
        let some = osm_after_check(
            &json!({"state": "unchecked"}),
            &[Candidate {
                id: "node/1".into(),
                distance_m: 12.5,
                name_score: 0.8,
            }],
            now,
        );
        schema::validate_def("osm_link", &some).unwrap();
        assert_eq!(some["state"], "candidate");
    }

    #[test]
    fn ready_rule() {
        let now = Utc::now();
        let doc = json!({"names": {"en": "A"}, "category": "cafe", "location": {"lat": 1.0, "lng": 2.0},
            "osm": {"state": "unmapped", "checked_at": "2026-01-01T00:00:00Z"}});
        let t = |d: i64| (now - chrono::Duration::days(d)).to_rfc3339();
        let fact = |status: &str, n: u64, days: i64, state: &str| {
            json!({"status": status, "counts": {"distinct_contributors": n},
                "last_observed_at": t(days), "current_value": {"state": state}})
        };
        let ok = fact("confirmed", 2, 10, "active");
        assert!(is_ready_to_map(&doc, "active", Some(&ok), now, 90));
        assert!(!is_ready_to_map(&doc, "merged", Some(&ok), now, 90));
        assert!(!is_ready_to_map(&doc, "active", None, now, 90));
        assert!(!is_ready_to_map(
            &doc,
            "active",
            Some(&fact("unconfirmed", 2, 10, "active")),
            now,
            90
        ));
        assert!(!is_ready_to_map(
            &doc,
            "active",
            Some(&fact("confirmed", 1, 10, "active")),
            now,
            90
        ));
        assert!(!is_ready_to_map(
            &doc,
            "active",
            Some(&fact("confirmed", 2, 91, "active")),
            now,
            90
        ));
        assert!(!is_ready_to_map(
            &doc,
            "active",
            Some(&fact("confirmed", 2, 1, "permanently_closed")),
            now,
            90
        ));
        let mut linked = doc.clone();
        linked["osm"] = json!({"state": "candidate", "candidates": [{"id": "node/1"}]});
        assert!(!is_ready_to_map(&linked, "active", Some(&ok), now, 90));
    }

    #[test]
    fn parses_overpass_and_keeps_only_ids() {
        let body = br#"{"elements":[
          {"type":"node","id":1,"lat":9.93120,"lon":76.26730,"tags":{"name":"Chaya Kada","amenity":"cafe"}},
          {"type":"way","id":2,"center":{"lat":9.9320,"lon":76.2673},"tags":{"name":"Other"}},
          {"type":"node","id":3,"lat":10.5,"lon":76.0}
        ]}"#;
        let els = parse_elements(body).unwrap();
        let place = PlaceToCheck {
            id: "pl_x".into(),
            lat: 9.9312,
            lng: 76.2673,
            names: vec!["Chaya Kada".into()],
            osm: json!({"state": "unchecked"}),
        };
        let c = candidates(&place, &els, 100.0, 5);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].id, "node/1");
        assert_eq!(c[0].name_score, 1.0);
        assert!(c[1].distance_m > 80.0 && c[1].distance_m <= 100.0);
        let osm = osm_after_check(&place.osm, &c, Utc::now());
        let text = osm.to_string();
        assert!(!text.contains("Chaya") && !text.contains("9.93") && !text.contains("amenity"));
    }
}
