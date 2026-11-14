-- Eatsights MVP schema (forward-only). See AGENTS.md 5.3 and docs/mvp-contract.md.

CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS unaccent;

CREATE SCHEMA IF NOT EXISTS private;

-- Group roles (NOLOGIN). Login roles are granted membership out of band.
-- Managed databases may refuse CREATE ROLE; then an operator creates them.
DO $$
DECLARE r text;
BEGIN
  FOREACH r IN ARRAY ARRAY['eatsights_read', 'eatsights_write', 'eatsights_worker'] LOOP
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
      BEGIN
        EXECUTE format('CREATE ROLE %I NOLOGIN', r);
      EXCEPTION WHEN insufficient_privilege THEN
        RAISE NOTICE 'cannot create role %, create it manually', r;
      END;
    END IF;
  END LOOP;
END $$;

-- Immutable wrapper so unaccent can be used in generated columns / indexes.
CREATE FUNCTION public.es_fold(t text) RETURNS text
  LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT
  AS $$ SELECT lower(public.unaccent('public.unaccent'::regdictionary, t)) $$;

-- ---------------------------------------------------------------- entities

CREATE TABLE public.places (
  id           text PRIMARY KEY CHECK (id ~ '^pl_[0-9A-HJKMNP-TV-Z]{26}$'),
  doc          jsonb NOT NULL,
  search_text  text NOT NULL,
  category     text NOT NULL,
  city         text NOT NULL,
  lat          double precision NOT NULL,
  lng          double precision NOT NULL,
  geohash      text COLLATE "C" NOT NULL,
  record_state text NOT NULL DEFAULT 'active' CHECK (record_state IN ('active', 'merged')),
  merged_into  text REFERENCES public.places (id),
  created_at   timestamptz NOT NULL,
  updated_at   timestamptz NOT NULL
);
CREATE INDEX places_geohash ON public.places (geohash);
CREATE INDEX places_city ON public.places (city, id);
CREATE INDEX places_category ON public.places (category, id);
CREATE INDEX places_search_trgm ON public.places USING gin (search_text gin_trgm_ops);

CREATE TABLE public.sections (
  id         text PRIMARY KEY CHECK (id ~ '^sc_[0-9A-HJKMNP-TV-Z]{26}$'),
  place_id   text NOT NULL REFERENCES public.places (id),
  doc        jsonb NOT NULL,
  sort       integer,
  created_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL
);
CREATE INDEX sections_place ON public.sections (place_id);

CREATE TABLE public.dishes (
  id           text PRIMARY KEY CHECK (id ~ '^di_[0-9A-HJKMNP-TV-Z]{26}$'),
  doc          jsonb NOT NULL,
  search_text  text NOT NULL,
  created_at   timestamptz NOT NULL,
  updated_at   timestamptz NOT NULL
);
CREATE INDEX dishes_search_trgm ON public.dishes USING gin (search_text gin_trgm_ops);

CREATE TABLE public.menu_items (
  id           text PRIMARY KEY CHECK (id ~ '^mi_[0-9A-HJKMNP-TV-Z]{26}$'),
  place_id     text NOT NULL REFERENCES public.places (id),
  section_id   text REFERENCES public.sections (id),
  dish_id      text REFERENCES public.dishes (id),
  doc          jsonb NOT NULL,
  record_state text NOT NULL DEFAULT 'active' CHECK (record_state IN ('active', 'merged')),
  merged_into  text REFERENCES public.menu_items (id),
  created_at   timestamptz NOT NULL,
  updated_at   timestamptz NOT NULL
);
CREATE INDEX menu_items_place ON public.menu_items (place_id);
CREATE INDEX menu_items_merged ON public.menu_items (merged_into) WHERE merged_into IS NOT NULL;

-- ---------------------------------------------------------------- observations

CREATE TABLE public.observations (
  id                     text NOT NULL CHECK (id ~ '^ob_[0-9A-HJKMNP-TV-Z]{26}$'),
  property               text NOT NULL,
  scope                  text NOT NULL CHECK (scope IN ('place', 'menu_item', 'dish')),
  place_id               text,
  menu_item_id           text,
  dish_id                text,
  subject_key            text NOT NULL,
  doc                    jsonb NOT NULL,
  pseudonym              text NOT NULL,
  pseudonym_salt_version integer NOT NULL,
  class                  text NOT NULL,
  observed_at            timestamptz NOT NULL,
  submitted_at           timestamptz NOT NULL,
  PRIMARY KEY (id, submitted_at)
) PARTITION BY RANGE (submitted_at);

