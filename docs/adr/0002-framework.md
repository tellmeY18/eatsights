# ADR-0002: HTTP framework — Poem + poem-openapi

- Status: Accepted (D2), with items marked **to confirm** pending spike/measurement
- Date: 2026-11-14

## Context

AGENTS.md D2 fixes Rust with Poem and `poem-openapi`; an OpenAPI document is a hard requirement. The real contract is the JSON Schema 2020-12 files (`schema.json`, `property.schema.json`, `properties.json`).

## Decision

Use Poem + `poem-openapi`. Generate `/openapi.json` from handler types, commit a copy as `openapi.json`, diff and lint it (Spectral) in CI.

How the hard parts are modelled:

| Need | Approach |
|---|---|
| OpenAPI version | `poem-openapi` emits **OpenAPI 3.0.x**. **To confirm** the exact version in the spike. |
| `unevaluatedProperties`, `if/then/else` | Not expressible in 3.0. The JSON Schema files stay the contract, served verbatim at `/schema/v0.3/...`. The server validates every observation `value` against its property's `value_schema` at runtime with the `jsonschema` crate (draft 2020-12). |
| Polymorphic observation `value` | Free-form JSON (`serde_json::Value` / `poem_openapi::types::Any`); description points to the registry. |
| `subject` `oneOf` | `#[derive(Union)]` with `discriminator_name = "scope"`. **To confirm** the generated `oneOf` + `discriminator` is valid 3.0 and lints clean. |
| Errors | RFC 9457 `application/problem+json` via a custom `ApiResponse` / `#[oai(content_type = "application/problem+json")]` payload. |
| `ETag` / `304` | Response headers via `#[oai(header = "ETag")]` and `Cache-Control`; a `304 Not Modified` variant on cacheable responses; `If-None-Match` read as a header param. |
| Static bytes (schemas, gzip documents) | Plain Poem endpoints alongside the OpenAPI service. |

Budgets (section 8.1: binary/image size, idle RSS, cached throughput): **to confirm by measurement** in M1.

## Consequences

- Clients needing 3.1 can be served later by a 3.0→3.1 conversion step.
- Some schema detail is weaker in OpenAPI than in the JSON Schemas; the docs say so.

## Fallback

Axum + `utoipa` (emits 3.1, larger ecosystem). Switch only if the spike shows `poem-openapi` cannot model the above or misses the budgets; record it as a superseding ADR.
