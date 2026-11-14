# M1–M3 MVP: read API, signed writes, aggregation

Date: 2026-11-14. Scope per ADR-0004 and `docs/mvp-contract.md`.

## Built

- **Migrations** (`migrations/0001_init.sql`): `pg_trgm` and `unaccent` only, no PostGIS (ADR-0001). Schemas `public` and `private`. Group roles `eatsights_read`, `eatsights_write` and `eatsights_worker` with grants. Entity tables store the canonical document as `jsonb` plus indexed columns (`geohash text COLLATE "C"`, trigram `search_text`). `observations` is partitioned by month from 2026-01 to 2028-12, with a DEFAULT partition. `observation_ids` gives global idempotency. Also: `facts`, `menu_docs`, `changes`, `removals`, `vendor_bindings`, `published_keys` and a registry mirror. Append-only triggers cover observations, submissions, entity submissions and changes, alongside REVOKEs. `public.redact_observation(id, reason)` is the single `SECURITY DEFINER` removal path.
- **eatsights-db**: runtime `sqlx` queries (no offline data), embedded migrator, read/write/worker query modules. Geo search follows ADR-0001: `cell=` is one range scan; `near=` uses the 3×3 precision-5 cells via `unnest … CROSS JOIN LATERAL` plus a haversine filter.
- **eatsights-api** (Poem + poem-openapi, OpenAPI 3.0.0): every read in AGENTS.md 6.1.
  - Caching: weak ETag from the latest `changes.seq`, kept in memory through LISTEN/NOTIFY with a polling fallback. `If-None-Match` gets a `304` without a database call. `Cache-Control` max-age is the shortest relevant half-life × 0.01, clamped to 5 s – 1 h. The moka cache holds both identity and gzip bodies.
  - Writes: `POST /v1/keys`, `/v1/submissions` (the full section 7 pipeline), `/v1/entities` (v0.4 proposal) and `/v1/attestations` (vouch, vendor_of and revoke, issued by root or verifier keys).
  - Requests: RFC 9457 errors, 1 MiB body limit, request timeout, per-key/per-IP `governor` limits, and trusted-proxy `X-Forwarded-For` handling.
  - Also served: `/healthz`, `/readyz`, `/metrics`, `/openapi.json` and `/schema/v0.3/*`.
- **eatsights-worker `runner`**: follows `changes` from a persisted cursor, recomputes the touched (subject, property) facts (following `merged_into`), upserts facts only when they change, and rebuilds `menu_docs`. It is idempotent: replaying the feed produces no new changes (tested).
- **Binary**: `serve [--roles api,worker] [--migrate]`, `migrate`, `registry sync`, `keygen`, `import <ndjson>`, `export <dir>` and `openapi`. Hidden e2e helpers: `sign`, `id`, `pubkey`. Logs are JSON to stdout. SIGTERM shuts down gracefully.

## Deferred / known limits

- Facts are recomputed only when new observations arrive. A periodic sweep that ages facts to `stale` without new input is not built yet.
- `/v1/export/*` returns 404. The `export` CLI writes snapshots, but the worker does not schedule them.
- (Superseded by M4, see `docs/m4-trust.md`.) `rotate` attestations, vouch depth limits, the OSM checker, the duplicate detector and the merge flow are not built. Revoking a vouch resets the subject to `unverified`.
- ETags are global (latest seq), not per resource. They are correct but conservative.
- In `export`, each table is loaded in memory (fine for MVP sizes).
- Observations with `source_type: vendor` from a key without a current `vendor_of` are rejected (403 `not-vendor`), not down-classed.

## Run locally

```sh
nix develop
pg-start                                   # 127.0.0.1:5432, .dev/pg
cargo build --release -p eatsights
target/release/eatsights keygen            # keep private_key offline
export DATABASE_URL=postgres://eatsights@127.0.0.1:5432/eatsights
export EATSIGHTS_ROOT_PUBLIC_KEY=<public_key>
export EATSIGHTS_PSEUDONYM_SALT=$(head -c 32 /dev/urandom | base64)
target/release/eatsights migrate
target/release/eatsights serve             # 0.0.0.0:8080
```

Tests: `TEST_DATABASE_URL=postgres://… cargo test --workspace`. Each test creates and drops its own database.
End to end: `DATABASE_URL=… scripts/e2e.sh` starts a server on `$PORT` (default 18080) with a fresh root key. To test a running server instead, pass `BASE=… ROOT_PRIVATE_KEY=…`.
