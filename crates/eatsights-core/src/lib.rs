//! Eatsights core: ids, canonical JSON, signatures, schema and registry validation.
//! No I/O apart from files embedded at build time.

pub mod crypto;
pub mod geo;
pub mod ids;
pub mod jcs;
pub mod registry;
pub mod schema;
pub mod submission;
pub mod trust;

/// Public base address. The binary may override it at runtime with `EATSIGHTS_BASE_URL`.
pub const BASE_URL: &str = "https://eatsights.letsimpl.fyi";

/// Schema version of the embedded contract files.
pub const SCHEMA_VERSION: &str = "0.3";

/// Prefix of problem `type` URIs (RFC 9457).
pub const PROBLEM_BASE: &str = "https://eatsights.letsimpl.fyi/problems/";
