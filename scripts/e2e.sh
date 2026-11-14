#!/usr/bin/env bash
# End-to-end check of the Eatsights MVP against a real server and Postgres.
#
# Mode 1 (default): the script generates a root key, starts `eatsights serve` on $PORT
#   (default 18080) against $DATABASE_URL (required), and stops it on exit.
# Mode 2: set BASE and ROOT_PRIVATE_KEY to test an already running server whose
#   EATSIGHTS_ROOT_PUBLIC_KEY matches.
#
# Needs: curl, jq, and the eatsights binary ($EATSIGHTS_BIN, default target/debug/eatsights).
# Exits non-zero on the first failure.
set -euo pipefail

BIN="${EATSIGHTS_BIN:-target/debug/eatsights}"
PORT="${PORT:-18080}"
FAILS=0
SERVER_PID=""
TMP="$(mktemp -d)"

cleanup() {
  if [ -n "$SERVER_PID" ]; then kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; fi
  rm -rf "$TMP"
}
trap cleanup EXIT

die() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok   $*"; }
check() { # check <description> <jq expr that must be true> <json>
  if echo "$3" | jq -e "$2" >/dev/null 2>&1; then ok "$1"; else echo "$3" | head -c 2000 >&2; echo >&2; die "$1"; fi
}

[ -x "$BIN" ] || die "binary $BIN not found (cargo build -p eatsights)"
now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
sign() { "$BIN" sign --key "$1"; }
newid() { "$BIN" id "$1"; }

if [ -z "${ROOT_PRIVATE_KEY:-}" ]; then
  : "${DATABASE_URL:?DATABASE_URL is required to start a server}"
  ROOT="$("$BIN" keygen)"
  ROOT_PRIVATE_KEY="$(echo "$ROOT" | jq -r .private_key)"
  BASE="http://127.0.0.1:$PORT"
  EATSIGHTS_ROOT_PUBLIC_KEY="$(echo "$ROOT" | jq -r .public_key)" \
  EATSIGHTS_PSEUDONYM_SALT="${EATSIGHTS_PSEUDONYM_SALT:-e2e-only-salt-0123456789abcdef0123456789}" \
  EATSIGHTS_LISTEN="127.0.0.1:$PORT" EATSIGHTS_POLL_MS=200 EATSIGHTS_WORKER_INTERVAL_SECS=1 \
    "$BIN" serve $([ "${E2E_MIGRATE:-1}" = 1 ] && echo --migrate) >"$TMP/server.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 100); do curl -fsS "$BASE/readyz" >/dev/null 2>&1 && break; sleep 0.1; done
fi
BASE="${BASE:-http://127.0.0.1:8080}"
curl -fsS "$BASE/readyz" >/dev/null || { [ -f "$TMP/server.log" ] && tail -5 "$TMP/server.log" >&2; die "server not ready at $BASE"; }
ok "server ready at $BASE"


post() { curl -sS -X POST -H 'content-type: application/json' --data-binary @- "$BASE$1"; }
get() { curl -sS "$BASE$1"; }

# ---- keys
A="$("$BIN" keygen)"; B="$("$BIN" keygen)"
A_SK="$(echo "$A" | jq -r .private_key)"; A_ID="$(echo "$A" | jq -r .key_id)"
B_SK="$(echo "$B" | jq -r .private_key)"; B_ID="$(echo "$B" | jq -r .key_id)"
register() { # register <json from keygen>
  jq -n --arg pk "$(echo "$1" | jq -r .public_key)" --arg t "$(now)" \
    '{type:"key_registration",algorithm:"ed25519",public_key:$pk,created_at:$t}' \
    | sign "$(echo "$1" | jq -r .private_key)" | post /v1/keys
}
R="$(register "$A")"; check "register key A" ".key_id == \"$A_ID\" and .level == \"unverified\"" "$R"
R="$(register "$B")"; check "register key B" ".key_id == \"$B_ID\"" "$R"
R="$(register "$A")"; check "re-register is idempotent" ".key_id == \"$A_ID\"" "$R"

# key registration with a tampered signature -> 401 problem+json
BAD="$(jq -n --arg pk "$(echo "$A" | jq -r .public_key)" --arg t "$(now)" \
  '{type:"key_registration",algorithm:"ed25519",public_key:$pk,created_at:$t}' | sign "$A_SK" | jq '.created_at = "2020-01-01T00:00:00Z"')"
CODE="$(echo "$BAD" | curl -sS -o "$TMP/bad.json" -D "$TMP/bad.h" -w '%{http_code}' -X POST -H 'content-type: application/json' --data-binary @- "$BASE/v1/keys")"
[ "$CODE" = 401 ] || die "tampered key registration returned $CODE"
grep -qi '^content-type: application/problem+json' "$TMP/bad.h" || die "tampered key registration is not problem+json"
check "tampered key registration -> 401 problem+json" '.type | endswith("/problems/bad-signature")' "$(cat "$TMP/bad.json")"

