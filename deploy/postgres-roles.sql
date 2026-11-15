-- Eatsights database and logins. Run once as a PostgreSQL superuser (or the managed
-- service's admin user) BEFORE the first `eatsights migrate`. Replace every CHANGE_ME.
--
--   psql "postgres://admin@host/postgres" -v ON_ERROR_STOP=1 -f deploy/postgres-roles.sql
--
-- Three logins, least privilege (AGENTS.md 5.3):
--   eatsights_owner  owns the schema; used ONLY by the migrate Job (Secret eatsights-migrate)
--   eatsights_app    API writes + worker (Secret eatsights, DATABASE_URL)
--   eatsights_ro     API read path, public schema only (Secret eatsights, DATABASE_READ_URL)

-- 1. Group roles. The first migration also creates them when the owner may create roles;
--    creating them here keeps the owner free of CREATEROLE.
DO $$
DECLARE r text;
BEGIN
  FOREACH r IN ARRAY ARRAY['eatsights_read', 'eatsights_write', 'eatsights_worker'] LOOP
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
      EXECUTE format('CREATE ROLE %I NOLOGIN', r);
    END IF;
  END LOOP;
END $$;

-- 2. Logins.
CREATE ROLE eatsights_owner LOGIN PASSWORD 'CHANGE_ME_owner';
CREATE ROLE eatsights_app   LOGIN PASSWORD 'CHANGE_ME_app'
  IN ROLE eatsights_write, eatsights_worker CONNECTION LIMIT 40;
CREATE ROLE eatsights_ro    LOGIN PASSWORD 'CHANGE_ME_ro'
  IN ROLE eatsights_read CONNECTION LIMIT 40;
-- The owner grants table privileges to the group roles inside migrations.
GRANT eatsights_read, eatsights_write, eatsights_worker TO eatsights_owner WITH ADMIN OPTION;

-- 3. Database owned by the schema owner.
CREATE DATABASE eatsights OWNER eatsights_owner ENCODING 'UTF8' TEMPLATE template0;

\connect eatsights
-- pg_trgm and unaccent are trusted extensions: the database owner may create them, but
-- creating them here works on every managed service.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS unaccent;
REVOKE ALL ON DATABASE eatsights FROM PUBLIC;
GRANT CONNECT ON DATABASE eatsights TO eatsights_app, eatsights_ro;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
ALTER SCHEMA public OWNER TO eatsights_owner;
