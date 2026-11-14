# MVP build contract (working notes for agents)

Status: working agreement for the MVP build, decided by the coordinating agent on
2026-11-14 at the maintainer's request ("get the MVP ready and deploy to localhost").
Every item here that is a real choice is also written up in an ADR or the v0.4
proposal for maintainer review. Read `AGENTS.md` first; this file only pins the
details that several crates must agree on.

## Environment

- Run everything through the flake: `nix develop -c <cmd>` (Rust 1.90, edition 2024, Postgres 18 + PostGIS, sqlx-cli, cargo-deny, cargo-audit, check-jsonschema).
- Local Postgres: `PGPORT=<port> EATSIGHTS_PGDIR=<dir> pg-start` (trust auth, superuser `eatsights`, db `eatsights`). The app instance uses port 5432 and `.dev/pg`. Spikes and tests use their own port/dir.
- Lints: `unwrap_used` / `expect_used` denied outside tests (`#[cfg(test)]` modules may `#![allow(...)]`). `cargo clippy --all-targets -- -D warnings` must pass.
- Base address constant: `eatsights_core::BASE_URL = "https://eatsights.letsimpl.fyi"`, overridable at runtime by `EATSIGHTS_BASE_URL`.

## Crypto and identity (pinned; documented in docs/proposals/0001-v0.4-entity-submissions.md)

- `key_id` = base64url (no padding) of the first 16 bytes of SHA-256 of the 32 raw Ed25519 public key bytes. Always 22 chars.
- `public_key` = base64url (no padding) of 32 raw bytes (43 chars). `signature` = base64url no padding of 64 bytes (86 chars).
- **Signed bytes** = UTF-8 of RFC 8785 (JCS) canonical JSON of the whole JSON object with the top-level `signature` member removed. Nothing else (no prefix). Applies to submissions, attestations, entity submissions and key-registration proofs.
- Pseudonym = base64url-nopad(HMAC-SHA256(salt, key_id_utf8 || anchor_utf8)) truncated to 22 chars. Anchor: `place_id` (place / menu_item scope) or `dish_id` (dish scope). Salt from env `EATSIGHTS_PSEUDONYM_SALT` (required, >= 32 bytes), version from `EATSIGHTS_PSEUDONYM_SALT_VERSION` (default 1), stored next to each public observation (`pseudonym_salt_version` column, not in JSON body).
- Key registration body (`POST /v1/keys`):
  ```json
  { "type": "key_registration", "algorithm": "ed25519", "public_key": "<43>", "created_at": "<rfc3339>", "signature": "<86>" }
  ```
  Signature by the key itself over JCS of the body minus `signature` (proof of possession). Server derives `key_id`, stores `level: unverified`, `status: active`, `depth`: null-like (stored as large sentinel; unverified keys have no chain) — store `depth` as NULL in DB, emit nothing publicly. Idempotent on `key_id`.
- Root key: server holds only the public key, env `EATSIGHTS_ROOT_PUBLIC_KEY` (43-char base64url). On startup it is upserted as `level: root, depth: 0`. `eatsights keygen` prints a fresh keypair (private key to stdout only, never stored).

## Entity submissions (v0.4 proposal, MVP implementation)

`POST /v1/entities` body: an array (max 100) of:
```json
{
  "id": "es_<26 crockford>",
  "schema_version": "0.4",
  "op": "create" | "edit",
  "entity_type": "place" | "section" | "menu_item" | "dish",
  "entity": { ...entity object per schema.json $defs, client-chosen id... },
  "submitted_at": "<rfc3339>",
  "contributor": { "key_id": "<22>" },
  "license": "CC0-1.0",
  "signature": "<86>"
}
```
- Server-controlled fields the client must NOT send (rejected if present): place `osm` (create forces `{state: unchecked}`; edit keeps current), `record_state`, `merged_into`, `created_at` (server sets on create). menu_item `record_state`, `merged_into`, `created_at` likewise.
- After filling server fields, the stored entity must validate against `schema.json#/$defs/<entity_type>`.
- `create`: any `active` key. Fails with 409 if the id exists with different content; idempotent on `es_` id.
- `edit`: full replacement of client fields. Allowed for level `member` or above, or a key with a current `vendor_of` for the entity's place. `place_id` of sections/menu items is immutable.
- Each accepted entity submission: insert into `private.entity_submissions` (full signed original), upsert public entity row, append `public.changes` — one transaction.

## Errors

RFC 9457 `application/problem+json`: `{ "type": "https://eatsights.letsimpl.fyi/problems/<slug>", "title", "status", "detail" }`. Batch endpoints return 200 with per-item results:
`{ "results": [ { "index": 0, "id": "...", "status": "accepted" | "duplicate" | "rejected", "problem": {..}? } ] }`.

## Aggregation interface (crate `eatsights-worker`, module `aggregate`, pure)

```rust
pub struct ObsInput {            // one public observation, already redirect-resolved
    pub id: String,              // ob_...
    pub kind: Kind,              // Value | Confirm | Dispute
    pub value: Option<serde_json::Value>,
    pub refers_to: Option<String>,
    pub observed_at: chrono::DateTime<Utc>,
    pub class: ContributorClass, // Unverified|Member|Trusted|Verifier|Vendor|Import
    pub pseudonym: String,
    pub source_type: SourceType, // Customer|Vendor|Import
}
pub struct PropertyDef { pub id: String, pub half_life_hours: f64, pub strategy: Strategy, pub field: Option<String>, pub window: Option<u32> }
pub fn aggregate(prop: &PropertyDef, subject: &serde_json::Value, obs: &[ObsInput], now: DateTime<Utc>, cfg: &AggConfig) -> Option<serde_json::Value> // a `fact` object valid against schema.json#/$defs/fact
```
`AggConfig` loads from TOML (embedded default `crates/eatsights-worker/aggregation.toml`, override path env `EATSIGHTS_AGGREGATION_CONFIG`), has `algorithm_version`. Output must be independent of input order.

## Database (crate `eatsights-db`, migrations in `/migrations`)

- Geo for the MVP: `geohash` text column (precision 9 stored, prefix-searched with btree `text_pattern_ops`) + haversine in SQL. No PostGIS in the MVP schema; ADR-0001 records the spike result. Extensions: `pg_trgm`, `unaccent`.
- Roles created by migration (NOLOGIN group roles): `eatsights_read`, `eatsights_write`, `eatsights_worker`. Append-only trigger on `public.observations` and `private.submissions`.
- Env: `DATABASE_URL` (write/worker pool), `DATABASE_READ_URL` (optional, read pool; default = `DATABASE_URL`). Pool size `EATSIGHTS_DB_POOL` default 8.

## Binary

`eatsights serve [--roles api,worker]`, `migrate`, `registry sync`, `keygen`, `import <ndjson>`, `export <dir>`. Env `EATSIGHTS_LISTEN` default `0.0.0.0:8080`. JSON logs to stdout via `tracing-subscriber` json.
