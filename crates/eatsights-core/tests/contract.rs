#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{DateTime, Utc};
use eatsights_core::crypto;
use eatsights_core::registry::{Registry, Writer};
use eatsights_core::schema::{self, validate_def};
use eatsights_core::submission::{
    CheckConfig, ContributorClass, SubmissionError, check_submission, to_public_observation,
};
use serde_json::{Value, json};

const SCHEMA_MD: &str = include_str!("../../../SCHEMA.md");

fn sig86() -> String {
    "A".repeat(86)
}

fn json_blocks() -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in SCHEMA_MD.lines() {
        match &mut cur {
            None if line.trim_start() == "```json" => cur = Some(String::new()),
            Some(buf) if line.trim_start() == "```" => {
                out.push(std::mem::take(buf));
                cur = None;
            }
            Some(buf) => {
                buf.push_str(line);
                buf.push('\n');
            }
            None => {}
        }
    }
    out
}

fn examples() -> Vec<Value> {
    json_blocks()
        .into_iter()
        .map(|b| {
            let b = b.replace("<86 characters of base64url>", &sig86());
            let t = b.trim();
            if t.starts_with('{') {
                serde_json::from_str(t).unwrap()
            } else {
                // A member fragment such as `"osm": {...}`.
                serde_json::from_str(&format!("{{{t}}}")).unwrap()
            }
        })
        .collect()
}

#[test]
fn schema_md_examples_validate() {
    let ex = examples();
    assert_eq!(ex.len(), 5, "expected 5 json blocks in SCHEMA.md");

    let place = &ex[0];
    validate_def("place", place).unwrap();

    let mut merged = place.clone();
    merged["osm"] = ex[1]["osm"].clone();
    validate_def("osm_link", &ex[1]["osm"]).unwrap();
    validate_def("place", &merged).unwrap();

    validate_def("submission", &ex[2]).unwrap();
    validate_def("observation", &ex[3]).unwrap();
    validate_def("attestation", &ex[4]).unwrap();

    // The forms are not interchangeable.
    assert!(validate_def("observation", &ex[2]).is_err());
    assert!(validate_def("submission", &ex[3]).is_err());
}

#[test]
fn public_form_of_example_matches_published_example() {
    let ex = examples();
    let public = to_public_observation(&ex[2], ContributorClass::Member, "Zq8nT3vLp0XhYc5JmR2sWd");
    assert_eq!(public, ex[3]);
    validate_def("observation", &public).unwrap();
    let text = public.to_string();
    assert!(!text.contains("k7Qm2xWv9LpR4tYb8NcD3a"));
    assert!(!text.contains("signature"));
}

#[test]
fn every_def_rejects_garbage() {
    for def in schema::DEFS {
        assert!(validate_def(def, &json!({"nope": 1})).is_err(), "{def}");
    }
}

#[test]
fn osm_link_rules() {
    validate_def(
        "osm_link",
        &json!({"state": "linked", "id": "node/1", "verified_at": "2026-01-01"}),
    )
    .unwrap();
    assert!(validate_def("osm_link", &json!({"state": "linked", "id": "node/1"})).is_err());
    assert!(validate_def("osm_link", &json!({"state": "unmapped"})).is_err());
    assert!(validate_def("osm_link", &json!({"state": "candidate", "candidates": []})).is_err());
    assert!(validate_def("osm_link", &json!({"state": "unchecked", "id": "node/1"})).is_err());
}

#[test]
fn merged_place_needs_merged_into() {
    let mut p = examples()[0].clone();
    p["record_state"] = json!("merged");
    assert!(validate_def("place", &p).is_err());
    p["merged_into"] = json!("pl_01JC8Y0A1B2C3D4E5F6G7H8J9M");
    validate_def("place", &p).unwrap();
    p["record_state"] = json!("active");
    assert!(validate_def("place", &p).is_err());
}

// ---------------------------------------------------------------- registry

#[test]
fn registry_loads_and_entries_validate() {
    let reg = Registry::embedded().unwrap();
    assert_eq!(reg.len(), 30);
    assert_eq!(reg.version(), "0.3");
    assert_eq!(reg.hash(), eatsights_core::registry::registry_hash());
    assert_eq!(reg.hash().len(), 64);
    let file: Value = serde_json::from_str(schema::PROPERTIES_JSON).unwrap();
    for entry in file["properties"].as_array().unwrap() {
        schema::validate_property_entry(entry).unwrap();
        let id = entry["id"].as_str().unwrap();
        assert_eq!(reg.get_raw(id), Some(entry));
    }
    assert!(schema::validate_property_entry(&json!({"id": "x"})).is_err());
    let p = reg.get("availability.declared_reliability").unwrap();
    assert!(!p.client_writable());
    assert_eq!(p.writers, vec![Writer::System]);
}

