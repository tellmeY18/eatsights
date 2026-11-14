# M5 (part): OSM checker, duplicate detector, merges and admin actions

Status: implemented on branch `m5-osm-dedupe`. Exports and M4 trust are separate work.

## What was built

| Piece | Where |
|---|---|
| Migration `0004_osm_dedupe.sql` (suggestion status/distance/unique pair, `private.admin_actions`, indexes, grants) | `migrations/`, registered in `eatsights-db` |
| OSM checker + `ready_to_map` | `crates/eatsights-worker/src/osm.rs`, queries in `crates/eatsights-db/src/osm.rs` |
| Duplicate detector | `crates/eatsights-worker/src/dedupe.rs` |
| Fact recomputation after merge / undo | `crates/eatsights-worker/src/merge.rs` (hooked into `runner::step`) |
| Signed admin endpoints | `crates/eatsights-api/src/admin.rs`, transactions in `crates/eatsights-db/src/admin.rs` |
| CLI `eatsights dedupe list [--status open\|confirmed\|dismissed\|all]` | binary |
| Tests | `crates/eatsights-api/tests/m5.rs`, unit tests in the worker and API, `scripts/e2e.sh` |

## OSM checker (AGENTS.md 9.2, ADR-0006)

- **Opt-in.** It runs only when `EATSIGHTS_OSM_ENDPOINT` (an Overpass API interpreter URL) is set. `EATSIGHTS_OSM_RECHECK_DAYS` (default 30) sets when `unmapped` places are checked again.
- Picks active places with `osm.state = unchecked`, or `unmapped` with `checked_at` older than the recheck window, one at a time.
- Query: `nwr(around:110,lat,lng)` for food `amenity` values (restaurant, cafe, fast_food, food_court, bar, pub, ice_cream, canteen) and food `shop` values, `out center tags`. Candidates are then filtered to 100 m from the actual pin.
- **Stored:** `state`, `checked_at`, and per candidate only `id` (`node|way|relation/<n>`), `distance_m` and `name_score`. Tags are used in memory to compute the name score and then dropped. Every written `osm` object validates against `schema.json#/$defs/osm_link`. A tested assertion checks that no OSM name, tag or coordinate shows up in `public.places`, and that no table has an OSM or tag column.
- Name score: the best trigram similarity (computed like `pg_trgm`) between any of our `names`/`aliases` values and any OSM `name`, `name:*`, `alt_name`, `old_name`, `short_name`, `official_name` or `brand`, after lower-casing. Range [0, 1], rounded to 2 decimals. Up to 5 candidates are kept, best first.
- Politeness: at most 1 request per 10 s; User-Agent `eatsights/<version> (+https://eatsights.letsimpl.fyi)`; 30 s HTTP timeout; in-memory response cache keyed by the pin rounded to 4 decimals (about 11 m), 24 h TTL, at most 2000 entries; on 429, 5xx or a network error it backs off exponentially, from 60 s up to 1 h (or longer if `Retry-After` asks for it).
- The checker never links. A candidate becomes `linked` only through `POST /v1/admin/osm-links`.
- A check only writes if the `osm` object is unchanged since it was read, so a concurrent admin link is never overwritten.
- **Dependency:** `reqwest 0.12` with `rustls-tls`. It reuses `hyper`, `hyper-util`, `rustls` (ring) and `webpki-roots`, which were already in the tree through poem and sqlx. It adds no OpenSSL and no aws-lc; `cargo deny` passes. Writing a hand-rolled HTTPS client on top of hyper would mean more code to audit than the few small crates this adds (`hyper-rustls`, `tower-http`, `serde_urlencoded`).

## `ready_to_map` (ADR-0006 #7)

`osm.ready_to_map = true` when all of these hold:

- the place is active
- `osm.state = unmapped`
- it has a non-empty name, a category and a pin
- its `status.operating` fact is `confirmed`, with at least 2 distinct contributors, `current_value.state = active`, and `last_observed_at` within 90 days

A worker task follows the change feed (cursor `ready_to_map`) for place changes and `status.operating` fact changes. Once an hour it also re-checks flagged places so the 90-day window expires. It writes `true` when the rule holds. It writes `false` only over an earlier `true`; a missing field means false, which avoids writing to every place. The only addition to the ADR rule is the `current_value.state = active` check (a confirmed `permanently_closed` place is not ready to map).

## Duplicate detector (AGENTS.md 9.3, ADR-0006 #6)

- Follows the change feed (cursor `dedupe`) for created or edited places. On first start it scans every place.
- Finds candidates in the 3x3 block of precision-6 geohash cells around the pin, with one `COLLATE "C"` btree range scan per cell (LATERAL + `OFFSET 0` fence, ADR-0001). It keeps same-category active places within 150 m (haversine).
- Similarity is the best `pg_trgm` `similarity` over all pairs of `names` values, each passed through `es_fold` (`unaccent` + lower-case).
- A pair is suggested when (distance <= 50 m AND similarity >= 0.4) OR (distance <= 150 m AND similarity >= 0.7). The numbers live in `DedupeConfig::default()`.
- Suggestions go to `private.merge_suggestions`: one row per unordered pair (`a_id < b_id`, unique), with `score` (the similarity), `distance_m`, `status` (`open`/`confirmed`/`dismissed`), `decided_at` and `decided_by` (the admin action id). Re-detection updates only `open` rows.
- Nothing is merged automatically. Suggestions are private: only the CLI `eatsights dedupe list` and the admin endpoint (by `suggestion_id`) use them.

