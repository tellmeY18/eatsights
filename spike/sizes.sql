-- table / index sizes and a text-vs-bytea ID size comparison
SELECT relname, pg_size_pretty(pg_total_relation_size(oid)) total, pg_size_pretty(pg_relation_size(oid)) heap
FROM pg_class WHERE relname IN ('places','changes') ;
SELECT 'observations (all partitions)' t,
  pg_size_pretty(sum(pg_relation_size(c.oid))) heap,
  pg_size_pretty(sum(pg_indexes_size(c.oid))) indexes
FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid WHERE i.inhparent = 'observations'::regclass;
SELECT idx, pg_size_pretty(sum(pg_relation_size(c.oid))) FROM (
  SELECT regexp_replace(c2.relname, '^observations_\d+_', '') idx, c2.oid FROM pg_inherits i
  JOIN pg_index x ON x.indrelid = i.inhrelid JOIN pg_class c2 ON c2.oid = x.indexrelid
  WHERE i.inhparent = 'observations'::regclass) c GROUP BY idx;
SELECT c.relname, pg_size_pretty(pg_relation_size(c.oid)) FROM pg_class c
WHERE c.relname IN ('places_pkey','places_geohash','places_geog','places_earth','places_name_trgm');
-- ID representation: 29-char text vs 16-byte bytea, 1M rows with btree
DROP TABLE IF EXISTS ids_text, ids_bytes;
CREATE TABLE ids_text AS SELECT id FROM observations;
CREATE TABLE ids_bytes AS SELECT substr(sha256(id::bytea),1,16) AS id FROM observations;
CREATE INDEX ON ids_text(id); CREATE INDEX ON ids_bytes(id);
SELECT 'text' r, pg_size_pretty(pg_relation_size('ids_text')) heap, pg_size_pretty(pg_indexes_size('ids_text')) idx
UNION ALL SELECT 'bytea16', pg_size_pretty(pg_relation_size('ids_bytes')), pg_size_pretty(pg_indexes_size('ids_bytes'));
DROP TABLE ids_text, ids_bytes;
SELECT avg(n) avg_rows_1km, max(n) FROM (SELECT (SELECT count(*) FROM places p WHERE ST_DWithin(p.geog, ST_SetSRID(ST_MakePoint(c.lng,c.lat),4326)::geography,1000)) n FROM bench_centers c) s;
SELECT avg(n), max(n) FROM (SELECT count(*) n FROM observations GROUP BY subject_id, property) s;
