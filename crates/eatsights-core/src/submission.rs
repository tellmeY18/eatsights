//! Pure checks for observation submissions and the public observation form.
//! Signature and key checks are done by the caller (they need the key store).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::registry::{Registry, Scope, Writer};
use crate::schema;

#[derive(Debug, Clone)]
pub struct CheckConfig {
    /// Oldest accepted `observed_at`, relative to now. Default 30 days.
    pub max_age: Duration,
    /// How far in the future `observed_at` / `submitted_at` may be. Default 5 minutes.
    pub future_allowance: Duration,
    /// How far in the past `submitted_at` may be. Default 15 minutes.
    pub submitted_max_lag: Duration,
}

impl Default for CheckConfig {
    fn default() -> Self {
        Self {
            max_age: Duration::days(30),
            future_allowance: Duration::minutes(5),
            submitted_max_lag: Duration::minutes(15),
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SubmissionError {
    #[error("submission does not match the schema: {0:?}")]
    InvalidEnvelope(Vec<String>),
    #[error("unknown property `{0}`")]
    UnknownProperty(String),
    #[error("property `{property}` has scope `{expected}` but subject scope is `{actual}`")]
    ScopeMismatch {
        property: String,
        expected: &'static str,
        actual: String,
    },
    #[error("source_type `{source_type}` may not write property `{property}`")]
    WriterNotAllowed {
        property: String,
        source_type: String,
    },
    #[error("value does not match the property's value_schema: {0:?}")]
    InvalidValue(Vec<String>),
    #[error("invalid timestamp in `{0}`")]
    BadTimestamp(&'static str),
    #[error("observed_at is in the future")]
    ObservedInFuture,
    #[error("observed_at is older than the maximum age")]
    ObservedTooOld,
    #[error("observed_at is after submitted_at")]
    ObservedAfterSubmitted,
    #[error("submitted_at is too far from server time")]
    SubmittedAtSkew,
}

impl SubmissionError {
    /// Problem slug, appended to [`crate::PROBLEM_BASE`].
    pub fn slug(&self) -> &'static str {
        match self {
            Self::InvalidEnvelope(_) => "invalid-submission",
            Self::UnknownProperty(_) => "unknown-property",
            Self::ScopeMismatch { .. } => "scope-mismatch",
            Self::WriterNotAllowed { .. } => "writer-not-allowed",
            Self::InvalidValue(_) => "invalid-value",
            Self::BadTimestamp(_) => "invalid-submission",
            Self::ObservedInFuture => "observed-in-future",
            Self::ObservedTooOld => "observed-too-old",
            Self::ObservedAfterSubmitted => "observed-after-submitted",
            Self::SubmittedAtSkew => "submitted-at-skew",
        }
    }

    /// Short human title for the problem document.
    pub fn title(&self) -> &'static str {
        match self {
            Self::InvalidEnvelope(_) | Self::BadTimestamp(_) => "Invalid submission",
            Self::UnknownProperty(_) => "Unknown property",
            Self::ScopeMismatch { .. } => "Scope mismatch",
            Self::WriterNotAllowed { .. } => "Writer not allowed",
            Self::InvalidValue(_) => "Invalid value",
            Self::ObservedInFuture => "Observed in the future",
            Self::ObservedTooOld => "Observation too old",
            Self::ObservedAfterSubmitted => "Observed after submitted",
            Self::SubmittedAtSkew => "Clock skew",
        }
    }

    /// HTTP status for the problem document.
    pub fn status(&self) -> u16 {
        422
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Customer,
    Vendor,
    Import,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Customer => "customer",
            Self::Vendor => "vendor",
            Self::Import => "import",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "customer" => Some(Self::Customer),
            "vendor" => Some(Self::Vendor),
            "import" => Some(Self::Import),
            _ => None,
        }
    }

    fn writer(self) -> Writer {
        match self {
            Self::Customer => Writer::Customer,
            Self::Vendor => Writer::Vendor,
            Self::Import => Writer::Import,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Value,
    Confirm,
    Dispute,
}

/// Public contributor class (`observation.contributor.class`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributorClass {
    Unverified,
    Member,
    Trusted,
    Verifier,
    Vendor,
    Import,
}

impl ContributorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Member => "member",
            Self::Trusted => "trusted",
            Self::Verifier => "verifier",
            Self::Vendor => "vendor",
            Self::Import => "import",
        }
    }
}

/// Fields extracted from a submission that passed [`check_submission`].
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedSubmission {
    pub id: String,
    pub property: String,
    pub kind: Kind,
    pub scope: Scope,
    pub place_id: Option<String>,
    pub menu_item_id: Option<String>,
    pub dish_id: Option<String>,
    /// Pseudonym anchor: `place_id` (place / menu_item scope) or `dish_id` (dish scope).
    pub anchor: String,
    pub key_id: String,
    pub source_type: SourceType,
    pub refers_to: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub submitted_at: DateTime<Utc>,
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(Value::as_str)
}