# ---- root vouches A and B as members
ROOT_KEY_ID="$("$BIN" pubkey --key "$ROOT_PRIVATE_KEY" | jq -r .key_id)"
[ -n "$ROOT_KEY_ID" ] || die "no published root key"
vouch() {
  jq -n --arg id "$(newid at_)" --arg i "$ROOT_KEY_ID" --arg s "$1" --arg t "$(now)" \
    '{id:$id,type:"vouch",issuer_key_id:$i,subject_key_id:$s,level_granted:"member",issued_at:$t}' | sign "$ROOT_PRIVATE_KEY"
}
R="$( (vouch "$A_ID"; vouch "$B_ID") | jq -s . | post /v1/attestations)"
check "root vouches A and B" '[.results[].status] == ["accepted","accepted"]' "$R"

# ---- entities (signed by A)
PL="$(newid pl_)"; SC="$(newid sc_)"; DI="$(newid di_)"; MI="$(newid mi_)"
LAT=9.9312; LNG=76.2673
es() { # es <entity_type> <entity json> [op]
  jq -n --arg id "$(newid es_)" --arg et "$1" --argjson e "$2" --arg t "$(now)" --arg k "$A_ID" --arg op "${3:-create}" \
    '{id:$id,schema_version:"0.4",op:$op,entity_type:$et,entity:$e,submitted_at:$t,contributor:{key_id:$k},license:"CC0-1.0"}' | sign "$A_SK"
}
R="$( (
  es place "{\"id\":\"$PL\",\"names\":{\"en\":\"E2E Chaya Kada\",\"ml-Latn\":\"chaaya kada\"},\"category\":\"tea_stall\",\"location\":{\"lat\":$LAT,\"lng\":$LNG},\"city\":\"ernakulam\"}"
  es section "{\"id\":\"$SC\",\"place_id\":\"$PL\",\"labels\":{\"en\":\"Drinks\"},\"kind\":\"drinks\",\"sort\":1}"
  es dish "{\"id\":\"$DI\",\"labels\":{\"en\":\"Chai\"},\"kind\":\"beverage\"}"
  es menu_item "{\"id\":\"$MI\",\"place_id\":\"$PL\",\"section_id\":\"$SC\",\"dish_id\":\"$DI\",\"labels\":{\"en\":\"Chai\"}}"
) | jq -s . | post /v1/entities)"
check "create place, section, dish, menu item" '[.results[].status] == ["accepted","accepted","accepted","accepted"]' "$R"
R="$(es place "{\"id\":\"$PL\",\"names\":{\"en\":\"E2E Chaya Kada\"},\"category\":\"tea_stall\",\"location\":{\"lat\":$LAT,\"lng\":$LNG},\"city\":\"ernakulam\",\"osm\":{\"state\":\"mapped\"}}" | jq -s . | post /v1/entities)"
check "server-controlled field rejected" '.results[0].status == "rejected" and .results[0].problem.status == 422' "$R"

# ---- observations: same price from two distinct members
obs() { # obs <key_id> <secret> <amount>
  jq -n --arg id "$(newid ob_)" --arg pl "$PL" --arg mi "$MI" --arg t "$(now)" --arg k "$1" --argjson amt "$3" \
    '{id:$id,schema_version:"0.3",kind:"value",property:"price.amount",
      subject:{scope:"menu_item",place_id:$pl,menu_item_id:$mi},
      value:{amount_minor:$amt,currency:"INR",fulfilment:"dine_in"},
      observed_at:$t,submitted_at:$t,method:"in_person",source_type:"customer",license:"CC0-1.0",
      contributor:{key_id:$k}}' | sign "$2"
}
OA="$(obs "$A_ID" "$A_SK" 1500)"; OB="$(obs "$B_ID" "$B_SK" 1500)"
R="$(echo "[$OA,$OB]" | post /v1/submissions)"
check "two signed observations accepted" '[.results[].status] == ["accepted","accepted"]' "$R"
R="$(echo "[$OA]" | post /v1/submissions)"
check "resubmission is a duplicate" '.results[0].status == "duplicate"' "$R"
TAMPERED="$(obs "$A_ID" "$A_SK" 1600 | jq '.value.amount_minor = 10')"
R="$(echo "[$TAMPERED]" | post /v1/submissions)"
check "tampered observation rejected with problem" '.results[0].status == "rejected" and .results[0].problem.status == 401 and (.results[0].problem.type | endswith("bad-signature"))' "$R"

