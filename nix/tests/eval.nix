# Evaluates (does not build) NixOS systems using the module, for both database modes.
# nix eval --impure --file nix/tests/eval.nix --json
let
  flake = builtins.getFlake (toString ../..);
  nixpkgs = flake.inputs.nixpkgs;
  mk =
    extra:
    nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        flake.nixosModules.default
        {
          boot.loader.grub.enable = false;
          fileSystems."/" = { device = "/dev/null"; fsType = "ext4"; };
          system.stateVersion = "25.11";
          services.eatsights = {
            enable = true;
            rootPublicKey = "IM7LfrFeRXiC7FdKJCIZXOdAz-HTH-_1mtU2nGI2gcY";
            environmentFile = "/run/secrets/eatsights.env";
          };
        }
        extra
      ];
    };
  local = mk { };
  external = mk {
    services.eatsights.database = {
      createLocally = false;
      url = "postgres://eatsights@db.internal/eatsights";
    };
  };
  svc = sys: sys.config.systemd.services.eatsights;
in
{
  local = {
    postgres = local.config.services.postgresql.enable;
    pgVersion = local.config.services.postgresql.package.version;
    requires = (svc local).requires;
    dbUrl = (svc local).environment.DATABASE_URL;
    exec = (svc local).serviceConfig.ExecStart;
    toplevel = local.config.system.build.toplevel.drvPath;
  };
  external = {
    postgres = external.config.services.postgresql.enable;
    requires = (svc external).requires;
    dbUrl = (svc external).environment.DATABASE_URL;
    toplevel = external.config.system.build.toplevel.drvPath;
  };
}
