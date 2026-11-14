-- M5: OSM checker, duplicate detector, merges and signed admin actions (forward-only).
-- See docs/m5-osm-dedupe.md.

-- ---------------------------------------------------------------- OSM checker

-- Places due for an OSM check: unchecked, or unmapped and checked long ago.
CREATE INDEX places_osm_state ON public.places ((doc #>> '{osm,state}'), (doc #>> '{osm,checked_at}'))
  WHERE record_state = 'active';

-- ---------------------------------------------------------------- duplicates

ALTER TABLE private.merge_suggestions
  ADD COLUMN distance_m double precision,
  ADD COLUMN status     text NOT NULL DEFAULT 'open'
    CHECK (status IN ('open', 'confirmed', 'dismissed')),
  ADD COLUMN decided_at timestamptz,
  ADD COLUMN decided_by text,
  ADD CONSTRAINT merge_suggestions_order CHECK (a_id < b_id);
CREATE UNIQUE INDEX merge_suggestions_pair ON private.merge_suggestions (entity_type, a_id, b_id);
CREATE INDEX merge_suggestions_open ON private.merge_suggestions (status, id);

CREATE INDEX places_merged ON public.places (merged_into) WHERE merged_into IS NOT NULL;

-- ---------------------------------------------------------------- admin actions

-- Signed admin objects (root or verifier), kept in full. The unique id is the replay guard.
CREATE TABLE private.admin_actions (
  id            text PRIMARY KEY CHECK (id ~ '^es_[0-9A-HJKMNP-TV-Z]{26}$'),
  action        text NOT NULL,
  issuer_key_id text NOT NULL,
  raw           jsonb NOT NULL,
  result        jsonb NOT NULL,
  issued_at     timestamptz NOT NULL,
  received_at   timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER admin_actions_append_only BEFORE UPDATE OR DELETE ON private.admin_actions
  FOR EACH ROW EXECUTE FUNCTION public.es_append_only();

-- ---------------------------------------------------------------- grants

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'eatsights_read') THEN
    GRANT SELECT ON ALL TABLES IN SCHEMA public TO eatsights_read;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'eatsights_write') THEN
    GRANT SELECT, INSERT ON private.admin_actions TO eatsights_write;
    GRANT SELECT, UPDATE ON private.merge_suggestions TO eatsights_write;
    GRANT DELETE ON public.facts TO eatsights_write;
    GRANT EXECUTE ON FUNCTION public.redact_observation(text, text) TO eatsights_write;
    REVOKE UPDATE, DELETE ON private.admin_actions FROM eatsights_write;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'eatsights_worker') THEN
    GRANT UPDATE ON public.places TO eatsights_worker;
    GRANT DELETE ON public.facts TO eatsights_worker;
  END IF;
END $$;