DO $$
DECLARE m date := date '2026-01-01';
BEGIN
  WHILE m < date '2029-01-01' LOOP
    EXECUTE format(
      'CREATE TABLE public.observations_%s PARTITION OF public.observations FOR VALUES FROM (%L) TO (%L)',
      to_char(m, 'YYYYMM'), m, (m + interval '1 month')::date);
    m := (m + interval '1 month')::date;
  END LOOP;
END $$;
CREATE TABLE public.observations_default PARTITION OF public.observations DEFAULT;

CREATE INDEX observations_subject ON public.observations (subject_key, property, observed_at DESC);
CREATE INDEX observations_place ON public.observations (place_id, id);
CREATE INDEX observations_menu_item ON public.observations (menu_item_id, id);
CREATE INDEX observations_dish ON public.observations (dish_id, id);
CREATE INDEX observations_property ON public.observations (property, id);
CREATE INDEX observations_id ON public.observations (id);

-- Global idempotency and lookup (the partitioned PK includes submitted_at).
CREATE TABLE public.observation_ids (
  id           text PRIMARY KEY,
  submitted_at timestamptz NOT NULL,
  property     text NOT NULL,
  subject_key  text NOT NULL,
  place_id     text
);

-- ---------------------------------------------------------------- facts and documents

CREATE TABLE public.facts (
  subject_key  text NOT NULL,
  property     text NOT NULL,
  place_id     text,
  menu_item_id text,
  dish_id      text,
  doc          jsonb NOT NULL,
  computed_at  timestamptz NOT NULL,
  PRIMARY KEY (subject_key, property)
);
CREATE INDEX facts_place ON public.facts (place_id);
CREATE INDEX facts_dish ON public.facts (dish_id);
CREATE INDEX facts_property ON public.facts (property);

CREATE TABLE public.menu_docs (
  place_id text PRIMARY KEY REFERENCES public.places (id),
  doc      jsonb NOT NULL,
  built_at timestamptz NOT NULL
);

-- ---------------------------------------------------------------- registry, keys, attestations

CREATE TABLE public.registry_properties (
  id               text PRIMARY KEY,
  doc              jsonb NOT NULL,
  registry_version text NOT NULL,
  registry_hash    text NOT NULL,
  synced_at        timestamptz NOT NULL
);

CREATE TABLE public.published_keys (
  key_id     text PRIMARY KEY,
  doc        jsonb NOT NULL,
  updated_at timestamptz NOT NULL
);

CREATE TABLE public.vendor_bindings (
  id             text PRIMARY KEY CHECK (id ~ '^at_[0-9A-HJKMNP-TV-Z]{26}$'),
  place_id       text NOT NULL REFERENCES public.places (id),
  subject_key_id text NOT NULL,
  doc            jsonb NOT NULL,
  issued_at      timestamptz NOT NULL,
  expires_at     timestamptz,
  revoked_at     timestamptz
);
CREATE INDEX vendor_bindings_place ON public.vendor_bindings (place_id);
CREATE INDEX vendor_bindings_subject ON public.vendor_bindings (subject_key_id, place_id);

-- ---------------------------------------------------------------- feed and removals