## Admin endpoints (AGENTS.md 6.3)

Every admin call is one signed JSON object, signed the same way as everything else (Ed25519 over the RFC 8785 form without `signature`):

```json
{ "id": "es_<ULID>", "action": "...", "params": { }, "issued_at": "<RFC 3339>",
  "issuer_key_id": "<root or verifier key_id>", "signature": "<86>" }
```

- **Id prefix:** `schema.json` defines no admin id, so no new prefix was invented. Admin actions reuse the `es_` envelope id from proposal 0001. They are kept in their own table, `private.admin_actions`, which stores the full signed object, the result and `issued_at`, and is append-only by trigger.
- **Issuer:** an active `root` or `verifier` key (otherwise 403). Bad signature: 401. Per-key and per-IP rate limits apply.
- **Replay protection:** `issued_at` must be within 5 minutes of server time (422 otherwise), and the `id` is unique. Resending the identical object returns the stored result with `status: "duplicate"`. A different object with a used id is `409 replay`.
- Unknown top-level fields or params, or an action that does not belong to the endpoint: 422.

| Endpoint | `action` | `params` |
|---|---|---|
| `POST /v1/admin/osm-links` | `link_osm` | `place_id`, `osm_id` (`node/<n>`, `way/<n>`, `relation/<n>`). Result: `osm = {state: linked, id, verified_at, checked_at?}`. Any valid id is accepted, not only current candidates, because a person may have found the object by hand. |
| `POST /v1/admin/merges` | `merge` | `entity_type` (`place` by default, or `menu_item`), and either `a_id` + `b_id` or `suggestion_id`; optional `survivor_id` |
| | `unmerge` | `entity_type`, `id` |
| | `dismiss` | `suggestion_id` |
| `POST /v1/admin/removals` | `remove_observation` | `observation_id`, `reason`: `legal` or `safety` |

Responses: `200 {id, action, status: accepted|duplicate, result}` or problem+json.

### Merge rules

- **Survivor:** the OSM-linked record, otherwise the older one (`created_at`, ties broken by the smaller id), unless `survivor_id` says otherwise.
- **No chains:** if the chosen survivor is itself merged, the loser points at its final survivor. Records that pointed at the loser are re-pointed to the final survivor (listed in `result.repointed`), so redirects are always one hop.
- **Refused (409):** a loser that is already merged (undo that merge first), any merge that would create a cycle, and menu items from different places.
- The loser gets `record_state: merged` and `merged_into` in both its columns and its document. The document still validates against the schema. Nothing is deleted. `GET /v1/places/{loser}` answers 301 to the survivor.
- **Undo** clears `merged_into` and sets the record back to `active`. Records that were re-pointed by a later merge stay with their new survivor. Undo each one separately if needed.
- A confirmed merge marks any matching open suggestion `confirmed`.
- **Facts:** each merge or undo writes `place`/`menu_item` changes with op `merge`/`unmerge` for both records. The aggregation worker then recomputes every property observed on the affected subjects. Aggregation already followed `merged_into` (`resolve_subject` / `merged_sources`); what was missing was this recomputation and the cleanup of stale facts. After a merge, the loser's facts are deleted (they now live on the survivor). After an undo, survivor facts that no longer have any observations are deleted. Each deletion is recorded in the feed (`fact`, op `delete`). Migration 0004 grants `DELETE` on `public.facts` for this.

### Removals

`remove_observation` calls the existing `SECURITY DEFINER public.redact_observation`. The public observation and the private original become tombstones. The public `removals` log gets only the observation id and the reason code: no free text and no content. Removing an observation that is already removed gives 409; an unknown observation gives 404. The aggregation worker recomputes the affected fact from the `redact` change.

### What is public

The admin endpoints, the change feed entries (`place … merge|unmerge|osm|ready_to_map`, `fact … delete`, `observation … redact`), the merged records (`merged_into`) and the removal log. The signed admin objects, issuer key ids and suggestions stay in `private`.

## Configuration

| Variable | Default | |
|---|---|---|
| `EATSIGHTS_OSM_ENDPOINT` | unset (checker off) | Overpass interpreter URL |
| `EATSIGHTS_OSM_RECHECK_DAYS` | 30 | re-check `unmapped` |

The duplicate detector and `ready_to_map` run whenever the `worker` role runs, on the same wake-up and interval as aggregation.

## Deviations and notes

- Admin ids use the `es_` prefix (see above), not a new prefix.
- `ready_to_map` also requires `current_value.state = active` (see above).
- `link_osm` accepts any well-formed OSM id, not only listed candidates.
- Undoing a merge does not restore records that were re-pointed by a later merge.
- The admin replay window (5 minutes) is a constant, `ADMIN_WINDOW`.
- Tests: the new `m5` integration tests serialise their migrations with a mutex. On a fresh cluster, concurrent first-time runs of `0001_init.sql` can race on `CREATE ROLE` (cluster-wide). This was seen once in the existing `integration` tests; 0001 was not changed.
