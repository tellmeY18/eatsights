# ADR-0005: Optional Swagger UI at /docs

Status: Accepted by the maintainer (2026-11-14, request in session).

## Context

AGENTS.md section 1 and section 13 say "build no UI, no web pages". The maintainer asked for Swagger UI to browse and try the API.

## Decision

- Serve poem-openapi's built-in Swagger UI at `/docs`, generated from the same OpenAPI document as `/openapi.json`. It is a viewer for the API contract, not a product UI.
- **Off by default.** Enabled with `EATSIGHTS_SWAGGER_UI=true`. Production (`deploy/app/configmap.yaml`) leaves it `false` unless the maintainer decides otherwise.
- "Try it out" targets the `servers` URL, which is `EATSIGHTS_BASE_URL` (set it to `http://127.0.0.1:8080` locally).

## Consequences

- Release binary grows by about 1.6 MB (8.6 MB to 10.2 MB) because the UI assets are embedded. Still well inside the 30 MB image budget.
- No new endpoint carries data; `/docs` exposes nothing beyond `/openapi.json`.
- Alternative not taken: an external viewer (editor.swagger.io) pointed at `/openapi.json`, which needs no code.
