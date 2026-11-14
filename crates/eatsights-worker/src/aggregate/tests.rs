use super::Strategy;
use super::*;
use chrono::{Duration, TimeZone};
use proptest::prelude::*;
#[allow(unused_imports)]
use proptest::strategy::Strategy as _;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

fn cfg() -> AggConfig {
    AggConfig::default_config().unwrap()
}

fn subject() -> Value {
    json!({"scope": "menu_item", "place_id": "pl_01JC0000000000000000000000", "menu_item_id": "mi_01JC0000000000000000000000"})
}

fn prop(id: &str, strategy: Strategy, hl: f64) -> PropertyDef {
    PropertyDef {
        id: id.into(),
        half_life_hours: hl,
        strategy,
        field: None,
        window: None,
    }
}

fn ob(id: &str, p: &str, v: Value, hours_ago: i64) -> ObsInput {
    ObsInput {
        id: id.into(),
        kind: Kind::Value,
        value: Some(v),
        refers_to: None,
        observed_at: now() - Duration::hours(hours_ago),
        class: ContributorClass::Member,
        pseudonym: p.into(),
        source_type: SourceType::Customer,
    }
}

fn confirm(id: &str, p: &str, refers: &str, hours_ago: i64) -> ObsInput {
    ObsInput {
        kind: Kind::Confirm,
        value: None,
        refers_to: Some(refers.into()),
        ..ob(id, p, Value::Null, hours_ago)
    }
}

fn dispute(id: &str, p: &str, refers: &str, hours_ago: i64) -> ObsInput {
    ObsInput {
        kind: Kind::Dispute,
        value: None,
        refers_to: Some(refers.into()),
        ..ob(id, p, Value::Null, hours_ago)
    }
}

fn price(n: i64) -> Value {
    json!({"amount": n, "currency": "INR"})
}

fn run(p: &PropertyDef, obs: &[ObsInput]) -> Value {
    aggregate(p, &subject(), obs, now(), &cfg()).unwrap()
}

#[test]
fn default_config_loads() {
    let c = cfg();
    assert!(c.thresholds.min_distinct_to_confirm >= 2);
    assert!(AggConfig::load(None).is_ok());
    assert!(
        AggConfig::parse(
            &DEFAULT_CONFIG.replace("min_distinct_to_confirm = 2", "min_distinct_to_confirm = 1")
        )
        .is_err()
    );
}

#[test]
fn canonical_sorts_keys() {
    assert_eq!(
        canonical_json(&json!({"b": 1, "a": [1, {"d": 2, "c": 3}]})),
        r#"{"a":[1,{"c":3,"d":2}],"b":1}"#
    );
}

