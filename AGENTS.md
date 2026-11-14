# AGENTS.md — Eatsights

Instructions for AI agents and contributors building the Eatsights API.
Read this file first, then the files in "Source of truth" below.

- **Project:** Eatsights, an open food dataset served as an API.
- **Org:** letsimpl (letsimpl.fyi): complex problems, simple solutions.
- **Host:** `https://eatsights.letsimpl.fyi` serves everything: API, OpenAPI document, JSON Schemas. No human is expected to open it in a browser.
- **Licence of the data:** CC0 1.0. **Maintainer / BDFL:** Vysakh Premkumar.
- **Status:** schema draft v0.3, no code yet. Date of this spec: 2026-11-14.

## 1. Mission

Eatsights is public infrastructure for everyday food data: what is on the menu, what it costs, whether it is available, what it includes, how good it is, and how long it takes. Chaikadi (chaikadi.fyi) and many other apps will read and write it through one public API. Chaikadi gets no special treatment and nothing is built for it here; Chaikadi adopts Eatsights later.

**API-first. Build no UI, no web pages, no admin dashboard.**

Two design goals beyond correctness:

1. **Simple.** One binary, one Postgres, no extra infrastructure unless a measured problem demands it.
2. **Efficient.** It must run comfortably in a constrained container (small CPU and memory requests on a Kubernetes cluster) and still serve a very large read load, by being cache-friendly (see section 8).

## 2. Source of truth

All of these are in the repo root. Do not move them.

| File | Role |
|---|---|
| `schema.json` | Entities, observations, keys, attestations, facts (JSON Schema 2020-12). The contract. |
| `property.schema.json` | Shape of one registry entry. |
| `properties.json` | The property registry (30 properties). Git is the source of truth. |
| `SCHEMA.md` | Explains the schema, trust chain and examples. Lists open questions. |
| `GOVERNANCE.md` | Commitments that bind the code (section 3). |
| `CONTRIBUTING.md` | Rules for contributors and for proposing properties. |

Rules:

- The schema files are the contract. **Code follows them, not the reverse.**
- Do not change `schema.json`, `property.schema.json` or `properties.json` without a written proposal in `docs/proposals/` that the maintainer approves. The two exceptions authorised by this document are listed in M0.
- Every JSON example in `SCHEMA.md` must validate in CI.
- If the docs disagree with each other or with the schema, stop and ask. Do not pick silently.

## 3. Hard constraints (from GOVERNANCE.md and CONTRIBUTING.md)

Never violate these. A failing test should catch each one.

1. **Observations are append-only.** No update or delete in normal operation. Errors are fixed by newer observations. Legal or safety removals go through one audited function that replaces content with a tombstone and writes a public log entry without the removed content.
2. **Public responses never contain** raw `key_id`s of ordinary contributors, signatures, or the vouch / revoke / rotate graph. Public: observations (class + pseudonym), `vendor_of` attestations, root and verifier keys. Golden tests enforce this.
3. **Data is CC0-1.0.** Every observation carries `license: "CC0-1.0"`; reject anything else.
4. **Nothing is copied from OSM, Google, Zomato, Swiggy or similar.** External data is linked by ID only. The OSM checker stores IDs, distances and match scores, never OSM tags, names or coordinates.
5. **No free text** in observations, no images stored, no personal data.
6. **No paid ranking or trust.** No API key, tier or customer can buy placement or weight.
7. **One public API for every app,** including Chaikadi. No hidden endpoints for any single app.
8. **The server never holds the root private key.** It holds the root public key only.

## 4. Decisions already made

| # | Decision |
|---|---|
| D1 | PostgreSQL is the database. One instance, no read replica for now. **No SQLite read copy.** Revisit a read replica only when measurements show a need. |
| D2 | Rust, with Poem and `poem-openapi`. An OpenAPI document is a hard requirement. |
| D3 | `properties.json` in git is the source of truth for the registry. It is embedded in the binary at build time and mirrored into the database on startup. |
| D4 | Canonical JSON for signatures is RFC 8785 (JCS). |
| D5 | Open key registration, rate-limited. New keys start `unverified`. |
| D6 | Aggregation algorithm and numbers are decided by the agents (section 9), versioned and configurable. The maintainer reviews them. |
| D7 | Entities (places, sections, menu items, dishes) need a signed write path. Agents propose it as schema v0.4 (M0 proposal). |
| D8 | Nothing is built for Chaikadi. No compatibility exports. |
| D9 | The filename is `AGENTS.md`. |
| D10 | The public host is `eatsights.letsimpl.fyi`. Schemas live at `/schema/v0.3/...`, the API at `/v1/...`. |