CREATE TABLE public.changes (
  seq         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  entity_type text NOT NULL,
  entity_id   text NOT NULL,
  op          text NOT NULL,
  at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE public.removals (
  id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  entity_type text NOT NULL,
  entity_id   text NOT NULL,
  reason      text NOT NULL,
  removed_at  timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- private

CREATE TABLE private.keys (
  key_id     text PRIMARY KEY,
  public_key text NOT NULL UNIQUE,
  level      text NOT NULL CHECK (level IN ('unverified', 'member', 'trusted', 'verifier', 'root')),
  status     text NOT NULL CHECK (status IN ('active', 'revoked', 'rotated')),
  depth      integer,
  successor_key_id text,
  created_at timestamptz NOT NULL,
  registered_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE private.submissions (
  id          text PRIMARY KEY,
  raw         jsonb NOT NULL,
  key_id      text NOT NULL,
  result      jsonb NOT NULL,
  received_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE private.entity_submissions (
  id          text PRIMARY KEY CHECK (id ~ '^es_[0-9A-HJKMNP-TV-Z]{26}$'),
  raw         jsonb NOT NULL,
  key_id      text NOT NULL,
  entity_type text NOT NULL,
  entity_id   text NOT NULL,
  op          text NOT NULL,
  received_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE private.attestations (
  id             text PRIMARY KEY CHECK (id ~ '^at_[0-9A-HJKMNP-TV-Z]{26}$'),
  type           text NOT NULL,
  issuer_key_id  text NOT NULL,
  subject_key_id text NOT NULL,
  raw            jsonb NOT NULL,
  revoked_at     timestamptz,
  received_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX attestations_subject ON private.attestations (subject_key_id);

CREATE TABLE private.salt_versions (
  version    integer PRIMARY KEY,
  first_used timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE private.merge_suggestions (
  id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  entity_type text NOT NULL,
  a_id       text NOT NULL,
  b_id       text NOT NULL,
  score      double precision NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE private.worker_cursors (
  name       text PRIMARY KEY,
  seq        bigint NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- append-only

CREATE FUNCTION public.es_append_only() RETURNS trigger
  LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE' AND current_setting('eatsights.redacting', true) = 'on' THEN
    RETURN NEW;
  END IF;
  RAISE EXCEPTION 'append-only: % on %.% is not allowed', TG_OP, TG_TABLE_SCHEMA, TG_TABLE_NAME
    USING ERRCODE = 'insufficient_privilege';
END $$;

CREATE TRIGGER observations_append_only BEFORE UPDATE OR DELETE ON public.observations
  FOR EACH ROW EXECUTE FUNCTION public.es_append_only();
CREATE TRIGGER observation_ids_append_only BEFORE UPDATE OR DELETE ON public.observation_ids
  FOR EACH ROW EXECUTE FUNCTION public.es_append_only();
CREATE TRIGGER submissions_append_only BEFORE UPDATE OR DELETE ON private.submissions
  FOR EACH ROW EXECUTE FUNCTION public.es_append_only();
CREATE TRIGGER entity_submissions_append_only BEFORE UPDATE OR DELETE ON private.entity_submissions
  FOR EACH ROW EXECUTE FUNCTION public.es_append_only();
CREATE TRIGGER changes_append_only BEFORE UPDATE OR DELETE ON public.changes
  FOR EACH ROW EXECUTE FUNCTION public.es_append_only();

-- The single audited removal path: tombstone the public observation and the
-- private original, log publicly without the removed content.
CREATE FUNCTION public.redact_observation(obs_id text, reason text) RETURNS boolean
  LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, private, pg_temp AS $$
DECLARE n integer;
BEGIN
  PERFORM set_config('eatsights.redacting', 'on', true);
  UPDATE public.observations
     SET doc = jsonb_build_object('id', id, 'redacted', true, 'property', property)
   WHERE id = obs_id;
  GET DIAGNOSTICS n = ROW_COUNT;
  UPDATE private.submissions
     SET raw = jsonb_build_object('id', id, 'redacted', true)
   WHERE id = obs_id;
  PERFORM set_config('eatsights.redacting', 'off', true);
  IF n = 0 THEN
    RETURN false;
  END IF;
  INSERT INTO public.removals (entity_type, entity_id, reason) VALUES ('observation', obs_id, reason);
  INSERT INTO public.changes (entity_type, entity_id, op) VALUES ('observation', obs_id, 'redact');
  PERFORM pg_notify('eatsights_changes', '');
  RETURN true;
END $$;
REVOKE ALL ON FUNCTION public.redact_observation(text, text) FROM PUBLIC;

-- ---------------------------------------------------------------- grants

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'eatsights_read') THEN
    GRANT USAGE ON SCHEMA public TO eatsights_read;
    GRANT SELECT ON ALL TABLES IN SCHEMA public TO eatsights_read;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'eatsights_write') THEN
    GRANT USAGE ON SCHEMA public, private TO eatsights_write;
    GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA public TO eatsights_write;
    GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA private TO eatsights_write;
    GRANT UPDATE ON public.places, public.sections, public.menu_items, public.dishes,
      public.vendor_bindings, public.published_keys, public.registry_properties TO eatsights_write;
    GRANT DELETE ON public.published_keys TO eatsights_write;
    GRANT USAGE ON ALL SEQUENCES IN SCHEMA public, private TO eatsights_write;
    GRANT UPDATE ON private.keys, private.attestations TO eatsights_write;
    REVOKE UPDATE, DELETE ON public.observations, public.observation_ids, public.changes,
      private.submissions, private.entity_submissions FROM eatsights_write;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'eatsights_worker') THEN
    GRANT USAGE ON SCHEMA public, private TO eatsights_worker;
    GRANT SELECT ON ALL TABLES IN SCHEMA public TO eatsights_worker;
    GRANT INSERT, UPDATE ON public.facts, public.menu_docs TO eatsights_worker;
    GRANT INSERT ON public.changes TO eatsights_worker;
    GRANT USAGE ON ALL SEQUENCES IN SCHEMA public, private TO eatsights_worker;
    GRANT SELECT, INSERT, UPDATE ON private.worker_cursors, private.merge_suggestions TO eatsights_worker;
    REVOKE UPDATE, DELETE ON public.observations, public.observation_ids, public.changes FROM eatsights_worker;
  END IF;
END $$;
