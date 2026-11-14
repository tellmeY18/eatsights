# Eatsights

An open food dataset served as an API: menus, prices, availability, contents, quality and wait times. Data is CC0-1.0. API only, no UI.

- API: `https://eatsights.letsimpl.fyi/v1/...` · OpenAPI: `/openapi.json` · Schemas: `/schema/v0.3/...`
- Contract: `schema.json`, `property.schema.json`, `properties.json` (explained in `SCHEMA.md`)
- Rules: `GOVERNANCE.md`, `CONTRIBUTING.md`, `AGENTS.md`

## Run locally

```sh
nix develop            # Rust, Postgres 18, tooling
pg-start               # local Postgres on 127.0.0.1:5432, data in .dev/pg
cargo run -p eatsights -- keygen          # keep the private key; export the public one:
export EATSIGHTS_ROOT_PUBLIC_KEY=<public key>
export EATSIGHTS_PSEUDONYM_SALT=$(head -c 32 /dev/urandom | base64)
cargo run -p eatsights -- migrate
cargo run -p eatsights -- serve           # http://localhost:8080
curl localhost:8080/healthz
```

Without Nix: `docker compose up -d db`, or the whole stack with `docker compose --profile app up --build`.

## Layout

`crates/` (core, db, api, worker, binary) · `migrations/` · `deploy/` (Kubernetes) · `docs/adr/` · `docs/proposals/`

## Licence

- **Code** (everything in this repository except the data and schema files below): [GNU AGPL-3.0-or-later](LICENSE). If you run a modified version as a network service, you must offer its source to its users.
- **Data** served by the API, exports, and the contract files `schema.json`, `property.schema.json`, `properties.json`: [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) (see `GOVERNANCE.md`).
