\set n random(1, 1000)
SELECT cells5::text AS cells, lat AS clat, lng AS clng FROM bench_centers WHERE n = :n \gset
-- 1 km radius: 3x3 cells at precision 5 (~4.9 km) computed by the server, then haversine.
-- LATERAL per cell forces an index range scan (a plain join + LIMIT picked a seq scan).
SELECT p.id, p.name FROM unnest(':cells'::text[]) cell
CROSS JOIN LATERAL (SELECT id, name, lat, lng FROM places
  WHERE geohash >= cell AND geohash < cell || '~' OFFSET 0) p  -- OFFSET 0 fences the haversine out of the scan
WHERE 2*6371000*asin(sqrt(power(sin(radians(p.lat - :clat)/2),2)
      + cos(radians(:clat))*cos(radians(p.lat))*power(sin(radians(p.lng - :clng)/2),2))) <= 1000
LIMIT 500;
