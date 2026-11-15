# M4: trust and vendors

Implements AGENTS.md M4 with the ADR-0006 defaults; the rules and choices are in `docs/adr/0007-trust-policy.md`.

## What changed

- **Core** `eatsights_core::trust`: `Level`, `can_grant`, `TrustConfig` and the pure, order-independent `recompute` of levels and depths (unit tested).
- **Migration** `migrations/0002_trust.sql`: `private.keys.level_cap`, `demoted_at`, `vouch_cooldown_until`; append-only `private.strikes`; `public.vendor_bindings.holder_key_id`; attestation indexes.
- **Writes** `POST /v1/attestations`:
  - `vouch`: issuer strictly above the granted level (member <- trusted+, trusted <- verifier+, verifier <- root), not in cooldown, depth <= `EATSIGHTS_TRUST_MAX_DEPTH`. Never lowers a level.
  - `vendor_of`: verifier or root; expiry defaults to and is capped at 365 days.
  - `revoke`: of `vouch` or `vendor_of`, by the issuer, a verifier or root. Revoking a vouch cascades through the graph; a revoke by root/verifier who is not the issuer is a strike; 2 strikes in 180 days demote one level (floor member, root exempt) with a 30-day vouch cooldown.
  - `rotate`: old key -> fresh registered key; the new key inherits level, depth, strike state and vendor bindings; the old key is `rotated` and its signatures are rejected.
- **Reads**: `GET /v1/attestations?type=vendor_of` returns current bindings only, `include_inactive=true` adds the rest (`active`, `revoked`, `holder_key_id` after rotation). `/v1/keys/published` is exactly the active root and verifier keys.
- **Config** (env): `EATSIGHTS_TRUST_MAX_DEPTH` (4), `EATSIGHTS_VENDOR_OF_MAX_DAYS` (365), `EATSIGHTS_TRUST_STRIKES` (2), `EATSIGHTS_TRUST_STRIKE_WINDOW_DAYS` (180), `EATSIGHTS_TRUST_VOUCH_COOLDOWN_DAYS` (30). Added to `deploy/app/configmap.yaml`.
- `openapi.json` regenerated (new `include_inactive` parameter, descriptions).

## Tests

- Unit: grant rules, chain depth, cascade on loss, strike cap, rotation, order independence.
- DB integration (`TEST_DATABASE_URL`): `trust_vouch_rules_and_depth`, `trust_revoke_cascade_and_strikes`, `trust_vendor_and_rotate` (includes a privacy check: no vouch/revoke/rotate data, member key ids or signatures in public reads).
- `scripts/e2e.sh`: root grants verifier V; V binds a vendor key; vendor observation gets class `vendor`; rotation keeps the binding and rejects the old key; revoking the binding rejects vendor submissions; depth limit (server started with max depth 2); two strikes demote V, unpublish it and start its cooldown.

## Deviations

- No `reason` field exists on attestations, so "abuse" is inferred: a vouch revoked by root or a verifier other than its issuer (ADR-0007 decision 4).
- With strictly decreasing levels the natural chain depth is at most 3, so the default limit of 4 never binds; e2e runs with 2.
