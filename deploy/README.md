# Deploying Eatsights on Kubernetes

Plain manifests, no Helm, no operator chosen (the maintainer decides; AGENTS.md section 12).

```sh
kubectl apply -f namespace.yaml -f configmap.yaml
# copy secret.template.yaml, fill in, apply out of band
kubectl apply -f migrate-job.yaml && kubectl -n eatsights wait --for=condition=complete job/eatsights-migrate
kubectl apply -f deployment.yaml -f service.yaml -f ingress.yaml
```

Migrations run only as the Job, never on replica start. Generate the root key offline with `eatsights keygen`; only the public key goes in the Secret.

## Postgres requirements

- PostgreSQL 18, one instance, in-cluster or external.
- Extensions: `pg_trgm`, `unaccent` (contrib). `postgis` optional; the MVP uses geohash + btree (see ADR-0001).
- Starting parameters for a small instance: `shared_buffers=128MB`, `max_connections` ≥ 8 × replicas + headroom, `wal_level=replica`, `archive_mode=on` if using WAL archiving.
- Login users: one for `DATABASE_URL` granted the `eatsights_write` and `eatsights_worker` roles; one for `DATABASE_READ_URL` granted `eatsights_read` only. Migrations need a user that can create schemas, roles and extensions.

## Backups

Backups include the `private` schema; keep them encrypted and access-restricted. Public exports never include it.

- Simple: nightly `pg_dump -Fc` uploaded to object storage, retained 30 days.
- Better: base backup plus continuous WAL archiving to object storage (e.g. `pgBackRest` or `wal-g`) for point-in-time recovery.

## Restore drill (at least quarterly)

1. Start a fresh PG18 with the same extensions.
2. Restore (`pg_restore -d eatsights dump.pgc`, or base backup + WAL replay to a target time).
3. Point a staging deployment at it; check `/readyz`, `GET /v1/changes` latest `seq`, and spot-check a place.
4. Record duration and data loss window in `docs/`.

During an outage, the CDN keeps serving reads via `stale-if-error`.

## Exports

The worker writes public snapshots (gzip NDJSON + `manifest.json`, latest 7 kept) to `EATSIGHTS_EXPORT_DIR` (`/var/lib/eatsights/exports`) every `EATSIGHTS_EXPORT_INTERVAL_HOURS` (24); the API serves them at `/v1/export` from the same directory. Because the root filesystem is read-only, the directory is a volume:

- `emptyDir` (default in `deployment.yaml`): simplest; a restarted pod writes a fresh snapshot at start-up. Fine for one replica running `api,worker`.
- `PersistentVolumeClaim`: keeps snapshots across restarts. Required if the API and worker run as separate Deployments (`--roles`), since both must see the same directory (ReadWriteMany, or co-locate them).

Size it at roughly 2× one snapshot × 7. Exports contain only `public` data and are safe to mirror to a CDN or object storage.
