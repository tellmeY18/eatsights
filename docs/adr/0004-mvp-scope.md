# ADR-0004: MVP scope

- Status: Accepted (maintainer request), scope open to review
- Date: 2026-11-14

## Context

On 2026-11-14 the maintainer asked to build the MVP through to a localhost deployment in one go, instead of stopping for acceptance after each milestone (AGENTS.md section 11). Working details are in `docs/mvp-contract.md`.

## In scope

- **M0 docs:** ADR-0001 (database), ADR-0002 (framework), proposal 0001 (v0.4 entity submissions), CI, dev shell, Dockerfile, `deploy/`.
- **M1 reads:** migrations, public read endpoints, caching headers, in-process cache, `/openapi.json`, `/schema/*`, health, metrics.
- **M2 writes:** key registration, signed submissions (section 7), rate limits, idempotency, entity writes (provisional v0.4).
- **M3 aggregation:** worker producing facts, ADR-0003.
- **Change feed:** `GET /v1/changes`.
- **Trust, minimal:** root key from env and `vendor_of` only as far as writes need them.

## Deferred

- Trust attestations beyond root / minimal `vendor_of` (vouch, revoke, rotate, depth limits).
- OSM checker.
- Duplicate detector and merge flow.
- Exports (`/v1/export/*`).
- Load tests in CI.
- Backups and restore drill (procedure documented in `deploy/README.md`, not automated).

## Consequences

Provisional choices (v0.4 proposal, aggregation numbers) ship before approval; they are versioned and may change.