# ---- wait for the worker
FACTS=""
for _ in $(seq 1 100); do
  FACTS="$(get "/v1/facts?menu_item_id=$MI")"
  if echo "$FACTS" | jq -e '.items | length > 0' >/dev/null 2>&1; then break; fi
  sleep 0.2
done
check "fact computed" '.items[0].property == "price.amount" and .items[0].current_value.amount_minor == 1500' "$FACTS"
check "fact confirmed by two distinct members" '.items[0].status == "confirmed"' "$FACTS"

# ---- reads
R="$(get "/v1/places?cell=t9y")"; check "places by cell" "[.items[].id] | index(\"$PL\") != null" "$R"
R="$(get "/v1/places?near=$LAT,$LNG&radius=1000")"; check "places near" "[.items[].id] | index(\"$PL\") != null" "$R"
R="$(get "/v1/places?q=chaya")"; check "places by name" "[.items[].id] | index(\"$PL\") != null" "$R"
R="$(get "/v1/places/$PL")"; check "place by id" ".id == \"$PL\" and .osm.state == \"unchecked\"" "$R"
MENU=""
for _ in $(seq 1 50); do
  MENU="$(get "/v1/places/$PL/menu")"
  echo "$MENU" | jq -e '.items[0].facts | length > 0' >/dev/null 2>&1 && break
  sleep 0.2
done
check "menu document with fact" ".place.id == \"$PL\" and .items[0].item.id == \"$MI\" and .items[0].facts[0].status == \"confirmed\"" "$MENU"
R="$(get "/v1/menu-items/$MI")"; check "menu item with facts" '.facts | length == 1' "$R"
R="$(get "/v1/dishes/$DI")"; check "dish" ".dish.id == \"$DI\"" "$R"
OBS="$(get "/v1/observations?menu_item_id=$MI")"
check "observations public form" '(.items | length == 2) and all(.items[]; .contributor.class == "member" and (.contributor | has("key_id") | not))' "$OBS"
R="$(get "/v1/changes?after=0&limit=500")"; check "change feed" '(.items | length > 0) and ([.items[].entity_type] | index("observation") != null)' "$R"
R="$(get /v1/registry/properties/price.amount)"; check "registry property" '.id == "price.amount"' "$R"
R="$(get /openapi.json)"; check "openapi document" '.openapi | startswith("3.")' "$R"
R="$(get /schema/v0.3/schema.json)"; check "schema served" '."$id" | contains("/schema/v0.3/schema.json")' "$R"

# ---- privacy: no ordinary key ids, no signatures in public reads
ALL="$(for u in "/v1/places?cell=t9y" "/v1/places/$PL" "/v1/places/$PL/menu" "/v1/menu-items/$MI" "/v1/dishes/$DI" \
  "/v1/facts?place_id=$PL" "/v1/observations?place_id=$PL" "/v1/changes?after=0&limit=500" "/v1/attestations?type=vendor_of" "/v1/keys/published"; do get "$u"; done)"
if echo "$ALL" | grep -q -e "$A_ID" -e "$B_ID" -e '"signature"'; then die "public responses leak key ids or signatures"; fi
ok "no key ids or signatures in public responses"

# ---- caching
curl -sS -D "$TMP/h1" -o /dev/null "$BASE/v1/places/$PL/menu"
ETAG="$(grep -i '^etag:' "$TMP/h1" | cut -d' ' -f2- | tr -d '\r')"
[ -n "$ETAG" ] || die "no ETag"
grep -qi '^cache-control: public, max-age=' "$TMP/h1" || die "no Cache-Control"
CODE="$(curl -sS -o /dev/null -w '%{http_code}' -H "If-None-Match: $ETAG" "$BASE/v1/places/$PL/menu")"
[ "$CODE" = 304 ] || die "If-None-Match returned $CODE"
ok "ETag $ETAG -> 304"
curl -sS -D "$TMP/h2" -o "$TMP/menu.gz" -H 'Accept-Encoding: gzip' "$BASE/v1/places/$PL/menu"
grep -qi '^content-encoding: gzip' "$TMP/h2" || die "no gzip"
gunzip -c "$TMP/menu.gz" | jq -e ".place.id == \"$PL\"" >/dev/null || die "gzip body"
ok "pre-compressed gzip"
CODE="$(curl -sS -o "$TMP/nf.json" -D "$TMP/nf.h" -w '%{http_code}' "$BASE/v1/places/pl_00000000000000000000000000")"
[ "$CODE" = 404 ] && grep -qi 'application/problem+json' "$TMP/nf.h" || die "404 is not problem+json"
ok "404 problem+json"

echo "e2e: all checks passed"
