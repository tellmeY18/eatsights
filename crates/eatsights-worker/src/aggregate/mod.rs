//! Pure aggregation: observations of one (subject, property) in, one `fact` out.
//!
//! See `docs/adr/0003-aggregation.md` for the algorithm and the numbers.
//! Everything here is deterministic and independent of input order.

mod config;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub use config::{AggConfig, CONFIG_ENV, ClassWeights, ConfigError, DEFAULT_CONFIG, Thresholds};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Value,
    Confirm,
    Dispute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributorClass {
    Unverified,
    Member,
    Trusted,
    Verifier,
    Vendor,
    Import,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Customer,
    Vendor,
    Import,
}

impl SourceType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Customer => "customer",
            Self::Vendor => "vendor",
            Self::Import => "import",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    LatestWeighted,
    Median,
    Mode,
    Union,
    Schedule,
    Derived,
}

/// One public observation, already redirect-resolved (`merged_into` followed by the caller).
#[derive(Debug, Clone, PartialEq)]
pub struct ObsInput {
    pub id: String,
    pub kind: Kind,
    pub value: Option<Value>,
    pub refers_to: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub class: ContributorClass,
    pub pseudonym: String,
    pub source_type: SourceType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyDef {
    pub id: String,
    pub half_life_hours: f64,
    pub strategy: Strategy,
    pub field: Option<String>,
    pub window: Option<u32>,
}

/// RFC 8785-style canonical JSON (sorted keys, no whitespace). Used for value equality and tie-breaks.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            out.push('{');
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                if let Some(x) = m.get(k) {
                    write_canonical(x, out);
                }
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

/// Aggregate the observations of one (subject, property) into a `fact`
/// (schema.json `#/$defs/fact`). Returns `None` when there is nothing to report
/// (no usable value observations, or a `derived` property, which the MVP does not compute).
pub fn aggregate(
    prop: &PropertyDef,
    subject: &Value,
    obs: &[ObsInput],
    now: DateTime<Utc>,
    cfg: &AggConfig,
) -> Option<Value> {
    if !(prop.half_life_hours.is_finite() && prop.half_life_hours > 0.0) || obs.is_empty() {
        return None;
    }
    let mut sorted: Vec<&ObsInput> = obs.iter().collect();
    sorted.sort_by(|a, b| obs_order(a, b));
    sorted.dedup_by(|a, b| a.id == b.id);
    let ctx = Ctx {
        prop,
        obs: sorted,
        now,
        cfg,
    };
    let outcome = match prop.strategy {
        Strategy::LatestWeighted => ctx.grouped(true, false),
        Strategy::Mode => ctx.grouped(false, false),
        Strategy::Schedule => ctx.grouped(true, true),
        Strategy::Median => ctx.median(),
        Strategy::Union => ctx.union(),
        Strategy::Derived => None,
    }?;
    Some(ctx.build_fact(subject, outcome))
}

fn obs_order(a: &ObsInput, b: &ObsInput) -> std::cmp::Ordering {
    a.observed_at
        .cmp(&b.observed_at)
        .then_with(|| a.id.cmp(&b.id))
        .then_with(|| a.pseudonym.cmp(&b.pseudonym))
}

struct Ctx<'a> {
    prop: &'a PropertyDef,
    obs: Vec<&'a ObsInput>,
    now: DateTime<Utc>,
    cfg: &'a AggConfig,
}

/// Strategy result, before status and serialisation.
struct Outcome {
    value: Value,
    /// Decayed, pseudonym-deduplicated support of the winner.
    support: f64,
    /// Pseudonym -> class of everyone backing the winner.
    supporters: BTreeMap<String, ContributorClass>,
    disputers: BTreeSet<String>,
    confidence: f64,
    contested: bool,
    /// Extra strategy-specific gate for `confirmed` (beyond distinct count and confidence).
    confirmable: bool,
    last_supported_at: Option<DateTime<Utc>>,
    last_confirmed_at: Option<DateTime<Utc>>,
    basis: BTreeSet<String>,
    vendor_value_differs: bool,
}

#[derive(Default)]
struct Group {
    value: Value,
    /// pseudonym -> (weight, class, obs id, observed_at, is_confirm, source)
    votes: BTreeMap<String, Vote>,
    disputes: BTreeMap<String, (f64, String)>,
}

#[derive(Clone)]
struct Vote {
    weight: f64,
    class: ContributorClass,
    id: String,
    at: DateTime<Utc>,
    confirm: bool,
    source: SourceType,
}

