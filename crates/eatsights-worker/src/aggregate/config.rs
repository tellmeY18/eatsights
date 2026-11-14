use std::path::Path;

use serde::Deserialize;

use super::ContributorClass;

/// Embedded default configuration.
pub const DEFAULT_CONFIG: &str = include_str!("../../aggregation.toml");

/// Environment variable naming an override config file.
pub const CONFIG_ENV: &str = "EATSIGHTS_AGGREGATION_CONFIG";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggConfig {
    pub algorithm_version: String,
    pub weights: ClassWeights,
    pub thresholds: Thresholds,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassWeights {
    pub unverified: f64,
    pub member: f64,
    pub trusted: f64,
    pub verifier: f64,
    pub vendor: f64,
    pub import: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    pub min_distinct_to_confirm: u32,
    pub confirm_confidence: f64,
    pub confidence_prior: f64,
    pub contested_ratio: f64,
    pub stale_half_lives: f64,
    pub stale_min_support: f64,
    pub dispute_factor: f64,
    pub median_tolerance: f64,
    pub basis_cap: usize,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Invalid(&'static str),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "reading aggregation config: {e}"),
            Self::Parse(e) => write!(f, "parsing aggregation config: {e}"),
            Self::Invalid(m) => write!(f, "invalid aggregation config: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl AggConfig {
    /// The embedded default.
    pub fn default_config() -> Result<Self, ConfigError> {
        Self::parse(DEFAULT_CONFIG)
    }

    /// Load from `path`, or the embedded default when `None`.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        match path {
            Some(p) => Self::parse(&std::fs::read_to_string(p).map_err(ConfigError::Io)?),
            None => Self::default_config(),
        }
    }

    /// Load from the path in `EATSIGHTS_AGGREGATION_CONFIG`, else the default.
    pub fn from_env() -> Result<Self, ConfigError> {
        match std::env::var_os(CONFIG_ENV) {
            Some(p) if !p.is_empty() => Self::load(Some(Path::new(&p))),
            _ => Self::default_config(),
        }
    }

    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(s).map_err(ConfigError::Parse)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let w = &self.weights;
        let all = [
            w.unverified,
            w.member,
            w.trusted,
            w.verifier,
            w.vendor,
            w.import,
        ];
        if all.iter().any(|x| !x.is_finite() || *x <= 0.0) {
            return Err(ConfigError::Invalid("weights must be finite and > 0"));
        }
        let t = &self.thresholds;
        if t.min_distinct_to_confirm < 2 {
            return Err(ConfigError::Invalid("min_distinct_to_confirm must be >= 2"));
        }
        let pos = [
            t.confidence_prior,
            t.contested_ratio,
            t.stale_half_lives,
            t.dispute_factor,
            t.median_tolerance,
        ];
        if pos.iter().any(|x| !x.is_finite() || *x <= 0.0) {
            return Err(ConfigError::Invalid("thresholds must be finite and > 0"));
        }
        if !(0.0..=1.0).contains(&t.confirm_confidence) || t.stale_min_support < 0.0 {
            return Err(ConfigError::Invalid(
                "confidence/support thresholds out of range",
            ));
        }
        if t.basis_cap == 0 {
            return Err(ConfigError::Invalid("basis_cap must be > 0"));
        }
        if self.algorithm_version.is_empty() {
            return Err(ConfigError::Invalid("algorithm_version is empty"));
        }
        Ok(())
    }

    pub fn weight(&self, class: ContributorClass) -> f64 {
        let w = &self.weights;
        match class {
            ContributorClass::Unverified => w.unverified,
            ContributorClass::Member => w.member,
            ContributorClass::Trusted => w.trusted,
            ContributorClass::Verifier => w.verifier,
            ContributorClass::Vendor => w.vendor,
            ContributorClass::Import => w.import,
        }
    }
}
