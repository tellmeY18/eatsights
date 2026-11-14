{
  description = "Eatsights dev shell: pinned Rust, Postgres + PostGIS, schema and API tooling";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Single source of truth for the toolchain version: rust-toolchain.toml.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        postgres = pkgs.postgresql_18.withPackages (ps: [ ps.postgis ]);

        # Throwaway local Postgres under ./.dev/pg (git-ignored). Same extensions as production.
        pgStart = pkgs.writeShellScriptBin "pg-start" ''
          set -euo pipefail
          root="''${EATSIGHTS_PGDIR:-$PWD/.dev/pg}"
          if [ ! -d "$root/data" ]; then
            mkdir -p "$root"
            initdb -D "$root/data" -U eatsights --auth=trust >/dev/null
          fi
          pg_ctl -D "$root/data" -l "$root/log" \
            -o "-k $root -c listen_addresses=127.0.0.1 -p ''${PGPORT:-5432} -c shared_buffers=128MB" \
            -w start
          createdb -h 127.0.0.1 -p ''${PGPORT:-5432} -U eatsights eatsights 2>/dev/null || true
          echo "postgres://eatsights@127.0.0.1:''${PGPORT:-5432}/eatsights"
        '';
        pgStop = pkgs.writeShellScriptBin "pg-stop" ''
          root="''${EATSIGHTS_PGDIR:-$PWD/.dev/pg}"
          pg_ctl -D "$root/data" -m fast stop
        '';
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.pkg-config
            pkgs.openssl
            pkgs.cargo-deny
            pkgs.cargo-audit
            pkgs.cargo-nextest
            pkgs.cargo-watch
            pkgs.sqlx-cli

            postgres
            pgStart
            pgStop

            # Schema and API contract checks
            pkgs.check-jsonschema
            pkgs.jq
            pkgs.nodejs_22 # npx @stoplight/spectral-cli lint openapi.json

            # Load tests (section 8.1)
            pkgs.oha
            pkgs.hyperfine
          ];

          shellHook = ''
            export DATABASE_URL="''${DATABASE_URL:-postgres://eatsights@127.0.0.1:5432/eatsights}"
            export RUST_BACKTRACE=1
            echo "eatsights dev shell: $(rustc --version)"
            echo "  pg-start / pg-stop   local Postgres + PostGIS"
          '';
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