## 5. Architecture

### 5.1 One binary, one database

```
            CDN / ingress (optional, recommended)
                      |
        +-------------v--------------+
        |  eatsights  (one binary)   |
        |  roles: api, worker        |   default: both in one process
        |  in-process cache          |
        +-------------+--------------+
                      |
                 PostgreSQL (one instance)
                 schemas: public, private
```

- One executable, `eatsights`, with subcommands: `serve` (flag `--roles api,worker`), `migrate`, `registry sync`, `keygen`, `import`, `export`. Splitting API and worker into separate deployments is just a different `--roles` value. If they run apart, they coordinate through Postgres `LISTEN/NOTIFY`.
- **Do not add:** Redis, message queues, search engines, an ORM, GraphQL, gRPC or any extra service. Postgres does storage, search (`pg_trgm`), geo and the queue-like work.
- Every added dependency must be justified in the PR (binary size, compile time, audit surface). Run `cargo deny` and `cargo audit` in CI.

### 5.2 Workspace layout

```
Cargo.toml                 workspace
crates/eatsights-core/     ids, canonical JSON, signature, schema + registry validation, no I/O
crates/eatsights-db/       sqlx queries, migrations
crates/eatsights-api/      Poem + poem-openapi handlers, middleware
crates/eatsights-worker/   aggregation, OSM checker, duplicate detector, exports
crates/eatsights/          the binary (CLI, config, wiring)
migrations/                SQL, forward-only
docs/adr/                  architecture decision records
docs/proposals/            schema / registry change proposals
```

Rust edition 2024, stable toolchain (Poem requires 1.85 or newer). Pin the toolchain.

### 5.3 Database layout

Two Postgres schemas and three roles so that a bug cannot easily leak private data.

| Schema | Holds |
|---|---|
| `public` | places, sections, dishes, menu_items, observations (public form), facts, `vendor_of` attestations, published keys (root, verifiers), registry mirror, `changes` (feed), `removals` (public log) |
| `private` | submissions (signed originals with raw `key_id`), keys, vouch / revoke / rotate attestations, merge suggestions, the pseudonym salt version |

Roles: `read` (SELECT on `public` only; the API read path), `write` (insert on both schemas through the write path), `worker`.

Rules for the data model (agents write the exact DDL; propose in an ADR first):

- IDs are the prefixed 26-character Crockford ULIDs from the schema. Store as `text` with CHECK constraints, or as 16-byte values with a prefix column if the benchmark shows a real size or speed win.
- `observations` is partitioned by month on `submitted_at`. Index for "latest per subject and property" and for the change feed.
- Observation `value` is `jsonb`.
- Append-only is enforced in the database: `REVOKE UPDATE, DELETE` for the application roles, plus a trigger that raises on any attempt. The single redaction function is `SECURITY DEFINER`.
- `public.changes` is a global, monotonically increasing sequence of `(seq, entity_type, entity_id, op)`, written in the same transaction as the change. It drives the change feed and cache ETags.
- Required extensions are minimal. Default plan: `postgis` (nearby search), `pg_trgm` and `unaccent` (name matching). ADR-0001 must also test whether geohash columns plus a btree (and `earthdistance`, no PostGIS) are good enough, because fewer extensions means a lighter image. Choose the simpler one that meets the budget.
- Connection pool is small (default 8 per process, configurable). No N+1 queries.

## 6. API v1

Base URL `https://eatsights.letsimpl.fyi`. JSON by default. Errors are RFC 9457 `application/problem+json`. List endpoints use cursor pagination (`limit` default 50, max 500). Times are UTC.

### 6.1 Public reads (no auth, cacheable)

