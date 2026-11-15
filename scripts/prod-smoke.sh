#!/usr/bin/env bash
# Read-only smoke test for a deployed Eatsights. Safe against production: it sends no
# writes and needs no keys. (scripts/e2e.sh WRITES test data: never run it against prod.)
#
#   scripts/prod-smoke.sh https://eatsights.letsimpl.fyi [expected-root-public-key]
set -euo pipefail

BASE="${1:?usage: $0 <base-url> [root-public-key]}"
ROOT_PK="${2:-}"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
fails=0
ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }
check() { if eval "$2" >/dev/null 2>&1; then ok "$1"; else bad "$1"; fi; }
get() { curl -fsS --max-time 15 "$BASE$1"; }

check "healthz" "get /healthz"
check "readyz (database reachable)" "get /readyz"
check "TLS certificate valid" "curl -fsS --max-time 15 -o /dev/null $BASE/healthz"
check "openapi.json" "get /openapi.json | jq -e '.openapi and (.paths | length > 20)'"
check "openapi servers = base url" "get /openapi.json | jq -e --arg b '$BASE' '.servers[0].url == \$b'"
for f in schema.json property.schema.json properties.json; do
  check "contract file $f" "get /schema/v0.3/$f | jq -e '.'"
done
check "registry has 30 properties" "get /v1/registry/properties | jq -e '.items | length == 30'"
check "published keys include root" "get /v1/keys/published | jq -e '[.items[].level] | index(\"root\")'"
if [ -n "$ROOT_PK" ]; then
  check "root key is the expected one" \
    "get /v1/keys/published | jq -e --arg pk '$ROOT_PK' '[.items[] | select(.level == \"root\") | .public_key] == [\$pk]'"
fi
check "a verifier is published" "get /v1/keys/published | jq -e '[.items[].level] | index(\"verifier\")'"
check "change feed" "get '/v1/changes?after=0&limit=1' | jq -e '.items | type == \"array\"'"
check "places search" "get '/v1/places?limit=1' | jq -e '.items | type == \"array\"'"

curl -fsS -D "$TMP/h" -o /dev/null "$BASE/v1/registry/properties"
check "Cache-Control with stale-if-error" "grep -qi '^cache-control:.*stale-if-error' $TMP/h"
ETAG="$(grep -i '^etag:' "$TMP/h" | cut -d' ' -f2- | tr -d '\r')"
check "ETag + 304" "[ \"\$(curl -sS -o /dev/null -w '%{http_code}' -H 'If-None-Match: $ETAG' $BASE/v1/registry/properties)\" = 304 ]"
check "gzip" "curl -fsS -D - -o /dev/null -H 'Accept-Encoding: gzip' $BASE/v1/registry/properties | grep -qi '^content-encoding: gzip'"

curl -sS -D "$TMP/c" -o /dev/null -H 'Origin: https://app.example' "$BASE/v1/places?limit=1"
check "CORS for browser apps" "grep -qiE '^access-control-allow-origin: (\\*|https://app\\.example)' $TMP/c"
check "CORS never allows credentials" "! grep -qi '^access-control-allow-credentials: true' $TMP/c"

CODE="$(curl -sS -o /dev/null -D "$TMP/nf" -w '%{http_code}' "$BASE/v1/places/pl_00000000000000000000000000")"
check "404 is problem+json" "[ $CODE = 404 ] && grep -qi 'application/problem+json' $TMP/nf"
CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' --data '[{}]' "$BASE/v1/submissions")"
check "unsigned write is rejected (no data written)" "[ $CODE = 200 ] || [ $CODE = 400 ] || [ $CODE = 422 ]"
check "swagger UI off in production" "[ \"\$(curl -sS -o /dev/null -w '%{http_code}' $BASE/docs/)\" = 404 ]"

# Privacy: nothing public may carry a signature or private attestation types.
# shellcheck disable=SC2034 # used inside the eval'd checks below
ALL="$(for u in '/v1/places?limit=50' '/v1/observations?limit=50' '/v1/facts?limit=50' \
  '/v1/attestations?type=vendor_of&include_inactive=true' '/v1/changes?after=0&limit=200'; do get "$u"; done)"
check "no signatures in public responses" "! grep -q '\"signature\"' <<< \"\$ALL\""
check "no vouch/revoke/rotate in public responses" "! grep -qE '\"type\": ?\"(vouch|revoke|rotate)\"' <<< \"\$ALL\""

if MAN="$(get /v1/export 2>/dev/null)"; then
  check "export manifest is CC0" "jq -e '.license == \"CC0-1.0\"' <<< \"\$MAN\""
  URL="$(jq -r '.files[0].url' <<< "$MAN")"
  check "export files are snapshot-pinned + immutable" \
    "curl -fsS -D - -o /dev/null '$URL' | grep -qi '^cache-control:.*immutable'"
else
  printf 'skip export (no snapshot yet; the worker writes one at start-up)\n'
fi

echo
if [ "$fails" -eq 0 ]; then echo "prod-smoke: all checks passed ($BASE)"; else echo "prod-smoke: $fails check(s) failed"; exit 1; fi