impl Group {
    fn support(&self) -> f64 {
        self.votes.values().map(|v| v.weight).sum()
    }
    fn customer_support(&self) -> f64 {
        self.votes
            .values()
            .filter(|v| v.source == SourceType::Customer)
            .map(|v| v.weight)
            .sum()
    }
    fn dispute(&self) -> f64 {
        self.disputes.values().map(|d| d.0).sum()
    }
}

impl Ctx<'_> {
    fn decay(&self, at: DateTime<Utc>) -> f64 {
        let age_h = (self.now - at).num_milliseconds().max(0) as f64 / 3_600_000.0;
        0.5_f64.powf(age_h / self.prop.half_life_hours)
    }

    fn weight(&self, o: &ObsInput, decayed: bool) -> f64 {
        let w = self.cfg.weight(o.class);
        if decayed {
            w * self.decay(o.observed_at)
        } else {
            w
        }
    }

    fn by_id(&self) -> BTreeMap<&str, &ObsInput> {
        self.obs.iter().map(|o| (o.id.as_str(), *o)).collect()
    }

    /// The value an observation backs: its own value, or that of the observation it confirms.
    fn backed_value<'b>(
        o: &'b ObsInput,
        by_id: &BTreeMap<&str, &'b ObsInput>,
    ) -> Option<(&'b Value, bool)> {
        match o.kind {
            Kind::Value | Kind::Dispute => o.value.as_ref().map(|v| (v, false)),
            Kind::Confirm => {
                let target = by_id.get(o.refers_to.as_deref()?)?;
                if target.kind == Kind::Confirm {
                    return None;
                }
                target.value.as_ref().map(|v| (v, true))
            }
        }
    }

    /// Each pseudonym's latest backing observation (value, confirm, or dispute with a counter-value).
    fn latest_votes(&self) -> BTreeMap<String, (&ObsInput, &Value, bool)> {
        let by_id = self.by_id();
        let mut out = BTreeMap::new();
        for o in &self.obs {
            if let Some((v, confirm)) = Self::backed_value(o, &by_id) {
                // obs are sorted ascending, so later inserts win
                out.insert(o.pseudonym.clone(), (*o, v, confirm));
            }
        }
        out
    }

    /// Disputes: pseudonym -> list of (disputed value canonical, weight, obs id).
    fn disputes(&self, decayed: bool) -> Vec<(String, String, f64, String)> {
        let by_id = self.by_id();
        let mut out = Vec::new();
        for o in &self.obs {
            if o.kind != Kind::Dispute {
                continue;
            }
            let Some(target) = o.refers_to.as_deref().and_then(|r| by_id.get(r)) else {
                continue;
            };
            let Some(tv) = target.value.as_ref() else {
                continue;
            };
            out.push((
                o.pseudonym.clone(),
                canonical_json(tv),
                self.weight(o, decayed),
                o.id.clone(),
            ));
        }
        out
    }

    fn build_groups(&self, decayed: bool) -> BTreeMap<String, Group> {
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        for (pseud, (o, v, confirm)) in self.latest_votes() {
            let g = groups.entry(canonical_json(v)).or_default();
            g.value = v.clone();
            g.votes.insert(
                pseud,
                Vote {
                    weight: self.weight(o, decayed),
                    class: o.class,
                    id: o.id.clone(),
                    at: o.observed_at,
                    confirm,
                    source: o.source_type,
                },
            );
        }
        for (pseud, key, w, id) in self.disputes(decayed) {
            if let Some(g) = groups.get_mut(&key) {
                if g.votes.contains_key(&pseud) {
                    continue;
                }
                let e = g.disputes.entry(pseud).or_insert((0.0, id.clone()));
                if w > e.0 || (w == e.0 && id < e.1) {
                    *e = (w, id);
                }
            }
        }
        groups
    }

    fn vendor_differs(&self, groups: &BTreeMap<String, Group>) -> bool {
        let vendor_latest = self
            .obs
            .iter()
            .rev()
            .find(|o| o.source_type == SourceType::Vendor && o.kind == Kind::Value)
            .and_then(|o| o.value.as_ref())
            .map(canonical_json);
        let Some(vendor_key) = vendor_latest else {
            return false;
        };
        let mut best: Option<(&String, f64)> = None;
        for (k, g) in groups {
            let s = g.customer_support();
            if s > 0.0 && best.is_none_or(|(_, b)| s > b) {
                best = Some((k, s));
            }
        }
        best.is_some_and(|(k, _)| *k != vendor_key)
    }

    fn grouped(&self, decayed: bool, schedule: bool) -> Option<Outcome> {
        let t = &self.cfg.thresholds;
        let groups = self.build_groups(decayed);
        let net = |g: &Group| g.support() - t.dispute_factor * g.dispute();
        let winner_key = if schedule {
            // newest declared rule wins
            self.obs
                .iter()
                .rev()
                .find(|o| o.kind == Kind::Value && o.value.is_some())
                .and_then(|o| o.value.as_ref())
                .map(canonical_json)
                .filter(|k| groups.contains_key(k))
        } else {
            let mut best: Option<(&String, f64)> = None;
            for (k, g) in &groups {
                let n = net(g);
                if best.is_none_or(|(_, b)| n > b) {
                    best = Some((k, n));
                }
            }
            best.map(|(k, _)| k.clone())
        }?;
        let winner = groups.get(&winner_key)?;
        let support = winner.support();
        let dispute = winner.dispute();
        let total: f64 = groups.values().map(Group::support).sum();
        let runner_up = groups
            .iter()
            .filter(|(k, _)| **k != winner_key)
            .map(|(_, g)| g.support())
            .fold(0.0, f64::max);
        let share =
            net(winner).max(0.0) / (total + t.dispute_factor * dispute).max(f64::MIN_POSITIVE);
        let confidence = share * total / (total + t.confidence_prior);
        let contested = support > 0.0
            && (t.dispute_factor * dispute >= t.contested_ratio * support
                || runner_up >= t.contested_ratio * support);
        let mut basis: BTreeSet<String> = winner.votes.values().map(|v| v.id.clone()).collect();
        basis.extend(winner.disputes.values().map(|d| d.1.clone()));
        Some(Outcome {
            value: winner.value.clone(),
            support: if decayed {
                support
            } else {
                self.decayed_support(winner)
            },
            supporters: winner
                .votes
                .iter()
                .map(|(p, v)| (p.clone(), v.class))
                .collect(),
            disputers: winner.disputes.keys().cloned().collect(),
            confidence,
            contested,
            confirmable: true,
            last_supported_at: winner.votes.values().map(|v| v.at).max(),
            last_confirmed_at: winner
                .votes
                .values()
                .filter(|v| v.confirm)
                .map(|v| v.at)
                .max(),
            basis,
            vendor_value_differs: self.vendor_differs(&groups),
        })
    }

    fn decayed_support(&self, g: &Group) -> f64 {
        g.votes.values().map(|v| v.weight * self.decay(v.at)).sum()
    }

    fn median(&self) -> Option<Outcome> {
        let t = &self.cfg.thresholds;
        let field = self.prop.field.as_deref()?;
        let window = self.prop.window.unwrap_or(10).max(1) as usize;
        // one value per pseudonym (its latest), then the newest `window` of those
        let mut votes: Vec<(&String, &ObsInput, &Value, bool)> = Vec::new();
        let latest = self.latest_votes();
        for (p, (o, v, c)) in &latest {
            votes.push((p, *o, *v, *c));
        }
        votes.sort_by(|a, b| obs_order(b.1, a.1));
        votes.truncate(window);

        // partition by the non-numeric remainder (unit, stage, day_type ...)
        struct Part<'b> {
            items: Vec<(f64, f64, &'b String, &'b ObsInput, bool)>,
            template: Value,
        }
        let mut parts: BTreeMap<String, Part<'_>> = BTreeMap::new();
        for (p, o, v, c) in votes {
            let Some(obj) = v.as_object() else { continue };
            let Some(x) = obj.get(field).and_then(numeric) else {
                continue;
            };
            let mut rest = obj.clone();
            rest.remove(field);
            let key = canonical_json(&Value::Object(rest));
            let part = parts.entry(key).or_insert_with(|| Part {
                items: Vec::new(),
                template: v.clone(),
            });
            part.items.push((x, self.weight(o, true), p, o, c));
        }
        let mut best: Option<(&String, f64)> = None;
        for (k, part) in &parts {
            let s: f64 = part.items.iter().map(|i| i.1).sum();
            if best.is_none_or(|(_, b)| s > b) {
                best = Some((k, s));
            }
        }
        let (best_key, _) = best?;
        let part = parts.get(best_key)?;
        let total_all: f64 = parts
            .values()
            .flat_map(|p| p.items.iter().map(|i| i.1))
            .sum();

        let mut items = part.items.clone();
        items.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.3.id.cmp(&b.3.id)));
        let part_total: f64 = items.iter().map(|i| i.1).sum();
        let mut acc = 0.0;
        let mut med = items.first()?.0;
        for i in &items {
            acc += i.1;
            if acc * 2.0 >= part_total {
                med = i.0;
                break;
            }
        }
        let tol = (med.abs() * t.median_tolerance).max(t.median_tolerance);
        let agreeing: Vec<_> = items.iter().filter(|i| (i.0 - med).abs() <= tol).collect();
        let agree_w: f64 = agreeing.iter().map(|i| i.1).sum();
        let disagree_w = total_all - agree_w;

        // disputes against agreeing observations
        let agree_ids: BTreeSet<&str> = agreeing.iter().map(|i| i.3.id.as_str()).collect();
        let agree_ps: BTreeSet<&str> = agreeing.iter().map(|i| i.2.as_str()).collect();
        let by_id = self.by_id();
        let mut disputers: BTreeMap<String, (f64, String)> = BTreeMap::new();
        for o in &self.obs {
            if o.kind != Kind::Dispute || agree_ps.contains(o.pseudonym.as_str()) {
                continue;
            }
            let Some(r) = o.refers_to.as_deref() else {
                continue;
            };
            let refs_agreeing = agree_ids.contains(r)
                || by_id.get(r).is_some_and(|t| {
                    t.kind == Kind::Value && agreeing.iter().any(|i| i.3.id == t.id)
                });
            if refs_agreeing {
                let w = self.weight(o, true);
                let e = disputers
                    .entry(o.pseudonym.clone())
                    .or_insert((0.0, o.id.clone()));
                if w > e.0 {
                    *e = (w, o.id.clone());
                }
            }
        }
        let dispute: f64 = disputers.values().map(|d| d.0).sum();
        let denom = (total_all + t.dispute_factor * dispute).max(f64::MIN_POSITIVE);
        let share = (agree_w - t.dispute_factor * dispute).max(0.0) / denom;
        let confidence = share * total_all / (total_all + t.confidence_prior);
        let contested = agree_w > 0.0
            && (disagree_w >= t.contested_ratio * agree_w
                || t.dispute_factor * dispute >= t.contested_ratio * agree_w);

        let mut value = part.template.clone();
        if let Some(obj) = value.as_object_mut() {
            let orig = obj.get(field).cloned().unwrap_or(Value::Null);
            obj.insert(field.to_string(), format_like(&orig, med));
        }
        let mut basis: BTreeSet<String> = items.iter().map(|i| i.3.id.clone()).collect();
        basis.extend(disputers.values().map(|d| d.1.clone()));

        // vendor differs: vendor's latest value outside tolerance of the customer median
        let vendor_value_differs = self
            .obs
            .iter()
            .rev()
            .find(|o| o.source_type == SourceType::Vendor && o.kind == Kind::Value)
            .and_then(|o| o.value.as_ref())
            .and_then(|v| {
                let obj = v.as_object()?;
                let x = obj.get(field).and_then(numeric)?;
                let has_customer = items
                    .iter()
                    .any(|i| i.3.source_type == SourceType::Customer);
                let mut rest = obj.clone();
                rest.remove(field);
                let same_part = canonical_json(&Value::Object(rest)) == *best_key;
                Some(has_customer && (!same_part || (x - med).abs() > tol))
            })
            .unwrap_or(false);

        Some(Outcome {
            value,
            support: agree_w,
            supporters: agreeing.iter().map(|i| (i.2.clone(), i.3.class)).collect(),
            disputers: disputers.keys().cloned().collect(),
            confidence,
            contested,
            confirmable: true,
            last_supported_at: agreeing.iter().map(|i| i.3.observed_at).max(),
            last_confirmed_at: agreeing
                .iter()
                .filter(|i| i.4)
                .map(|i| i.3.observed_at)
                .max(),
            basis,
            vendor_value_differs,
        })
    }

    fn union(&self) -> Option<Outcome> {
        let t = &self.cfg.thresholds;
        let latest = self.latest_votes();
        // the array-valued field, taken from the newest vote that has one
        let mut newest: Vec<_> = latest.values().collect();
        newest.sort_by(|a, b| obs_order(b.0, a.0));
        let field = newest.iter().find_map(|(_, v, _)| {
            v.as_object()?
                .iter()
                .filter(|(_, x)| x.is_array())
                .map(|(k, _)| k.clone())
                .min()
        })?;

        let mut elems: BTreeMap<String, UnionElem> = BTreeMap::new();
        let mut voters: BTreeMap<String, (ContributorClass, f64, &ObsInput, bool)> =
            BTreeMap::new();
        for (p, (o, v, c)) in &latest {
            let Some(arr) = v.get(&field).and_then(Value::as_array) else {
                continue;
            };
            let w = self.weight(o, true);
            voters.insert(p.clone(), (o.class, w, *o, *c));
            for e in arr {
                let entry = elems
                    .entry(canonical_json(e))
                    .or_insert_with(|| (e.clone(), BTreeMap::new(), 0.0));
                entry.1.insert(p.clone(), w);
            }
        }
        if voters.is_empty() {
            return None;
        }
        // disputes reduce the elements of the observation they dispute
        let by_id = self.by_id();
        let mut disputers: BTreeMap<String, (f64, String)> = BTreeMap::new();
        let mut per_elem_disp: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
        for o in &self.obs {
            if o.kind != Kind::Dispute {
                continue;
            }
            let Some(target) = o.refers_to.as_deref().and_then(|r| by_id.get(r)) else {
                continue;
            };
            let Some(arr) = target
                .value
                .as_ref()
                .and_then(|v| v.get(&field))
                .and_then(Value::as_array)
            else {
                continue;
            };
            let w = self.weight(o, true);
            for e in arr {
                let k = canonical_json(e);
                let supported_by_self = elems
                    .get(&k)
                    .is_some_and(|x| x.1.contains_key(&o.pseudonym));
                if supported_by_self {
                    continue;
                }
                let d = per_elem_disp.entry(k).or_default();
                let cur = d.entry(o.pseudonym.clone()).or_insert(0.0);
                *cur = cur.max(w);
            }
            let e = disputers
                .entry(o.pseudonym.clone())
                .or_insert((0.0, o.id.clone()));
            if w > e.0 {
                *e = (w, o.id.clone());
            }
        }
        for (k, d) in per_elem_disp {
            if let Some(e) = elems.get_mut(&k) {
                e.2 = d.values().sum();
            }
        }

        let mut included: Vec<(&String, &UnionElem)> = elems
            .iter()
            .filter(|(_, e)| e.1.values().sum::<f64>() - t.dispute_factor * e.2 > 0.0)
            .collect();
        included.sort_by(|a, b| a.0.cmp(b.0));
        let total: f64 = voters.values().map(|v| v.1).sum();
        let (confidence, contested, confirmable) = if included.is_empty() {
            (total / (total + t.confidence_prior), false, true)
        } else {
            let mut sum = 0.0;
            let mut contested = false;
            let mut confirmable = true;
            for (_, e) in &included {
                let s: f64 = e.1.values().sum();
                let net = s - t.dispute_factor * e.2;
                sum += net / (s + t.dispute_factor * e.2) * s / (s + t.confidence_prior);
                contested |= t.dispute_factor * e.2 >= t.contested_ratio * s;
                confirmable &= e.1.len() >= t.min_distinct_to_confirm as usize;
            }
            (sum / included.len() as f64, contested, confirmable)
        };
        let mut obj = Map::new();
        obj.insert(
            field,
            Value::Array(included.iter().map(|(_, e)| e.0.clone()).collect()),
        );
        let mut basis: BTreeSet<String> = voters.values().map(|v| v.2.id.clone()).collect();
        basis.extend(disputers.values().map(|d| d.1.clone()));
        let value = Value::Object(obj);

        let vendor_value_differs = self
            .obs
            .iter()
            .rev()
            .find(|o| o.source_type == SourceType::Vendor && o.kind == Kind::Value)
            .and_then(|o| o.value.as_ref())
            .is_some_and(|v| {
                let has_customer = voters
                    .values()
                    .any(|x| x.2.source_type == SourceType::Customer);
                has_customer && canonical_json(v) != canonical_json(&value)
            });

        Some(Outcome {
            value,
            support: total,
            supporters: voters.iter().map(|(p, v)| (p.clone(), v.0)).collect(),
            disputers: disputers.keys().cloned().collect(),
            confidence,
            contested,
            confirmable,
            last_supported_at: voters.values().map(|v| v.2.observed_at).max(),
            last_confirmed_at: voters
                .values()
                .filter(|v| v.3)
                .map(|v| v.2.observed_at)
                .max(),
            basis,
            vendor_value_differs,
        })
    }

    fn build_fact(&self, subject: &Value, o: Outcome) -> Value {
        let t = &self.cfg.thresholds;
        let confidence = if o.confidence.is_finite() {
            o.confidence.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let distinct_supporters = o.supporters.len();
        let stale = match o.last_supported_at {
            None => true,
            Some(at) => {
                let age_h = (self.now - at).num_milliseconds().max(0) as f64 / 3_600_000.0;
                age_h > t.stale_half_lives * self.prop.half_life_hours
                    || o.support < t.stale_min_support
            }
        };
        // Hard rule: never confirmed with fewer than 2 distinct pseudonyms, whatever the config says.
        let min_distinct = (t.min_distinct_to_confirm as usize).max(2);
        let status = if stale {
            "stale"
        } else if o.contested {
            "contested"
        } else if distinct_supporters >= min_distinct
            && o.confirmable
            && confidence >= t.confirm_confidence
        {
            "confirmed"
        } else {
            "unconfirmed"
        };

        let all_pseudonyms: BTreeSet<&str> =
            self.obs.iter().map(|x| x.pseudonym.as_str()).collect();
        let trusted = o
            .supporters
            .values()
            .filter(|c| matches!(c, ContributorClass::Trusted | ContributorClass::Verifier))
            .count();
        let basis_set: BTreeSet<&str> = o.basis.iter().map(String::as_str).collect();
        let sources: BTreeSet<SourceType> = self
            .obs
            .iter()
            .filter(|x| basis_set.contains(x.id.as_str()))
            .map(|x| x.source_type)
            .collect();
        let last_observed = self
            .obs
            .iter()
            .map(|x| x.observed_at)
            .max()
            .unwrap_or(self.now);

        let mut fact = Map::new();
        fact.insert("subject".into(), subject.clone());
        fact.insert("property".into(), Value::String(self.prop.id.clone()));
        fact.insert("current_value".into(), o.value);
        fact.insert("status".into(), Value::String(status.into()));
        fact.insert("confidence".into(), json!(round6(confidence)));
        fact.insert(
            "counts".into(),
            json!({
                "confirmations": distinct_supporters,
                "disputes": o.disputers.len(),
                "distinct_contributors": all_pseudonyms.len(),
                "trusted_confirmations": trusted,
            }),
        );
        fact.insert(
            "sources".into(),
            Value::Array(
                sources
                    .iter()
                    .map(|s| Value::String(s.as_str().into()))
                    .collect(),
            ),
        );
        fact.insert(
            "vendor_value_differs".into(),
            Value::Bool(o.vendor_value_differs),
        );
        fact.insert(
            "last_observed_at".into(),
            Value::String(rfc3339(last_observed)),
        );
        let last_confirmed = o.last_confirmed_at.or(if status == "confirmed" {
            o.last_supported_at
        } else {
            None
        });
        if let Some(at) = last_confirmed {
            fact.insert("last_confirmed_at".into(), Value::String(rfc3339(at)));
        }
        fact.insert("half_life_hours".into(), json!(self.prop.half_life_hours));
        fact.insert(
            "basis".into(),
            Value::Array(
                o.basis
                    .into_iter()
                    .take(t.basis_cap)
                    .map(Value::String)
                    .collect(),
            ),
        );
        fact.insert("computed_at".into(), Value::String(rfc3339(self.now)));
        fact.insert(
            "algorithm_version".into(),
            Value::String(self.cfg.algorithm_version.clone()),
        );
        Value::Object(fact)
    }
}

/// (element value, pseudonym -> support weight, dispute weight)
type UnionElem = (Value, BTreeMap<String, f64>, f64);

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Numeric view of a median field: numbers, or "HH:MM" as minutes since midnight.
fn numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        Value::String(s) => {
            let (h, m) = s.split_once(':')?;
            let h: u32 = h.parse().ok()?;
            let m: u32 = m.parse().ok()?;
            (h < 24 && m < 60).then_some(f64::from(h * 60 + m))
        }
        _ => None,
    }
}

/// Render a median back in the shape of the original field.
fn format_like(orig: &Value, x: f64) -> Value {
    match orig {
        Value::String(_) => {
            let mins = (x.round() as i64).rem_euclid(24 * 60);
            Value::String(format!("{:02}:{:02}", mins / 60, mins % 60))
        }
        Value::Number(n) if n.is_i64() || n.is_u64() => json!(x.round() as i64),
        _ => json!(x),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