fn time_at(v: &Value, field: &'static str) -> Result<DateTime<Utc>, SubmissionError> {
    str_at(v, &format!("/{field}"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or(SubmissionError::BadTimestamp(field))
}

fn invalid(msg: &str) -> SubmissionError {
    SubmissionError::InvalidEnvelope(vec![msg.to_owned()])
}

/// Steps 1, 4 and 5 of the write path (AGENTS.md section 7): envelope, registry, time.
pub fn check_submission(
    sub: &Value,
    registry: &Registry,
    now: DateTime<Utc>,
    cfg: &CheckConfig,
) -> Result<CheckedSubmission, SubmissionError> {
    schema::validate_def("submission", sub).map_err(SubmissionError::InvalidEnvelope)?;

    let property_id = str_at(sub, "/property").ok_or_else(|| invalid("missing property"))?;
    let prop = registry
        .get(property_id)
        .ok_or_else(|| SubmissionError::UnknownProperty(property_id.to_owned()))?;

    let scope_str =
        str_at(sub, "/subject/scope").ok_or_else(|| invalid("missing subject.scope"))?;
    let scope = Scope::parse(scope_str).ok_or_else(|| invalid("bad subject.scope"))?;
    if scope != prop.scope {
        return Err(SubmissionError::ScopeMismatch {
            property: prop.id.clone(),
            expected: prop.scope.as_str(),
            actual: scope_str.to_owned(),
        });
    }

    let st_str = str_at(sub, "/source_type").ok_or_else(|| invalid("missing source_type"))?;
    let source_type = SourceType::parse(st_str).ok_or_else(|| invalid("bad source_type"))?;
    if !prop.allows_writer(source_type.writer()) {
        return Err(SubmissionError::WriterNotAllowed {
            property: prop.id.clone(),
            source_type: st_str.to_owned(),
        });
    }

    let kind: Kind = sub
        .get("kind")
        .cloned()
        .and_then(|k| serde_json::from_value(k).ok())
        .ok_or_else(|| invalid("bad kind"))?;
    if let Some(value) = sub.get("value") {
        registry
            .validate_value(&prop.id, value)
            .map_err(SubmissionError::InvalidValue)?;
    }

    let observed_at = time_at(sub, "observed_at")?;
    let submitted_at = time_at(sub, "submitted_at")?;
    if observed_at > now + cfg.future_allowance {
        return Err(SubmissionError::ObservedInFuture);
    }
    if observed_at < now - cfg.max_age {
        return Err(SubmissionError::ObservedTooOld);
    }
    if observed_at > submitted_at + cfg.future_allowance {
        return Err(SubmissionError::ObservedAfterSubmitted);
    }
    if submitted_at > now + cfg.future_allowance || submitted_at < now - cfg.submitted_max_lag {
        return Err(SubmissionError::SubmittedAtSkew);
    }

    let place_id = str_at(sub, "/subject/place_id").map(str::to_owned);
    let menu_item_id = str_at(sub, "/subject/menu_item_id").map(str::to_owned);
    let dish_id = str_at(sub, "/subject/dish_id").map(str::to_owned);
    let anchor = match scope {
        Scope::Dish => dish_id.clone(),
        Scope::Place | Scope::MenuItem => place_id.clone(),
    }
    .ok_or_else(|| invalid("missing subject anchor id"))?;

    Ok(CheckedSubmission {
        id: str_at(sub, "/id")
            .ok_or_else(|| invalid("missing id"))?
            .to_owned(),
        property: prop.id.clone(),
        kind,
        scope,
        place_id,
        menu_item_id,
        dish_id,
        anchor,
        key_id: str_at(sub, "/contributor/key_id")
            .ok_or_else(|| invalid("missing contributor.key_id"))?
            .to_owned(),
        source_type,
        refers_to: str_at(sub, "/refers_to").map(str::to_owned),
        observed_at,
        submitted_at,
    })
}

/// Build the public observation: drop `signature`, replace `contributor` with
/// `{class, pseudonym, checkin_verified?}`. Everything else is copied.
pub fn to_public_observation(sub: &Value, class: ContributorClass, pseudonym: &str) -> Value {
    let mut out = Map::new();
    if let Some(obj) = sub.as_object() {
        for (k, v) in obj {
            if k != "signature" && k != "contributor" {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    let mut contributor = Map::new();
    contributor.insert("class".into(), Value::String(class.as_str().into()));
    contributor.insert("pseudonym".into(), Value::String(pseudonym.into()));
    if let Some(c) = sub.pointer("/contributor/checkin_verified") {
        contributor.insert("checkin_verified".into(), c.clone());
    }
    out.insert("contributor".into(), Value::Object(contributor));
    Value::Object(out)
}
