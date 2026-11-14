{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.eatsights;
  db = cfg.database;
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    mkDefault
    mkPackageOption
    types
    optional
    optionalAttrs
    ;

  localUrl = "postgres://${db.user}@localhost/${db.name}?host=/run/postgresql";

  toEnv = v: if lib.isBool v then lib.boolToString v else toString v;

  environment =
    {
      EATSIGHTS_LISTEN = "${cfg.listenAddress}:${toString cfg.port}";
      EATSIGHTS_ROLES = lib.concatStringsSep "," cfg.roles;
      EATSIGHTS_BASE_URL = cfg.baseUrl;
      EATSIGHTS_ROOT_PUBLIC_KEY = cfg.rootPublicKey;
      EATSIGHTS_SWAGGER_UI = lib.boolToString cfg.swaggerUi;
      EATSIGHTS_EXPORT_DIR = cfg.exportDir;
      EATSIGHTS_DB_POOL = toString db.poolSize;
      RUST_LOG = cfg.logLevel;
    }
    // optionalAttrs db.createLocally { DATABASE_URL = localUrl; }
    // optionalAttrs (!db.createLocally && db.url != null) { DATABASE_URL = db.url; }
    // optionalAttrs (db.readUrl != null) { DATABASE_READ_URL = db.readUrl; }
    // optionalAttrs (cfg.trustedProxies != [ ]) {
      EATSIGHTS_TRUSTED_PROXIES = lib.concatStringsSep "," cfg.trustedProxies;
    }
    // optionalAttrs (cfg.osm.endpoint != null) { EATSIGHTS_OSM_ENDPOINT = cfg.osm.endpoint; }
    // optionalAttrs (cfg.workerThreads != null) { TOKIO_WORKER_THREADS = toString cfg.workerThreads; }
    // lib.mapAttrs (_: toEnv) cfg.settings;

  dbUnits = optional db.createLocally "postgresql.service";

  hardening = {
    User = cfg.user;
    Group = cfg.group;
    StateDirectory = "eatsights";
    StateDirectoryMode = "0750";
    WorkingDirectory = "/var/lib/eatsights";
    EnvironmentFile = optional (cfg.environmentFile != null) cfg.environmentFile;
    NoNewPrivileges = true;
    PrivateTmp = true;
    PrivateDevices = true;
    ProtectSystem = "strict";
    ProtectHome = true;
    ProtectKernelTunables = true;
    ProtectKernelModules = true;
    ProtectKernelLogs = true;
    ProtectControlGroups = true;
    ProtectClock = true;
    ProtectHostname = true;
    ProtectProc = "invisible";
    ProcSubset = "pid";
    RestrictAddressFamilies = [
      "AF_UNIX"
      "AF_INET"
      "AF_INET6"
    ];
    RestrictNamespaces = true;
    RestrictRealtime = true;
    RestrictSUIDSGID = true;
    LockPersonality = true;
    MemoryDenyWriteExecute = true;
    SystemCallArchitectures = "native";
    SystemCallFilter = [
      "@system-service"
      "~@privileged"
      "~@resources"
    ];
    CapabilityBoundingSet = "";
    UMask = "0027";
  };
