\set n random(1, 1000)
SELECT lat AS clat, lng AS clng FROM bench_centers WHERE n = :n \gset
SELECT p.id, p.name FROM places p
WHERE ST_DWithin(p.geog, ST_SetSRID(ST_MakePoint(:clng, :clat),4326)::geography, 1000)
LIMIT 500;
