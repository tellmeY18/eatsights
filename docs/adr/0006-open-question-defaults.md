# ADR-0006: Defaults for the open questions

Status: Accepted as defaults by the maintainer ("go with sane defaults", 2026-11-14). Every number is configuration, not code, and can be changed without a schema change.

| # | Question (SCHEMA.md / AGENTS.md 12) | Default |
|---|---|---|
| 1a | Weight per level | As ADR-0003: unverified 0.5, member 1.0, trusted 1.5, verifier 2.0, vendor 1.5, import 0.5 (see `aggregation.toml`). |
| 1b | Maximum chain depth | 4 (root 0). A vouch that would put the subject deeper than 4 is rejected. Config `EATSIGHTS_TRUST_MAX_DEPTH`. |
| 1c | Default attestation expiry | `vendor_of`: max 365 days; issued without `expires_at` it is treated as expiring 365 days after `issued_at`. `vouch`: no expiry. Config `EATSIGHTS_VENDOR_OF_MAX_DAYS`. |
| 1d | Penalty for vouching for a bad key | When a vouch is revoked by root or a verifier with reason `abuse`, the issuer gets a strike. 2 strikes within 180 days lower the issuer one level (never below `member`, root exempt) and the issuer cannot vouch for 30 days. Config `EATSIGHTS_TRUST_STRIKES`, `..._STRIKE_WINDOW_DAYS`, `..._VOUCH_COOLDOWN_DAYS`. |
| 2 | Who may grant what | `member`: trusted, verifier or root. `trusted`: verifier or root. `verifier`: root only. Issuer level must be strictly above the granted level. |
| 3 | Pseudonym salt rotation | No scheduled rotation; rotate only on suspected compromise. Each observation stores its salt version; aggregation counts distinct `(salt_version, pseudonym)`, so a rotation can at worst count a person twice for observations straddling it. That is accepted and documented, not hidden. |
| 4 | Time zone of `sold_out_time.local_time` and schedules | Local time at the place. Zone resolved from config `EATSIGHTS_DEFAULT_TIMEZONE` (default `Asia/Kolkata`); a per-place zone is a later additive field. |
| 5 | Derive `meal_times` from exact schedules | No. Both are kept as observed; availability uses the exact schedule when present (SCHEMA.md layering). |
| 6 | Duplicate suggestion thresholds | Same category AND (distance <= 50 m AND name trigram similarity >= 0.4, OR distance <= 150 m AND similarity >= 0.7). Names compared after `unaccent` + lower-case across all `names` values. Suggestions only; a person confirms. |
| 7 | `ready_to_map` | `osm.state = unmapped`, has name, category, pin, and `status.operating` fact is `confirmed` with >= 2 distinct contributors (the aggregation confirm rule) observed within the last 90 days. |
| 8 | Max life of a `proposed` property | 180 days, then promoted or deprecated in a registry PR. Process rule, not code. |
| — | `key_id` and signed bytes | As in proposal 0001 (accepted as default). |
| — | OSM checker | Opt-in: off unless `EATSIGHTS_OSM_ENDPOINT` is set (Overpass API URL). Max 1 request per 10 s, User-Agent `eatsights/<version> (+https://eatsights.letsimpl.fyi)`, results cached, re-check `unmapped` places every 30 days. Tests use a local mock, never the public endpoint. |
| — | Exports | Daily, plus on demand via `eatsights export`. Kept: latest 7 snapshots. |
| — | Postgres hosting | No operator chosen. Default: an external/managed PostgreSQL 18 with `pg_trgm` and `unaccent`; `deploy/README.md` documents requirements. Revisit with the maintainer. |
| — | Backup root key holder (GOVERNANCE.md) | Not defaulted: needs a named person. Left `[TBD]`. |