/// (property, valid value, invalid value). Must cover every property.
fn fixtures() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "listing.status",
            json!({"state": "listed"}),
            json!({"state": "gone"}),
        ),
        (
            "price.amount",
            json!({"amount_minor": 1000, "currency": "INR", "fulfilment": "dine_in"}),
            json!({"amount_minor": -1, "currency": "INR", "fulfilment": "dine_in"}),
        ),
        (
            "price.packaging_charge",
            json!({"amount_minor": 500, "currency": "INR"}),
            json!({"amount_minor": 500, "currency": "inr"}),
        ),
        (
            "serving.quantity",
            json!({"value": 150, "unit": "ml"}),
            json!({"value": 0, "unit": "ml"}),
        ),
        (
            "serving.temperature",
            json!({"state": "hot"}),
            json!({"state": "lukewarm"}),
        ),
        (
            "serving.container",
            json!({"kind": "clay_cup"}),
            json!({"kind": "mug"}),
        ),
        (
            "composition.milk",
            json!({"type": "cow"}),
            json!({"type": "goat"}),
        ),
        (
            "composition.sweetness",
            json!({"level": 2}),
            json!({"level": 5}),
        ),
        (
            "includes.items",
            json!({"items": ["rice", "sambar"]}),
            json!({"items": ["rice", "rice"]}),
        ),
        (
            "customisation.options",
            json!({"options": ["no_sugar"]}),
            json!({"options": []}),
        ),
        (
            "quality.score",
            json!({"score": 4, "worth_price": true}),
            json!({"score": 4, "comment": "nice"}),
        ),
        (
            "prep.wait_time",
            json!({"seconds": 300, "stage": "order_to_ready"}),
            json!({"seconds": 14401, "stage": "order_to_ready"}),
        ),
        (
            "availability.declared_schedule",
            json!({"rule": "Mo-Sa 06:00-11:00"}),
            json!({"rule": "x".repeat(501)}),
        ),
        (
            "availability.observed",
            json!({"state": "sold_out"}),
            json!({"state": "maybe"}),
        ),
        (
            "availability.sold_out_time",
            json!({"local_time": "10:45", "day_type": "weekday"}),
            json!({"local_time": "24:00", "day_type": "weekday"}),
        ),
        (
            "availability.declared_reliability",
            json!({"score": 0.8, "sample_days": 30}),
            json!({"score": 1.5, "sample_days": 30}),
        ),
        (
            "hours.declared",
            json!({"rule": "Mo-Su 06:00-22:00"}),
            json!({}),
        ),
        (
            "hours.observed_open",
            json!({"state": "open_now"}),
            json!({"state": "open"}),
        ),
        (
            "status.operating",
            json!({"state": "active"}),
            json!({"state": "closed"}),
        ),
        (
            "payment.methods",
            json!({"methods": ["cash", "upi"]}),
            json!({"methods": ["bitcoin"]}),
        ),
        (
            "seating.kind",
            json!({"kinds": ["standing"]}),
            json!({"kinds": "standing"}),
        ),
        (
            "facilities.features",
            json!({"features": ["wifi"]}),
            json!({"features": ["pool"]}),
        ),
        ("hygiene.score", json!({"score": 5}), json!({"score": 0})),
        (
            "queue.length",
            json!({"bucket": "1_to_3"}),
            json!({"bucket": 2}),
        ),
        ("dish.diet", json!({"diet": "veg"}), json!({"diet": "keto"})),
        (
            "dish.ingredients",
            json!({"items": ["tea_leaf", "Q11002"]}),
            json!({"items": ["Tea Leaf"]}),
        ),
        (
            "dish.allergens",
            json!({"allergens": []}),
            json!({"allergens": ["dust"]}),
        ),
        (
            "dish.typical_serving",
            json!({"value": 1, "unit": "plate"}),
            json!({"value": 1}),
        ),
        (
            "dish.origin",
            json!({"wikidata": "Q1186"}),
            json!({"wikidata": "Kerala"}),
        ),
        (
            "availability.meal_times",
            json!({"times": ["breakfast"]}),
            json!({"times": ["brunch"]}),
        ),
    ]
}

#[test]
fn every_property_has_valid_and_invalid_fixture() {
    let reg = Registry::embedded().unwrap();
    let fx = fixtures();
    for p in reg.iter() {
        assert!(
            fx.iter().any(|(id, _, _)| *id == p.id),
            "no fixture for {}",
            p.id
        );
    }
    for (id, good, bad) in fx {
        reg.validate_value(id, &good)
            .unwrap_or_else(|e| panic!("{id} good: {e:?}"));
        assert!(
            reg.validate_value(id, &bad).is_err(),
            "{id} bad value accepted"
        );
    }
    assert!(reg.validate_value("no.such", &json!({})).is_err());
}

// -------------------------------------------------------------- submissions

fn now() -> DateTime<Utc> {
    "2026-11-14T09:15:00Z".parse().unwrap()
}

fn example_submission() -> Value {
    examples()[2].clone()
}

fn check(v: &Value) -> Result<eatsights_core::submission::CheckedSubmission, SubmissionError> {
    check_submission(
        v,
        Registry::embedded().unwrap(),
        now(),
        &CheckConfig::default(),
    )
}

