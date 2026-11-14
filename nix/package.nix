{
  lib,
  rustPlatform,
}:

let
  root = ../.;
in
rustPlatform.buildRustPackage {
  pname = "eatsights";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  # Only what the build reads: the workspace, migrations and the contract files
  # embedded with include_str!.
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../migrations
      ../schema.json
      ../property.schema.json
      ../properties.json
      ../SCHEMA.md
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [ "-p" "eatsights" ];
  # Unit tests; the Postgres integration tests skip themselves without TEST_DATABASE_URL.
  cargoTestFlags = [ "--workspace" ];

  meta = {
    description = "Eatsights: open food data served as an API (one binary: API + worker)";
    homepage = "https://github.com/tellmeY18/eatsights";
    license = lib.licenses.agpl3Plus;
    mainProgram = "eatsights";
    platforms = lib.platforms.unix;
  };
}
