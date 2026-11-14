-- ADR-0001 spike schema (draft, not the migration). Close to AGENTS.md 5.3.
CREATE EXTENSION IF NOT EXISTS postgis;
CREATE EXTENSION IF NOT EXISTS cube;
CREATE EXTENSION IF NOT EXISTS earthdistance;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
DROP TABLE IF EXISTS places, observations, changes, bench_centers, ids_text, ids_bytes CASCADE;

CREATE TABLE places (
  id       text PRIMARY KEY CHECK (id ~ '^pl_[0-9A-HJKMNP-TV-Z]{26}$'),
  name     text NOT NULL,
  category text NOT NULL,
  lat      double precision NOT NULL,
  lng      double precision NOT NULL,
  geohash  text COLLATE "C" NOT NULL,    -- precision 9; C collation so range prefix scans work
  geog     geography(Point,4326)         -- PostGIS variant only
);
-- (b) geohash prefix, queried as a range: geohash >= cell AND geohash < cell || '~'
CREATE INDEX places_geohash ON places (geohash);
-- (a) PostGIS
CREATE INDEX places_geog ON places USING gist (geog);
-- (c) earthdistance + cube
CREATE INDEX places_earth ON places USING gist (ll_to_earth(lat, lng));
CREATE INDEX places_name_trgm ON places USING gin (name gin_trgm_ops);

CREATE TABLE observations (
  id            text NOT NULL CHECK (id ~ '^ob_[0-9A-HJKMNP-TV-Z]{26}$'),
  scope         text NOT NULL,           -- place | menu_item | dish
  subject_id    text NOT NULL,           -- pl_/mi_/di_ id
  place_id      text,
  property      text NOT NULL,
  kind          text NOT NULL DEFAULT 'value',
  value         jsonb,
  class         text NOT NULL,
  pseudonym     text NOT NULL,
  pseudonym_salt_version int NOT NULL DEFAULT 1,
  observed_at   timestamptz NOT NULL,
  submitted_at  timestamptz NOT NULL,
  PRIMARY KEY (id, submitted_at)
) PARTITION BY RANGE (submitted_at);

DO $$
DECLARE m date := date '2026-05-01';
BEGIN
  WHILE m < date '2026-12-01' LOOP
    EXECUTE format('CREATE TABLE observations_%s PARTITION OF observations FOR VALUES FROM (%L) TO (%L)',
                   to_char(m,'YYYYMM'), m, m + interval '1 month');
    m := m + interval '1 month';
  END LOOP;
END $$;

-- latest per (subject, property)
CREATE INDEX obs_latest ON observations (subject_id, property, observed_at DESC);
CREATE INDEX obs_place ON observations (place_id, submitted_at);

CREATE TABLE changes (
  seq         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  entity_type text NOT NULL,
  entity_id   text NOT NULL,
  op          text NOT NULL,
  at          timestamptz NOT NULL DEFAULT now()
);
