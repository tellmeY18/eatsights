-- M4: trust and vendors (ADR-0006, ADR-0007). Forward-only; 0001 is unchanged.

-- Strike demotion and vouch cooldown state, kept with the key (private).
ALTER TABLE private.keys
  ADD COLUMN level_cap text CHECK (level_cap IN ('member', 'trusted', 'verifier')),
  ADD COLUMN demoted_at timestamptz,
  ADD COLUMN vouch_cooldown_until timestamptz;

CREATE INDEX attestations_issuer ON private.attestations (issuer_key_id);
CREATE INDEX attestations_type ON private.attestations (type) WHERE revoked_at IS NULL;

-- Abuse strikes: a vouch revoked by root or a verifier other than its issuer.
CREATE TABLE private.strikes (
  id             bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  key_id         text NOT NULL,
  attestation_id text NOT NULL,
  revoke_id      text NOT NULL UNIQUE,
  at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX strikes_key ON private.strikes (key_id, at);
CREATE TRIGGER strikes_append_only BEFORE UPDATE OR DELETE ON private.strikes
  FOR EACH ROW EXECUTE FUNCTION public.es_append_only();

-- The key currently allowed to use a vendor binding (follows rotation).
ALTER TABLE public.vendor_bindings ADD COLUMN holder_key_id text;
UPDATE public.vendor_bindings SET holder_key_id = subject_key_id;
ALTER TABLE public.vendor_bindings ALTER COLUMN holder_key_id SET NOT NULL;
CREATE INDEX vendor_bindings_holder ON public.vendor_bindings (holder_key_id, place_id);

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'eatsights_read') THEN
    GRANT SELECT ON ALL TABLES IN SCHEMA public TO eatsights_read;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'eatsights_write') THEN
    GRANT SELECT, INSERT ON private.strikes TO eatsights_write;
    GRANT USAGE ON ALL SEQUENCES IN SCHEMA private TO eatsights_write;
    REVOKE UPDATE, DELETE ON private.strikes FROM eatsights_write;
  END IF;
END $$;
