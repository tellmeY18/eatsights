# syntax=docker/dockerfile:1
# Eatsights: one static musl binary on distroless. Budget: image <= 30 MB (AGENTS.md 8.1).
# Built for linux/amd64 and linux/arm64 by .github/workflows/image.yml.
FROM rust:1.90-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
# The toolchain is pinned by rust-toolchain.toml (same version as the base image).
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p eatsights \
 && cp target/release/eatsights /eatsights \
 && strip /eatsights \
 && mkdir -p /out/var/lib/eatsights/exports

FROM gcr.io/distroless/static-debian12:nonroot
ARG VERSION=dev
ARG REVISION=unknown
LABEL org.opencontainers.image.title="eatsights" \
      org.opencontainers.image.description="Open food data API (API + worker in one binary)" \
      org.opencontainers.image.source="https://github.com/tellmeY18/eatsights" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"
COPY --from=build /eatsights /eatsights
# Export snapshots; mount a volume here in Kubernetes (deploy/deployment.yaml).
COPY --from=build --chown=65532:65532 /out/var/lib/eatsights /var/lib/eatsights
ENV EATSIGHTS_LISTEN=0.0.0.0:8080 \
    EATSIGHTS_EXPORT_DIR=/var/lib/eatsights/exports
# Numeric UID so Kubernetes `runAsNonRoot` can verify it (distroless "nonroot" = 65532).
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/eatsights"]
CMD ["serve"]
