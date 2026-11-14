\set n random(1, 1000)
SELECT cells6::text AS cells FROM bench_centers WHERE n = :n \gset
-- API path: cell= precision 6 + 8 neighbours (cacheable cell query); per-cell index range scan
SELECT p.id, p.name FROM unnest(':cells'::text[]) cell
CROSS JOIN LATERAL (SELECT id, name FROM places
  WHERE geohash >= cell AND geohash < cell || '~' LIMIT 500) p
LIMIT 500;