| Method and path | Purpose |
|---|---|
| `GET /v1/places` | Search: `cell=` (geohash), `near=lat,lng&radius=`, `city=`, `category=`, `q=` (name) |
| `GET /v1/places/{id}` | One place. For a merged place, `301` to the survivor (or `200` with `merged_into` for clients that don't follow) |
| `GET /v1/places/{id}/menu` | Sections and menu items with current facts, pre-built |
| `GET /v1/menu-items/{id}` | One item with all facts |
| `GET /v1/dishes`, `/v1/dishes/{id}` | Dishes |
| `GET /v1/facts` | Facts, filtered by subject and property |
| `GET /v1/observations` | Public observations, filtered |
| `GET /v1/registry/properties`, `/{id}` | The registry |
| `GET /v1/attestations?type=vendor_of&place_id=` | Public vendor bindings |
| `GET /v1/keys/published` | Root and verifier keys |
| `GET /v1/changes?after=<seq>` | Change feed. Apps sync with this and keep a local copy |
| `GET /v1/export/{name}` | Bulk NDJSON snapshots (gzip), generated by the worker |
| `GET /schema/v0.3/schema.json`, `/property.schema.json`, `/properties.json` | The contract files, served verbatim |
| `GET /openapi.json` | OpenAPI document (generated) |
| `GET /healthz`, `/readyz` | Liveness and readiness |

Optional, after M5: `Accept: text/csv` on list endpoints.

### 6.2 Signed writes

Authentication is the Ed25519 signature, not a password, token or account.

| Method and path | Purpose |
|---|---|
| `POST /v1/submissions` | Batch of up to 100 signed submissions. Per-item result; the batch is not all-or-nothing |
| `POST /v1/keys` | Register a public key (proof of possession required). Starts `unverified` |
| `POST /v1/attestations` | Signed vouch, `vendor_of`, revoke or rotate |
| `POST /v1/entities` | Signed place / section / menu item / dish create and edit (shape comes from the v0.4 proposal, D7) |

### 6.3 Admin (signed by root or a verifier key; same scheme)

Confirm or undo place merges, revoke attestations, record a legal removal. Property promotion is a git change to `properties.json` plus a deploy, not an API call.

### 6.4 OpenAPI

- Generate it with `poem-openapi`; serve `/openapi.json` and keep a copy committed as `openapi.json`. CI diffs it and fails on accidental breaking changes.
- Reality check to confirm in the spike: `poem-openapi` emits OpenAPI **3.0.x**, which cannot express `unevaluatedProperties` or `if/then/else`. The JSON Schema files remain the real contract and are served at `/schema/...`. The server validates every observation `value` against its property's `value_schema` at runtime (use the `jsonschema` crate, draft 2020-12). If the maintainer later needs 3.1, add a 3.0-to-3.1 conversion step, or switch frameworks (ADR-0002).
- Lint the document in CI (for example with Spectral).

## 7. Write path

For every submission, in this order:

1. Parse and validate the envelope against the schema (private `submission` form).
2. Canonicalise (RFC 8785) everything except `signature`, and verify the Ed25519 signature against the key.
3. Look up the key: it must exist and be `active`. Apply rate limits (per key and per IP; in-process, e.g. `governor`; trust `X-Forwarded-For` only from configured proxies).
4. Look up `property` in the registry. Check scope matches `subject.scope`, the writer type is allowed, and `value` satisfies `value_schema`.
5. Check time: `observed_at` not in the future (small allowance) and not older than a configurable maximum (default 30 days, since offline apps submit late). `submitted_at` close to server time.
6. Idempotency: the observation `id` is unique. Resubmitting returns the original result.
7. Assign `class` from the key level. `vendor` only if the key holds a current `vendor_of` for that place and `source_type` is `vendor`.
8. Compute the pseudonym `HMAC(salt, key_id + anchor)`, truncated to 22 base64url characters, where the anchor is the `place_id` (place and menu-item scope) or `dish_id` (dish scope).
9. In one transaction: insert the private submission, insert the public observation, append to `changes`.

Pin down in the v0.4 proposal: the exact definition of `key_id` (default: base64url of the first 16 bytes of SHA-256 of the raw public key, which gives 22 characters), and the exact bytes that are signed.

Secrets (the HMAC salt, database credentials) come from Kubernetes Secrets via environment variables. Store a salt version with each pseudonym so rotation is possible later.

## 8. Performance, caching and constrained environments

The target workload is a very large read volume on a very small origin. This works because the data is public and identical for everyone.

### 8.1 Initial budgets (verify with the spike, then keep as CI benchmarks)

| Budget | Target |
|---|---|
| API+worker process, idle RSS | at most 64 MB |
| Process RSS under load | at most 256 MB (the in-process cache size is configurable) |
| Container image | at most 30 MB (static musl binary, distroless or scratch base) |
| Cold start to ready | under 1 second |
| Cached read throughput | 20,000 requests/s or more on one core |
| Cached read p99 at the origin | under 5 ms |
| Uncached indexed read p99 | under 20 ms |

Postgres is sized separately. Start with settings for a small instance (for example `shared_buffers` around 128 MB) and document them.

### 8.2 Rules

1. **Every public GET is cacheable.** Send `Cache-Control` (with `stale-while-revalidate` and `stale-if-error`), an `ETag` derived from the latest relevant `changes.seq`, and answer `If-None-Match` with `304` without touching the database where possible.
2. **TTL follows the property's half-life.** Fast-changing facts (`availability.observed`, half-life 2 h; `queue.length`, 0.5 h) get seconds to minutes. Slow facts get hours. Derive it; do not hard-code per endpoint.
3. **Quantised geo queries.** Nearby search is by geohash cell (default precision 6, about 1.2 km by 0.6 km) so many users share one cacheable URL. `near=lat,lng&radius=` is accepted but quantised server-side and rate-limited.
4. **Pre-built documents.** The worker regenerates a place's menu document when its facts change. The API serves the stored document from the in-process cache (`moka` or similar) and keeps a pre-compressed (gzip) copy so it does not compress per request.
5. **Apps replicate instead of polling.** `GET /v1/changes` and `/v1/export/*` let apps keep local copies. This is the main way the system scales; keep these endpoints cheap and well documented.
6. **Aggregation is off the request path.** It runs in the worker, incrementally per (subject, property).
7. **No unbounded work per request:** capped page sizes, capped batch sizes, request timeouts, body size limits.
8. **The single database is a single point of failure.** Mitigate with automated backups (base backup plus WAL or `pg_dump`) to object storage, a tested restore, and CDN `stale-if-error`. Document the procedure. Backups include the private schema; public exports must never include it.

### 8.3 Kubernetes

- Provide a minimal `Dockerfile` (multi-stage, static binary) and plain manifests under `deploy/` (Deployment, Service, Ingress, ConfigMap, Secret template, a migration Job). No Helm unless the maintainer asks.
- Starting resources for the app (tune after measuring): requests about 50m CPU and 64 Mi, limits about 500m CPU and 256 Mi.
- Graceful shutdown on SIGTERM (finish in-flight requests), readiness that checks the database, liveness that does not.
- Config entirely through environment variables. Set Tokio worker threads from the CPU limit (configurable).
- Run migrations as a Job (or an init step), never implicitly on every replica start in production.
- Postgres on the cluster: document what is required (extensions, parameters, backups). Do not choose an operator without asking.
- Structured logs (JSON) to stdout, `/metrics` in Prometheus format. No agents or sidecars required.

## 9. Workers

All run inside the binary as Tokio tasks. All are idempotent and safe to run twice.

### 9.1 Aggregation

- Turns observations into `fact` records using each property's `aggregation.strategy` (`latest_weighted`, `median`, `mode`, `union`, `schedule`, `derived`) and `half_life_hours`.
- **The agents decide the numbers** (D6): weights per contributor class, recency decay, confidence formula, thresholds for `confirmed`, `contested` and `stale`. Put them in a config file with an embedded default, stamp each fact with `algorithm_version`, and describe the choices in `docs/adr/0003-aggregation.md` for the maintainer to review.
- Non-negotiable properties:
  - **No single key can confirm a fact alone.** `confirmed` needs independent contributors (distinct pseudonyms at the same place or dish).
  - Trust raises weight, never grants the power to confirm alone.
  - Vendor values are weighted but never silently override customers: set `vendor_value_differs` when they disagree.
  - It is a pure, deterministic function of its inputs, covered by property-based tests.
  - Follows `merged_into` redirects.
  - Follows the availability layering in `SCHEMA.md` (place hours, listing, declared schedule, fresh observation).

### 9.2 OSM checker

Searches OSM near a place's pin (Overpass or Nominatim, or a self-hosted endpoint set by config) and records only `state`, `checked_at`, candidate IDs, distance and a name score. Respect the usage policies of any public endpoint: a descriptive User-Agent, a strict rate limit, caching. Periodically re-checks `unmapped` places. Sets `ready_to_map` when the schema's conditions hold.

### 9.3 Duplicate detector

Finds likely duplicate places (same category, close pins by PostGIS or geohash, similar names by `pg_trgm`) and stores suggestions in `private`. A person confirms; the system never merges automatically. The merge sets `record_state: merged` and `merged_into`, and nothing is deleted.

### 9.4 Exports

Writes gzip NDJSON snapshots of the public data (never `private`) on a schedule, plus a manifest with checksums. Serve them from `/v1/export/*`.

## 10. Testing

- **Schema conformance:** every example in `SCHEMA.md` and every fixture validates; invalid fixtures fail for the right reason.
- **Registry:** `properties.json` validates against `property.schema.json`; every `value_schema` is valid; sample values pass and fail as expected.
- **Signatures:** fixed test vectors (canonicalisation and Ed25519), including tampered fields.
- **Privacy golden tests:** snapshot every public response and assert that no raw key, signature or private attestation appears.
- **Append-only:** tests prove `UPDATE` and `DELETE` fail for the application roles.
- **Aggregation:** property-based tests (deterministic, order-independent, a single key never confirms).
- **API contract:** the generated `openapi.json` is linted and diffed; examples round-trip.
- **Load tests:** a repeatable benchmark for the budgets in 8.1 on a constrained container (CPU and memory limits applied).
- **Migrations:** apply from scratch and from the previous version.

## 11. Milestones

Each milestone ends with passing CI and a short note in `docs/`. Do not start a milestone before the previous one is accepted.

**M0: foundations**
- Validate the three schema files with a real JSON Schema 2020-12 validator and fix problems by proposal (the draft says this was never done).
- Authorised edits, because the host is now known: set the `$id` of `schema.json` and `property.schema.json` (and registry references) to `https://eatsights.letsimpl.fyi/schema/v0.3/...`, and replace "project name TBD" with "Eatsights" in the docs. Make the base address one config constant in code.
- Initialise git. Create the workspace, toolchain pin, CI (fmt, clippy with warnings denied, tests, `cargo deny`, `cargo audit`), and a reproducible dev environment (a Nix flake dev shell is preferred because the maintainer uses Nix; plus a Docker Compose Postgres for others).
- Write ADR-0001 (database), ADR-0002 (framework) and the v0.4 entity-submission proposal (D7). In ADR-0001 record D1, compare PostgreSQL with SQLite, MySQL, MongoDB, TimescaleDB and ClickHouse against the needs in section 5.3, and state what measurement would trigger a read replica later.
- Spike for ADR-0001: load the registry and about 1M synthetic observations, then measure nearby search, "latest fact" reads, inserts, and memory use on a constrained instance. Also test PostGIS against geohash plus btree.
- Spike for ADR-0002: confirm `poem-openapi` can model the polymorphic `value`, the `subject` `oneOf`, problem+json errors and `ETag`/`304`; confirm the OpenAPI version it emits; check binary size and memory against 8.1. Name Axum with `utoipa` as the fallback.

**M1: read-only API.** Migrations, seed data loader, all public read endpoints, caching headers and in-process cache, `/openapi.json`, `/schema/*`, health and metrics. Dockerfile and manifests.

**M2: signed writes.** Key registration, the write path in section 7, rate limits, idempotency, entity writes per the approved v0.4 proposal.

**M3: aggregation and facts.** The worker, facts served on read endpoints, ADR-0003.

**M4: trust and vendors.** Attestations (vouch, `vendor_of`, revoke, rotate), levels, depth limits as configuration, vendor-class observations.

**M5: sync, workers and exports.** Change feed, exports, OSM checker, duplicate detector and merge flow.

**M6: hardening.** Backups and restore drill, load tests in CI, security review (replay, canonicalisation edge cases, header trust, DoS limits), published OpenAPI document and a short developer guide (in `docs/`, Markdown only).

## 12. Open items

Agents propose answers in an ADR or proposal; the maintainer decides.

- The aggregation numbers and thresholds (D6), including the trust weights, maximum chain depth and the penalty for vouching for a bad key (SCHEMA.md open questions 1 and 2).
- Pseudonym salt rotation and its effect on distinct-contributor counts (open question 3).
- Time zone handling for `sold_out_time.local_time` (open question 4).
- Whether `meal_times` is derived from exact schedules (open question 5).
- Merge and `ready_to_map` thresholds (open questions 6 and 7).
- How long a `proposed` property may stay unpromoted (open question 8).
- The exact `key_id` definition and signed bytes (section 7).
- Whether Postgres runs in-cluster with an operator or is external.

## 13. Out of scope

Any UI or web page; image storage; user accounts, passwords or payments; reviews or free text; OCR or menu-photo processing on the server (clients extract, then submit structured data); ordering, delivery or listings; anything specific to Chaikadi.

## 14. Working agreements for agents

- Prefer the simplest thing that meets the budgets. Measure before optimising.
- Small, reviewable changes. One concern per PR.
- Write an ADR for any choice with real alternatives. Short is fine.
- If a requirement is ambiguous or the files disagree, ask. Do not guess.
- Never log or return private data. Never weaken a constraint in section 3 to make a test pass.
- No `unwrap` or `expect` in non-test code. Use typed errors.
- Keep the public contract stable: version the API (`/v1`), and add fields instead of changing meanings.
