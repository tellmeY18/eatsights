# syntax=docker/dockerfile:1
# Static musl binary on distroless. Target image <= 30 MB.
FROM rust:1.90-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p eatsights \
 && cp target/release/eatsights /eatsights \
 && strip /eatsights

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /eatsights /eatsights
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/eatsights"]
CMD ["serve"]
