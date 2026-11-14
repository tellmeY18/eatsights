# M5 (part): exports, change feed, stale sweep

Date: 2026-11-14. Covers AGENTS.md 9.4, 8.2 rule 5 and the stale sweep deferred in `docs/m1-m3-mvp.md`. OSM, duplicates, merges and trust are handled in other M5/M4 changes.

## Exports

- **What:** gzip NDJSON, one file per public dataset: `places`, `sections`, `menu_items`, `dishes`, `observations` (public form: class + pseudonym), `facts`, `vendor_of` (public vendor bindings plus `revoked`) and `registry`. Published keys are not exported (`/v1/keys/published` serves them). Nothing from `private` is exported.
- **Consistency:** one `REPEATABLE READ, READ ONLY` transaction per snapshot. `manifest.changes_seq` is the `max(changes.seq)` visible in that snapshot, so the snapshot contains every change up to and including it.
- **Memory:** rows are read in keyset batches of 1000 (`id`, or `(subject_key, property)` for facts) and streamed through gzip to disk. The SHA-256 and byte count are computed while writing.
- **Layout** (`EATSIGHTS_EXPORT_DIR`, default `./.dev/exports`; `/var/lib/eatsights/exports` in k8s): one directory per snapshot, named `<UTC time>-<changes_seq>`. Each holds `manifest.json` and `*.ndjson.gz`. A `latest` file holds the current snapshot's name. Each snapshot is written to `.tmp-*`, then renamed, then `latest` is replaced atomically. The newest `EATSIGHTS_EXPORT_KEEP` (7) snapshots are kept.
- **Manifest:** `{snapshot, generated_at, changes_seq, license, files: [{name, bytes, sha256, records}]}`.
- **Schedule:** the worker task (`eatsights_worker::export::run`) exports at start-up if the latest snapshot is older than `EATSIGHTS_EXPORT_INTERVAL_HOURS` (24), then once per interval. After a failure, it retries in 5 minutes.
- **CLI:** `eatsights export <dir>` runs the same code (`write_snapshot`) and prints the manifest.
- **Privacy:** a unit test checks that the export SQL never references `private.`. An integration test checks that exported files contain no contributor or root `key_id` and no `signature`, and that record counts and checksums match. The e2e test checks the same against the live server.

## API

- `GET /v1/export`: the latest manifest, plus a `url` for each file and `next` (`/v1/changes?after=<changes_seq>`). Returns 404 when there is no snapshot yet. `ETag` is the snapshot name. `Cache-Control: max-age=300, stale-if-error=604800`.
- `GET /v1/export/{name}`: `places.ndjson.gz` (or `places`) from the latest snapshot, served as `application/gzip` and streamed from disk. `ETag` is `"<sha256>"`, and a matching `If-None-Match` returns 304. `Cache-Control: max-age=3600, stale-while-revalidate=86400, stale-if-error=604800`. Only names listed in the manifest are served, and the snapshot name is validated, so a request path never reaches the filesystem. Traversal attempts return 404.
- The OpenAPI descriptions document the bootstrap: fetch the manifest, download and verify the files, then follow `/v1/changes?after=<changes_seq>`.

## Change feed

- `GET /v1/changes?after=&limit=` (unchanged contract: `items`, `next_after`). Its OpenAPI description now lists the entity types and the paging rule.
- Migration `0003_exports.sql` adds the covering index `changes_feed (seq) INCLUDE (entity_type, entity_id, op, at)`, and lowers `autovacuum_vacuum_insert_scale_factor` on `changes` so the visibility map stays current. The feed query is an **Index Only Scan** with a limit (asserted with `EXPLAIN` in `change_feed_is_an_index_only_scan`).

## Stale sweep

- `eatsights_worker::sweep` runs every `EATSIGHTS_STALE_SWEEP_MINUTES` (15). It selects facts that are not `stale` and whose `last_observed_at` is older than `stale_half_lives × half_life_hours` (from `aggregation.toml` and the fact itself), in keyset chunks of 500. It recomputes each one with the normal aggregation and rebuilds the affected menus.
- `upsert_fact` writes, and appends to `changes`, only when the fact's content changed, so a repeated sweep writes nothing. This is tested with `sweep_at(now + 7h)` on a 2 h half-life property: the first pass adds `fact` and `menu` changes, and the second pass adds none.
- **Limit:** the candidate filter covers the "too old" stale rule only. A fact that would turn stale earlier because decayed support drops below `stale_min_support` is caught once it passes the age rule, or on its next observation.

## Config

| Env | Default |
|---|---|
| `EATSIGHTS_EXPORT_DIR` | `.dev/exports` |
| `EATSIGHTS_EXPORT_INTERVAL_HOURS` | 24 |
| `EATSIGHTS_EXPORT_KEEP` | 7 |
| `EATSIGHTS_STALE_SWEEP_MINUTES` | 15 |

`deploy/deployment.yaml` mounts an `emptyDir` at the export directory. `deploy/README.md` explains when to use a PVC instead.

## Other changes

- `eatsights-api` now depends on `eatsights-worker` (it was a dev-dependency) for the manifest type. `eatsights-worker` gained `flate2`, `sha2` and `thiserror`, all already in the dependency tree. The binary no longer depends on `flate2` directly.
- Integration tests serialise `migrate()` behind a mutex, because concurrent migrations raced on the cluster-wide roles.
