//! RFC 9457 problem details.

use poem::http::StatusCode;
use poem_openapi::{ApiResponse, Object, payload::Json};
use serde::Serialize;

/// RFC 9457 problem details (`application/problem+json`).
#[derive(Debug, Clone, PartialEq, Object, Serialize)]
pub struct Problem {
    /// `https://eatsights.letsimpl.fyi/problems/<slug>`
    #[oai(rename = "type")]
    #[serde(rename = "type")]
    pub type_: String,
    pub title: String,
    pub status: u16,
    #[oai(skip_serializing_if_is_none)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Problem {
    pub fn new(status: u16, slug: &str, title: &str, detail: impl Into<Option<String>>) -> Self {
        Self {
            type_: format!("{}{slug}", eatsights_core::PROBLEM_BASE),
            title: title.to_owned(),
            status,
            detail: detail.into(),
        }
    }
}

/// Error response for every endpoint.
#[derive(Debug, ApiResponse)]
#[oai(bad_request_handler = "bad_request")]
pub enum ProblemResp {
    /// Problem details.
    #[oai(content_type = "application/problem+json")]
    Default(StatusCode, Json<Problem>),
}

fn bad_request(e: poem::Error) -> ProblemResp {
    ProblemResp::new(Problem::new(
        400,
        "bad-request",
        "Bad request",
        Some(e.to_string()),
    ))
}

impl ProblemResp {
    pub fn new(p: Problem) -> Self {
        let status = StatusCode::from_u16(p.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        Self::Default(status, Json(p))
    }
    pub fn bad(slug: &str, detail: impl Into<String>) -> Self {
        Self::new(Problem::new(400, slug, "Bad request", Some(detail.into())))
    }
    pub fn not_found(what: &str) -> Self {
        Self::new(Problem::new(
            404,
            "not-found",
            "Not found",
            Some(format!("{what} not found")),
        ))
    }
    pub fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!(error = %e, "internal error");
        Self::new(Problem::new(500, "internal", "Internal error", None))
    }
}

impl From<eatsights_db::DbError> for ProblemResp {
    fn from(e: eatsights_db::DbError) -> Self {
        Self::internal(e)
    }
}