#[test]
fn example_submission_passes_checks() {
    let c = check(&example_submission()).unwrap();
    assert_eq!(c.property, "price.amount");
    assert_eq!(c.anchor, "pl_01JC8Y0A1B2C3D4E5F6G7H8J9K");
    assert_eq!(c.key_id, "k7Qm2xWv9LpR4tYb8NcD3a");
}

#[test]
fn submission_rejections() {
    let base = example_submission();

    let mut s = base.clone();
    s["license"] = json!("CC-BY-4.0");
    assert!(matches!(
        check(&s),
        Err(SubmissionError::InvalidEnvelope(_))
    ));

    let mut s = base.clone();
    s["extra"] = json!(1);
    assert!(matches!(
        check(&s),
        Err(SubmissionError::InvalidEnvelope(_))
    ));

    let mut s = base.clone();
    s["property"] = json!("price.unknown");
    assert!(matches!(
        check(&s),
        Err(SubmissionError::UnknownProperty(_))
    ));

    let mut s = base.clone();
    s["property"] = json!("hours.observed_open");
    s["value"] = json!({"state": "open_now"});
    assert!(matches!(
        check(&s),
        Err(SubmissionError::ScopeMismatch { .. })
    ));

    let mut s = base.clone();
    s["property"] = json!("quality.score");
    s["value"] = json!({"score": 3});
    s["source_type"] = json!("vendor");
    s["method"] = json!("vendor_declared");
    assert!(matches!(
        check(&s),
        Err(SubmissionError::WriterNotAllowed { .. })
    ));

    let mut s = base.clone();
    s["property"] = json!("availability.declared_reliability");
    s["value"] = json!({"score": 0.5, "sample_days": 3});
    for st in ["customer", "vendor", "import"] {
        s["source_type"] = json!(st);
        assert!(
            matches!(check(&s), Err(SubmissionError::WriterNotAllowed { .. })),
            "{st}"
        );
    }

    let mut s = base.clone();
    s["value"] = json!({"amount_minor": "ten"});
    assert!(matches!(check(&s), Err(SubmissionError::InvalidValue(_))));

    // dispute with a value is checked too
    let mut s = base.clone();
    s["kind"] = json!("dispute");
    s["refers_to"] = json!("ob_01JC8Z3QK5M7N2P4R6T8V0X1Y3");
    check(&s).unwrap();
    s["value"] = json!({"amount_minor": -5, "currency": "INR", "fulfilment": "dine_in"});
    assert!(matches!(check(&s), Err(SubmissionError::InvalidValue(_))));

    // confirm with no value
    let mut s = base.clone();
    s["kind"] = json!("confirm");
    s["refers_to"] = json!("ob_01JC8Z3QK5M7N2P4R6T8V0X1Y3");
    s.as_object_mut().unwrap().remove("value");
    check(&s).unwrap();

    let mut s = base.clone();
    s["observed_at"] = json!("2026-11-14T09:30:00Z");
    assert!(matches!(check(&s), Err(SubmissionError::ObservedInFuture)));

    let mut s = base.clone();
    s["observed_at"] = json!("2026-10-01T00:00:00Z");
    assert!(matches!(check(&s), Err(SubmissionError::ObservedTooOld)));

    let mut s = base.clone();
    s["submitted_at"] = json!("2026-11-14T08:00:00Z");
    s["observed_at"] = json!("2026-11-14T07:00:00Z");
    assert!(matches!(check(&s), Err(SubmissionError::SubmittedAtSkew)));

    let mut s = base.clone();
    s["observed_at"] = json!("not a time");
    assert!(check(&s).is_err());
}

#[test]
fn dish_scope_anchor_is_dish_id() {
    let mut s = example_submission();
    s["property"] = json!("dish.diet");
    s["subject"] = json!({"scope": "dish", "dish_id": "di_01JC8Y0A1B2C3D4E5F6G7H8J9K"});
    s["value"] = json!({"diet": "veg"});
    let c = check(&s).unwrap();
    assert_eq!(c.anchor, "di_01JC8Y0A1B2C3D4E5F6G7H8J9K");
}

#[test]
fn signed_submission_end_to_end() {
    let sk = crypto::generate_signing_key();
    let kid = crypto::key_id(&sk.verifying_key());
    let mut s = example_submission();
    s["contributor"]["key_id"] = json!(kid);
    let signed = crypto::sign_object(&s, &sk).unwrap();
    validate_def("submission", &signed).unwrap();
    crypto::verify_object(&signed, &sk.verifying_key()).unwrap();
    let c = check(&signed).unwrap();
    let pseud = crypto::pseudonym(&[9u8; 32], &c.key_id, &c.anchor).unwrap();
    let public = to_public_observation(&signed, ContributorClass::Unverified, &pseud);
    validate_def("observation", &public).unwrap();
    let text = public.to_string();
    assert!(!text.contains(&kid));
    assert!(!text.contains(signed["signature"].as_str().unwrap()));
}
