# M0: foundations — milestone note

Date: 2026-11-14. Scope per ADR-0004 (MVP built in one pass).

## Done

- Schema `$id`s set to `https://eatsights.letsimpl.fyi/schema/v0.3/...`; base address is `eatsights_core::BASE_URL` (override `EATSIGHTS_BASE_URL`).
- Workspace, toolchain pin (`rust-toolchain.toml`, 1.90), Nix flake dev shell with `pg-start`/`pg-stop`, `compose.yaml` for non-Nix users.
- CI (`.github/workflows/ci.yml`): fmt, clippy `-D warnings`, tests against Postgres 18, `cargo deny` (`deny.toml`), `cargo audit`, JSON Schema metaschema check, OpenAPI diff + Spectral lint (`.spectral.yaml`).
- `Dockerfile` (static musl, distroless) and `deploy/` manifests with Postgres requirements and backup/restore procedure.
- ADR-0001 (database), ADR-0002 (framework), ADR-0004 (MVP scope), proposal 0001 (v0.4 entity submissions, provisional).

## Open for the maintainer

- Approve proposal 0001 (key_id, signed bytes, entity writes).
- Confirm ADR-0002 spike items (OpenAPI version, `Union` discriminator output, budgets).
- Postgres hosting choice (in-cluster operator or external).
