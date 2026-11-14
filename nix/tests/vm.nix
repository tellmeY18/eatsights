# NixOS VM test: inbuilt Postgres, migrations, API and worker up, public reads work.
# Run on Linux: nix build .#checks.x86_64-linux.vm
{ self, pkgs }:

pkgs.testers.runNixOSTest {
  name = "eatsights";

  nodes.machine =
    { ... }:
    {
      imports = [ self.nixosModules.default ];
      virtualisation.memorySize = 1024;

      # Test-only secret. Real deployments use agenix / sops-nix / a root-only file.
      environment.etc."eatsights.env".text = ''
        EATSIGHTS_PSEUDONYM_SALT=vm-test-only-salt-0123456789abcdef0123456789
      '';

      services.eatsights = {
        enable = true;
        rootPublicKey = "IM7LfrFeRXiC7FdKJCIZXOdAz-HTH-_1mtU2nGI2gcY";
        environmentFile = "/etc/eatsights.env";
        swaggerUi = true;
        baseUrl = "http://127.0.0.1:8080";
      };
    };

  testScript = ''
    machine.wait_for_unit("postgresql.service")
    machine.wait_for_unit("eatsights-migrate.service")
    machine.wait_for_unit("eatsights.service")
    machine.wait_for_open_port(8080)

    machine.succeed("curl -sf http://127.0.0.1:8080/healthz")
    machine.wait_until_succeeds("curl -sf http://127.0.0.1:8080/readyz", timeout=30)
    machine.succeed("curl -sf http://127.0.0.1:8080/openapi.json | grep -q '\"openapi\"'")
    machine.succeed("curl -sf http://127.0.0.1:8080/v1/registry/properties | grep -q price.amount")
    machine.succeed("curl -sf http://127.0.0.1:8080/v1/keys/published | grep -q root")
    machine.succeed("curl -sfL http://127.0.0.1:8080/docs/ | grep -q 'Swagger UI'")

    # Append-only holds on the deployed database too.
    machine.fail("sudo -u eatsights psql -d eatsights -c 'DELETE FROM private.submissions'")

    # Graceful restart keeps working (migrations are idempotent).
    machine.succeed("systemctl restart eatsights-migrate.service eatsights.service")
    machine.wait_until_succeeds("curl -sf http://127.0.0.1:8080/readyz", timeout=30)
  '';
}