#[test]
fn latest_weighted_confirms_with_two_pseudonyms() {
    let p = prop("price.amount", Strategy::LatestWeighted, 1440.0);
    let f = run(
        &p,
        &[
            ob("ob_1", "a", price(20), 2),
            confirm("ob_2", "b", "ob_1", 1),
            ob("ob_3", "c", price(20), 1),
        ],
    );
    assert_eq!(f["status"], "confirmed");
    assert_eq!(f["current_value"], price(20));
    assert_eq!(f["counts"]["confirmations"], 3);
    assert_eq!(f["algorithm_version"], "agg-2026.11-1");
    assert!(f["last_confirmed_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(f["basis"], json!(["ob_1", "ob_2", "ob_3"]));
}

#[test]
fn repeating_key_cannot_stack() {
    let p = prop("price.amount", Strategy::LatestWeighted, 1440.0);
    let mut obs: Vec<_> = (0..10)
        .map(|i| ob(&format!("ob_a{i}"), "a", price(30), 1))
        .collect();
    obs.push(ob("ob_b", "b", price(20), 1));
    obs.push(ob("ob_c", "c", price(20), 1));
    let f = run(&p, &obs);
    assert_eq!(f["current_value"], price(20));
}

#[test]
fn single_verifier_never_confirms() {
    let p = prop("price.amount", Strategy::LatestWeighted, 1440.0);
    let mut o = ob("ob_1", "a", price(20), 0);
    o.class = ContributorClass::Verifier;
    let f = run(
        &p,
        &[
            o.clone(),
            ObsInput {
                id: "ob_2".into(),
                ..o
            },
        ],
    );
    assert_eq!(f["status"], "unconfirmed");
}

#[test]
fn disputes_contest() {
    let p = prop("price.amount", Strategy::LatestWeighted, 1440.0);
    let f = run(
        &p,
        &[
            ob("ob_1", "a", price(20), 2),
            ob("ob_2", "b", price(20), 2),
            dispute("ob_3", "c", "ob_1", 1),
        ],
    );
    assert_eq!(f["status"], "contested");
    assert_eq!(f["counts"]["disputes"], 1);
}

#[test]
fn old_facts_go_stale() {
    let p = prop("queue.length", Strategy::LatestWeighted, 0.5);
    let f = run(
        &p,
        &[
            ob("ob_1", "a", json!({"people": 3}), 5),
            ob("ob_2", "b", json!({"people": 3}), 5),
        ],
    );
    assert_eq!(f["status"], "stale");
}

#[test]
fn vendor_difference_is_flagged() {
    let p = prop("price.amount", Strategy::LatestWeighted, 1440.0);
    let mut v = ob("ob_v", "v", price(25), 0);
    v.class = ContributorClass::Vendor;
    v.source_type = SourceType::Vendor;
    let f = run(
        &p,
        &[
            ob("ob_1", "a", price(20), 1),
            ob("ob_2", "b", price(20), 1),
            v,
        ],
    );
    assert_eq!(f["current_value"], price(20));
    assert_eq!(f["vendor_value_differs"], true);
    assert_eq!(f["sources"], json!(["customer"]));
}

#[test]
fn mode_picks_most_common() {
    let p = prop("serving.temperature", Strategy::Mode, 2160.0);
    let hot = json!({"temperature": "hot"});
    let f = run(
        &p,
        &[
            ob("ob_1", "a", hot.clone(), 100),
            ob("ob_2", "b", hot.clone(), 90),
            ob("ob_3", "c", json!({"temperature": "cold"}), 1),
        ],
    );
    assert_eq!(f["current_value"], hot);
}

#[test]
fn median_by_field_and_unit() {
    let mut p = prop("serving.quantity", Strategy::Median, 2160.0);
    p.field = Some("value".into());
    p.window = Some(10);
    let q = |n: i64| json!({"value": n, "unit": "ml"});
    let f = run(
        &p,
        &[
            ob("ob_1", "a", q(90), 1),
            ob("ob_2", "b", q(100), 1),
            ob("ob_3", "c", q(110), 1),
            ob("ob_4", "d", json!({"value": 1, "unit": "cup"}), 1),
        ],
    );
    assert_eq!(f["current_value"], q(100));
}

#[test]
fn median_of_local_time() {
    let mut p = prop("availability.sold_out_time", Strategy::Median, 336.0);
    p.field = Some("local_time".into());
    let t = |s: &str| json!({"local_time": s, "day_type": "weekday"});
    let f = run(
        &p,
        &[
            ob("ob_1", "a", t("10:00"), 1),
            ob("ob_2", "b", t("10:30"), 1),
            ob("ob_3", "c", t("11:00"), 1),
        ],
    );
    assert_eq!(f["current_value"], t("10:30"));
}

#[test]
fn union_merges_sets() {
    let p = prop("payment.methods", Strategy::Union, 2160.0);
    let f = run(
        &p,
        &[
            ob("ob_1", "a", json!({"methods": ["upi", "cash"]}), 1),
            ob("ob_2", "b", json!({"methods": ["cash"]}), 1),
        ],
    );
    assert_eq!(f["current_value"], json!({"methods": ["cash", "upi"]}));
    assert_eq!(f["status"], "unconfirmed"); // upi has only one backer
}

#[test]
fn schedule_newest_rule_wins() {
    let p = prop("hours.declared", Strategy::Schedule, 720.0);
    let mut v = ob("ob_2", "v", json!({"rule": "Mo-Su 07:00-21:00"}), 1);
    v.source_type = SourceType::Vendor;
    v.class = ContributorClass::Vendor;
    let f = run(
        &p,
        &[ob("ob_1", "a", json!({"rule": "Mo-Sa 06:00-22:00"}), 10), v],
    );
    assert_eq!(f["current_value"], json!({"rule": "Mo-Su 07:00-21:00"}));
}

#[test]
fn derived_and_empty_yield_none() {
    let p = prop(
        "availability.declared_reliability",
        Strategy::Derived,
        720.0,
    );
    assert!(
        aggregate(
            &p,
            &subject(),
            &[ob("ob_1", "a", json!({}), 1)],
            now(),
            &cfg()
        )
        .is_none()
    );
    let p = prop("price.amount", Strategy::LatestWeighted, 1440.0);
    assert!(aggregate(&p, &subject(), &[], now(), &cfg()).is_none());
}

fn arb_obs() -> impl proptest::strategy::Strategy<Value = Vec<ObsInput>> {
    let one = (0u8..4, 0u8..3, 0i64..200, 0u8..6, 0u8..3, 0u8..2);
    prop::collection::vec(one, 1..14).prop_map(|v| {
        v.into_iter()
            .enumerate()
            .map(|(i, (p, val, age, class, kind, src))| {
                let id = format!("ob_{i:02}");
                let sets = [json!(["cash"]), json!(["upi"]), json!(["cash", "card"])];
                let mut o = ob(
                    &id,
                    &format!("p{p}"),
                    json!({"methods": sets[val as usize].clone()}),
                    age,
                );
                o.class = [
                    ContributorClass::Unverified,
                    ContributorClass::Member,
                    ContributorClass::Trusted,
                    ContributorClass::Verifier,
                    ContributorClass::Vendor,
                    ContributorClass::Import,
                ][class as usize];
                o.source_type = if src == 1 {
                    SourceType::Vendor
                } else {
                    SourceType::Customer
                };
                if kind > 0 && i > 0 {
                    o.kind = if kind == 1 {
                        Kind::Confirm
                    } else {
                        Kind::Dispute
                    };
                    o.refers_to = Some(format!("ob_{:02}", i / 2));
                    if kind == 1 {
                        o.value = None;
                    }
                }
                o
            })
            .collect()
    })
}

const STRATS: [Strategy; 4] = [
    Strategy::LatestWeighted,
    Strategy::Mode,
    Strategy::Union,
    Strategy::Schedule,
];

proptest! {
    #[test]
    fn permutation_invariant(obs in arb_obs(), seed in any::<u64>(), s in 0usize..4) {
        let p = prop("payment.methods", STRATS[s], 24.0);
        let mut shuffled = obs.clone();
        let n = shuffled.len();
        let mut x = seed;
        for i in (1..n).rev() {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            shuffled.swap(i, (x >> 33) as usize % (i + 1));
        }
        let a = aggregate(&p, &subject(), &obs, now(), &cfg());
        let b = aggregate(&p, &subject(), &shuffled, now(), &cfg());
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(a, aggregate(&p, &subject(), &obs, now(), &cfg()));
    }

    #[test]
    fn single_pseudonym_never_confirmed(obs in arb_obs(), s in 0usize..4) {
        let p = prop("payment.methods", STRATS[s], 24.0);
        let one: Vec<_> = obs.into_iter().map(|mut o| { o.pseudonym = "solo".into(); o }).collect();
        if let Some(f) = aggregate(&p, &subject(), &one, now(), &cfg()) {
            prop_assert_ne!(&f["status"], "confirmed");
        }
    }

    #[test]
    fn confidence_in_range(obs in arb_obs(), s in 0usize..4) {
        let p = prop("payment.methods", STRATS[s], 24.0);
        if let Some(f) = aggregate(&p, &subject(), &obs, now(), &cfg()) {
            let c = f["confidence"].as_f64().unwrap_or(-1.0);
            prop_assert!((0.0..=1.0).contains(&c));
        }
    }

    #[test]
    fn median_permutation_invariant(vals in prop::collection::vec((0u8..5, 1i64..500, 0i64..100), 1..12)) {
        let mut p = prop("serving.quantity", Strategy::Median, 100.0);
        p.field = Some("value".into());
        p.window = Some(5);
        let obs: Vec<_> = vals.iter().enumerate().map(|(i, (ps, v, age))| ob(&format!("ob_{i:02}"), &format!("p{ps}"), json!({"value": v, "unit": "ml"}), *age)).collect();
        let mut rev = obs.clone();
        rev.reverse();
        let a = aggregate(&p, &subject(), &obs, now(), &cfg());
        prop_assert_eq!(&a, &aggregate(&p, &subject(), &rev, now(), &cfg()));
        if let Some(f) = a {
            let c = f["confidence"].as_f64().unwrap_or(-1.0);
            prop_assert!((0.0..=1.0).contains(&c));
        }
    }
}
