\set n random(1, 1000)
SELECT lat AS clat, lng AS clng FROM bench_centers WHERE n = :n \gset
SELECT p.id, p.name FROM places p
WHERE earth_box(ll_to_earth(:clat, :clng), 1000) @> ll_to_earth(p.lat, p.lng)
  AND earth_distance(ll_to_earth(:clat, :clng), ll_to_earth(p.lat, p.lng)) <= 1000
LIMIT 500;
