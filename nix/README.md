# Nix: package and NixOS module

Eatsights ships its own flake outputs, so any NixOS machine can deploy it with a flake input. Nothing is in upstream nixpkgs yet.

| Output | What |
|---|---|
| `packages.<system>.default` | The `eatsights` binary, built with the pinned toolchain (`rust-toolchain.toml`) |
| `nixosModules.default` | `services.eatsights` (this directory's `module.nix`) |
| `overlays.default` | Adds `pkgs.eatsights` |
| `checks.<linux>.vm` | NixOS VM test: inbuilt Postgres, migrations, API up (`tests/vm.nix`) |

## Staging host example

```nix
# flake.nix of the machine
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    eatsights.url = "github:tellmeY18/eatsights";
  };

  outputs = { nixpkgs, eatsights, ... }: {
    nixosConfigurations.staging = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./hardware-configuration.nix
        eatsights.nixosModules.default
        {
          services.eatsights = {
            enable = true;
            baseUrl = "https://staging.eatsights.letsimpl.fyi";
            rootPublicKey = "<43-char public key from `eatsights keygen`>";
            environmentFile = "/run/secrets/eatsights.env"; # agenix / sops-nix
            trustedProxies = [ "127.0.0.1/32" "::1/128" ];
            swaggerUi = true; # staging only
            # database.createLocally = true;  # default: inbuilt PostgreSQL 18
          };

          services.nginx = {
            enable = true;
            recommendedProxySettings = true;
            virtualHosts."staging.eatsights.letsimpl.fyi" = {
              enableACME = true;
              forceSSL = true;
              locations."/".proxyPass = "http://127.0.0.1:8080";
            };
          };
        }
      ];
    };
  };
}
```

`/run/secrets/eatsights.env` (never in the Nix store, never in git):

```sh
EATSIGHTS_PSEUDONYM_SALT=<at least 32 random bytes, e.g. `head -c 32 /dev/urandom | base64`>
# External database only, if the URL carries a password:
# DATABASE_URL=postgres://eatsights_app:...@db.internal:5432/eatsights?sslmode=require
```

## Inbuilt or external PostgreSQL

`services.eatsights.database.createLocally` (default `true`):

- **`true`**: enables `services.postgresql` with PostgreSQL 18 (`database.postgresqlPackage`), creates database and user `eatsights` (with `CREATEROLE`, needed by the first migration to create the `eatsights_read/write/worker` group roles), sets `shared_buffers = 128MB`, and connects over `/run/postgresql` with peer authentication. No password exists anywhere.
- **`false`**: no local server. Set `database.url` (no password) or `DATABASE_URL` in `environmentFile`. The server needs PostgreSQL 18 with `pg_trgm` and `unaccent`; if the app user cannot create roles, create the three group roles beforehand (see `deploy/README.md`).

## Units

- `eatsights-migrate.service`: oneshot `eatsights migrate`, runs before the server (`runMigrations`, default on). Migrations are idempotent and forward-only.
- `eatsights.service`: `eatsights serve --roles api,worker`, restarts on failure, SIGTERM drains in-flight requests, sandboxed (`ProtectSystem=strict`, no capabilities, syscall filter). State and exports live in `/var/lib/eatsights`.

## Options

Main ones: `enable`, `package`, `listenAddress` (127.0.0.1), `port` (8080), `openFirewall`, `roles`, `baseUrl`, `rootPublicKey`, `environmentFile`, `swaggerUi`, `exportDir`, `trustedProxies`, `osm.endpoint` (OSM checker is off unless set), `workerThreads`, `logLevel`, `runMigrations`, `database.{createLocally, postgresqlPackage, name, user, url, readUrl, poolSize}`. Anything else from `deploy/configmap.yaml` goes in `settings` (non-secret only), e.g. `settings.EATSIGHTS_CACHE_MB = 128;`.

## Checks

```sh
nix build .#eatsights                                   # package (+ unit tests)
nix eval --impure --file nix/tests/eval.nix --json      # module evaluates in both DB modes
nix build .#checks.x86_64-linux.vm                      # VM test (Linux / Linux builder)
```
