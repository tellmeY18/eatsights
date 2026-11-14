# ADR-0001: Database

- Status: proposed (for maintainer review)
- Date: 2026-11-14
- Spike: `spike/` (schema, loader, pgbench scripts, size queries)

**Geo decision in one line:** the MVP uses geohash + btree (no PostGIS). The spike shows it meets the budget for nearby search: p99 0.5 ms for a cell query, 3.1 ms for a 1 km radius. PostGIS is **not** required.

## Decision

- **PostgreSQL 18, one instance** (D1). No read replica and no SQLite read copy.
- Extensions: `pg_trgm` and `unaccent` only. No `postgis`, `cube` or `earthdistance`.
- Places store `lat`, `lng` and a precision-9 `geohash text COLLATE "C"` with a plain btree index.
- IDs are prefixed ULIDs stored as `text` with a `CHECK` regex.
- `observations` is partitioned by month on `submitted_at`, with `value jsonb` and an index on `(subject_id, property, observed_at DESC)`.

## Alternatives against the needs in AGENTS.md 5.3

| Need (5.3) | PostgreSQL | SQLite | MySQL 8 | MongoDB | TimescaleDB | ClickHouse |
|---|---|---|---|---|---|---|
| Two schemas + role separation (read/write/worker) | yes | no (one file, no roles) | databases + grants, ok | per-DB roles, weaker | yes (is Postgres) | coarse |
| Append-only in DB (REVOKE + trigger, SECURITY DEFINER redaction) | yes | triggers only, no roles | triggers, no SECURITY DEFINER equivalent as clean | no triggers on writes | yes | insert-only fits, but no per-role function security |
| Transactional write of submission + observation + `changes` | yes | yes, single writer | yes | multi-doc txns, heavier | yes | no real transactions |
| Monotonic `changes.seq` | identity column | yes | auto_increment | no native sequence | yes | no |
| Monthly partitions, `jsonb` | native | no partitioning | partitions, JSON weaker | n/a | hypertables (extra extension) | strong for analytics |
| Name search (`pg_trgm`), geo | yes | FTS only, no trigram | ngram FTS | text + 2dsphere | yes | limited |
| One small container, ops simplicity | good | best, but multi-process writers and K8s volumes are awkward | ok | heavier RAM | Postgres + extension (bigger image, licence split) | heavy, OLAP-oriented |

PostgreSQL is the only option that covers every row without extra services. TimescaleDB adds nothing we need over native partitioning. ClickHouse is built for analytics, not transactional writes. SQLite cannot enforce the role separation.

## Spike results

**Machine: macOS dev laptop (Apple M1 Pro, 10 cores, 16 GB).** This is **not** a constrained container. Postgres 18.7 ran with `shared_buffers=128MB` (set by `pg-start`), and the client ran on the same host. Treat these numbers as a ceiling and an order-of-magnitude check, not proof of the 8.1 budgets. M6 must repeat them under CPU and memory limits.

**Data** (`spike/load.sql`):

- 50,000 places in the Kerala bounding box: 70% clustered around 10 towns, 30% uniform.
- 1,000,000 observations spread over 6 months (7 monthly partitions). The property mix follows `properties.json`: price 30%, availability 25%, quality, place status, queue length and others. Load is skewed toward busy places.
- Bulk load took about 45 s, including indexes and the matching `changes` rows.
- A 1 km radius returns on average 56 places (maximum 174).

**Latency** (`spike/bench/run.sh`): pgbench with 4 clients for 15 s per script. Percentiles come from the per-transaction log, and every query hit a warm cache. Each geo script includes one extra small lookup query to fetch the test centre, so true query latency is slightly lower.

| Query | TPS | p50 | p99 |
|---|---|---|---|
| (a) PostGIS `ST_DWithin` on geography + GiST, 1 km | 7,577 | 0.52 ms | 0.75 ms |
| (b1) geohash: precision-6 cell + 8 neighbours (API `cell=` path) | 12,722 | 0.30 ms | 0.49 ms |
| (b2) geohash: precision-5 3x3 cells + haversine, 1 km | 2,438 | 1.84 ms | 3.08 ms |
| (c) `earthdistance` + `cube` (`earth_box` GiST), 1 km (available in the nix Postgres) | 6,062 | 0.60 ms | 1.37 ms |
| Latest 20 observations for one (subject, property) | 15,537 | 0.25 ms | 0.38 ms |
| Insert observation + `changes` row (one statement, autocommit) | 16,921 | 0.21 ms | 0.55 ms |

Every query is far below the 20 ms uncached p99 budget.

**Sizes** (measured after the insert benchmark, so about 2.35M observations):

| Object | Size |
|---|---|
| Observation heap, all partitions | 493 MB (about 210 B/row) |
| Observation indexes, all partitions | 513 MB total: pkey 171 MB, latest-per-subject 196 MB, place/time 146 MB |
| `changes` (2.35M rows) | 277 MB total |
| `places` total | 24 MB |
| Place geo indexes | geohash btree 1.5 MB, PostGIS GiST 3.5 MB, earth GiST 3.8 MB, trigram GIN 5.3 MB |

The observation indexes are larger than the observation heap. At about 0.4 kB per observation (heap plus indexes, without `changes`), 128 MB of shared buffers holds the hot recent partitions and the OS page cache holds the rest. Revisit the place/time index in M1 if no query needs it.

**Text vs 16-byte IDs** (2.35M IDs in a single-column table with a btree index):

| Storage | Heap | Index |
|---|---|---|
| `text` | 135 MB | 111 MB |
| 16-byte `bytea` | 117 MB | 91 MB |

16-byte IDs save about 13% of heap and 18% of index, and only on ID columns. That is not worth the extra encoding code. **Keep `text` with a CHECK constraint.**

## Geo decision

Choose **geohash + btree** (the MVP plan). It is the simplest option: no extension, the smallest index, and cell URLs that map directly to the cacheable `cell=` endpoint in 8.2.3. Its 1 km radius p99 (3 ms) is well under 20 ms. PostGIS is about 4x faster for radius queries, but it is not needed to meet the budget and it makes the image heavier. Reconsider PostGIS only if radius or polygon queries become common and measurably miss the budget.

Pitfalls found in the spike that the M1 queries must follow:

1. **Collation.** Under the default libc collation, a range scan of the form `geohash >= cell AND geohash < cell || '~'` returned wrong rows (zero). Declare the column `COLLATE "C"` and use a plain btree index. Do not use `text_pattern_ops`.
2. **No `LIKE` with a computed pattern.** `LIKE cell || '%'` with a non-constant pattern cannot use the index and caused sequential scans (about 30 ms).
3. **Force an index scan per cell.** Query each cell with `unnest($cells) CROSS JOIN LATERAL (... WHERE geohash >= cell AND geohash < cell || '~' OFFSET 0)`. Without the lateral subquery and the `OFFSET 0` fence, the planner pushed the haversine filter or `LIMIT` into a sequential scan (13–46 ms).
4. **Cell size.** A precision-6 cell is about 1.2 x 0.6 km, so the cell plus its 8 neighbours does not cover a 1 km radius. Use precision 5 for `near=...&radius<=2km` and keep precision 6 for `cell=`.

## Read replica trigger

Do not add a replica until **both** of these hold:

- The in-process and CDN cache hit ratio for public GETs has been tuned to 99% or more.
- Then **either** the primary sustains more than 70% CPU from read queries for 15 minutes or longer at peak, **or** uncached indexed read p99 at the origin exceeds 20 ms while write latency is fine.

Record the measurement in a new ADR before you add a replica.