in
{
  options.services.eatsights = {
    enable = mkEnableOption "Eatsights, the open food data API (API + worker in one binary)";

    package = mkPackageOption pkgs "eatsights" { };

    user = mkOption {
      type = types.str;
      default = "eatsights";
      description = "System user the service runs as. With the local database it must equal `database.user` (peer authentication).";
    };

    group = mkOption {
      type = types.str;
      default = "eatsights";
      description = "System group the service runs as.";
    };

    listenAddress = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = "Address to bind. Keep the loopback default behind a reverse proxy.";
    };

    port = mkOption {
      type = types.port;
      default = 8080;
      description = "HTTP port.";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open `port` in the firewall.";
    };

    roles = mkOption {
      type = types.listOf (
        types.enum [
          "api"
          "worker"
        ]
      );
      default = [
        "api"
        "worker"
      ];
      description = "Roles this process runs (`serve --roles`).";
    };

    baseUrl = mkOption {
      type = types.str;
      default = "https://eatsights.letsimpl.fyi";
      example = "https://staging.eatsights.letsimpl.fyi";
      description = "Public base URL (OpenAPI `servers`, problem type links).";
    };

    rootPublicKey = mkOption {
      type = types.strMatching "^[A-Za-z0-9_-]{43}$";
      example = "IM7LfrFeRXiC7FdKJCIZXOdAz-HTH-_1mtU2nGI2gcY";
      description = ''
        Root Ed25519 public key (43 characters base64url, from `eatsights keygen`).
        Public by design; the root private key never goes on the server.
      '';
    };

    environmentFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      example = "/run/secrets/eatsights.env";
      description = ''
        File with secret environment variables, loaded by systemd and kept out of the Nix store.
        Must set `EATSIGHTS_PSEUDONYM_SALT` (at least 32 bytes). With an external database it
        may also set `DATABASE_URL` / `DATABASE_READ_URL` instead of `database.url`.
        Works with agenix and sops-nix.
      '';
    };

    swaggerUi = mkOption {
      type = types.bool;
      default = false;
      description = "Serve Swagger UI at `/docs` (ADR-0005). Off by default.";
    };

    exportDir = mkOption {
      type = types.str;
      default = "/var/lib/eatsights/exports";
      description = "Where the worker writes NDJSON export snapshots.";
    };

    trustedProxies = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "127.0.0.1/32" "::1/128" ];
      description = "CIDRs whose `X-Forwarded-For` header is trusted for per-IP rate limits.";
    };

    osm.endpoint = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "https://overpass-api.de/api/interpreter";
      description = "Overpass API URL. The OSM checker only runs when this is set (ADR-0006).";
    };

    workerThreads = mkOption {
      type = types.nullOr types.ints.positive;
      default = null;
      description = "Tokio worker threads (`TOKIO_WORKER_THREADS`). Default: one per CPU.";
    };

    logLevel = mkOption {
      type = types.str;
      default = "info";
      description = "`RUST_LOG` filter. Logs are JSON on stdout (journald).";
    };

    settings = mkOption {
      type = types.attrsOf (
        types.oneOf [
          types.str
          types.int
          types.bool
          types.float
        ]
      );
      default = { };
      example = {
        EATSIGHTS_CACHE_MB = 128;
        EATSIGHTS_TRUST_MAX_DEPTH = 4;
      };
      description = "Any other environment variables (see `deploy/configmap.yaml`). Not for secrets: these land in the Nix store.";
    };

    runMigrations = mkOption {
      type = types.bool;
      default = true;
      description = "Run `eatsights migrate` in a oneshot unit (`eatsights-migrate.service`) before the server starts.";
    };

    database = {
      createLocally = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Run the inbuilt PostgreSQL on this machine (`services.postgresql`) and connect over
          its Unix socket with peer authentication. Set to false to use an external server via
          `database.url` or `DATABASE_URL` in `environmentFile`.
        '';
      };

      postgresqlPackage = mkOption {
        type = types.package;
        default = pkgs.postgresql_18;
        defaultText = lib.literalExpression "pkgs.postgresql_18";
        description = "PostgreSQL package for the inbuilt server. Needs `pg_trgm` and `unaccent` (in contrib).";
      };

      name = mkOption {
        type = types.str;
        default = "eatsights";
        description = "Database name (inbuilt server).";
      };

      user = mkOption {
        type = types.str;
        default = "eatsights";
        description = "Database user (inbuilt server). Must equal `name` for ownership.";
      };

      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "postgres://eatsights@db.internal:5432/eatsights?sslmode=require";
        description = "External database URL without a password. Put URLs with passwords in `environmentFile`.";
      };

      readUrl = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional read-only role URL (`DATABASE_READ_URL`).";
      };

      poolSize = mkOption {
        type = types.ints.positive;
        default = 8;
        description = "Connection pool size per process.";
      };
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = db.createLocally || db.url != null || cfg.environmentFile != null;
        message = "services.eatsights: with database.createLocally = false, set database.url or DATABASE_URL in environmentFile.";
      }
      {
        assertion = !db.createLocally || (db.user == db.name && cfg.user == db.user);
        message = "services.eatsights: with the inbuilt database, user, database.user and database.name must be equal (peer auth + ownership).";
      }
      {
        assertion = cfg.environmentFile != null || cfg.settings ? EATSIGHTS_PSEUDONYM_SALT;
        message = "services.eatsights: EATSIGHTS_PSEUDONYM_SALT is required; provide it via environmentFile.";
      }
    ];

    users.users = mkIf (cfg.user == "eatsights") {
      eatsights = {
        isSystemUser = true;
        group = cfg.group;
        home = "/var/lib/eatsights";
      };
    };
    users.groups = mkIf (cfg.group == "eatsights") { eatsights = { }; };

    services.postgresql = mkIf db.createLocally {
      enable = true;
      # Normal priority: NixOS picks a version from stateVersion with mkDefault.
      # Change it through services.eatsights.database.postgresqlPackage.
      package = db.postgresqlPackage;
      ensureDatabases = [ db.name ];
      ensureUsers = [
        {
          name = db.user;
          ensureDBOwnership = true;
          # The first migration creates the eatsights_read/write/worker group roles.
          ensureClauses.createrole = true;
        }
      ];
      settings.shared_buffers = mkDefault "128MB";
    };

    systemd.services.eatsights-migrate = mkIf cfg.runMigrations {
      description = "Eatsights database migrations";
      after = [ "network-online.target" ] ++ dbUnits;
      wants = [ "network-online.target" ];
      requires = dbUnits;
      before = [ "eatsights.service" ];
      requiredBy = [ "eatsights.service" ];
      inherit environment;
      serviceConfig = hardening // {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${lib.getExe cfg.package} migrate";
      };
    };

    systemd.services.eatsights = {
      description = "Eatsights API and worker";
      wantedBy = [ "multi-user.target" ];
      after =
        [ "network-online.target" ]
        ++ dbUnits
        ++ optional cfg.runMigrations "eatsights-migrate.service";
      wants = [ "network-online.target" ];
      requires = dbUnits;
      inherit environment;
      serviceConfig = hardening // {
        ExecStart = "${lib.getExe cfg.package} serve --roles ${lib.concatStringsSep "," cfg.roles}";
        Restart = "on-failure";
        RestartSec = 2;
        # Graceful shutdown: in-flight requests finish on SIGTERM.
        KillSignal = "SIGTERM";
        TimeoutStopSec = 30;
      };
    };

    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [ cfg.port ];
  };
}
