-- ~50k places in Kerala (lat 8.2..12.8, lng 74.9..77.4), 1M observations over 6 months.
CREATE OR REPLACE FUNCTION fake_ulid(prefix text) RETURNS text LANGUAGE sql AS $$
  SELECT prefix || string_agg(substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ', 1 + floor(random()*32)::int, 1), '')
  FROM generate_series(1,26) $$;

-- 70% clustered around 10 towns (sigma ~4 km), 30% uniform.
WITH towns(lat,lng) AS (VALUES (8.52,76.94),(9.93,76.27),(11.26,75.78),(10.53,76.21),(8.89,76.61),
  (9.59,76.52),(11.87,75.37),(10.79,76.65),(9.49,76.33),(11.05,76.07)),
pts AS (
  SELECT g,
    CASE WHEN g % 10 < 7 THEN (SELECT lat FROM towns OFFSET (g % 10) LIMIT 1) + (random()+random()+random()-1.5)*0.07
         ELSE 8.2 + random()*4.6 END AS lat,
    CASE WHEN g % 10 < 7 THEN (SELECT lng FROM towns OFFSET (g % 10) LIMIT 1) + (random()+random()+random()-1.5)*0.07
         ELSE 74.9 + random()*2.5 END AS lng
  FROM generate_series(1,50000) g)
INSERT INTO places (id,name,category,lat,lng,geohash,geog)
SELECT fake_ulid('pl_'), 'Chaikada ' || g, (ARRAY['tea_shop','restaurant','bakery','juice_shop','hotel'])[1+g%5],
       lat, lng, ST_GeoHash(ST_SetSRID(ST_MakePoint(lng,lat),4326),9),
       ST_SetSRID(ST_MakePoint(lng,lat),4326)::geography
FROM pts;

-- 10 menu items per place (ids only, as subjects)
CREATE TEMP TABLE pl AS SELECT row_number() OVER () AS n, id FROM places;
CREATE INDEX ON pl(n);

-- property mix: mostly menu-item price/availability, then place hours/status/queue
CREATE TEMP TABLE mix(property text, scope text, w int, v text);
INSERT INTO mix VALUES
 ('price.amount','menu_item',30,'{"amount":%s,"currency":"INR"}'),
 ('availability.observed','menu_item',25,'{"available":true}'),
 ('quality.score','menu_item',10,'{"score":%s}'),
 ('prep.wait_time','menu_item',5,'{"minutes":%s}'),
 ('listing.status','menu_item',5,'{"status":"listed"}'),
 ('status.operating','place',8,'{"status":"open"}'),
 ('queue.length','place',7,'{"people":%s}'),
 ('hours.observed_open','place',5,'{"open":true}'),
 ('payment.methods','place',3,'{"methods":["cash","upi"]}'),
 ('hygiene.score','place',2,'{"score":%s}');
CREATE TEMP TABLE mixc AS SELECT property, scope, v,
  sum(w) OVER (ORDER BY property) - w AS lo, sum(w) OVER (ORDER BY property) AS hi FROM mix;

INSERT INTO observations (id,scope,subject_id,place_id,property,value,class,pseudonym,observed_at,submitted_at)
SELECT fake_ulid('ob_'), m.scope,
       CASE WHEN m.scope='place' THEN p.id ELSE 'mi_' || substr(p.id,4,24) || lpad((x.r % 10)::text,2,'0') END,
       p.id, m.property, replace(m.v,'%s',(x.r % 90 + 10)::text)::jsonb,
       (ARRAY['unverified','unverified','unverified','member','trusted','vendor'])[1 + x.r % 6],
       substr(md5(x.r::text || p.id),1,22), x.t - interval '2 minutes', x.t
FROM (SELECT g, floor(random()*1e9)::bigint AS r,
             1 + floor(power(random(),2)*50000)::int AS pn,   -- skew: some places busy
             floor(random()*100)::int AS pick,
             timestamptz '2026-05-15' + random()*interval '180 days' AS t
      FROM generate_series(1,1000000) g) x
JOIN pl p ON p.n = x.pn
JOIN mixc m ON x.pick >= m.lo AND x.pick < m.hi;

INSERT INTO changes (entity_type,entity_id,op) SELECT 'observation', id, 'insert' FROM observations;

-- bench centres: 1000 random places, with 3x3 geohash cells at precision 5 and 6
CREATE TABLE bench_centers AS
SELECT row_number() OVER () AS n, lat, lng,
  ARRAY(SELECT DISTINCT ST_GeoHash(ST_SetSRID(ST_MakePoint(lng+dx*0.044,lat+dy*0.044),4326),5)
        FROM generate_series(-1,1) dx, generate_series(-1,1) dy) AS cells5,
  ARRAY(SELECT DISTINCT ST_GeoHash(ST_SetSRID(ST_MakePoint(lng+dx*0.011,lat+dy*0.0055),4326),6)
        FROM generate_series(-1,1) dx, generate_series(-1,1) dy) AS cells6
FROM (SELECT lat,lng FROM places ORDER BY random() LIMIT 1000) s;
CREATE TABLE bench_subjects AS SELECT row_number() OVER () AS n, subject_id, property
FROM (SELECT DISTINCT subject_id, property FROM observations ORDER BY 1,2 LIMIT 0) s;
INSERT INTO bench_subjects SELECT row_number() OVER (), subject_id, property
FROM (SELECT subject_id, property FROM observations TABLESAMPLE SYSTEM(1) LIMIT 5000) s;
CREATE INDEX ON bench_subjects(n);
VACUUM ANALYZE;
CREATE UNIQUE INDEX ON bench_centers(n); VACUUM ANALYZE bench_centers;
