#!/usr/bin/env bash
# End-to-end check of the Eatsights MVP against a real server and Postgres.
#
# Mode 1 (default): the script generates a root key, starts `eatsights serve` on $PORT
#   (default 18080) against $DATABASE_URL (required), and stops it on exit.
# Export checks run when EXPORT_DIR is set (mode 1 sets it to a temp dir that the server
#   serves); mode 2 needs DATABASE_URL and the server's EATSIGHTS_EXPORT_DIR as EXPORT_DIR.
# Mode 2: set BASE and ROOT_PRIVATE_KEY to test an already running server whose
#   EATSIGHTS_ROOT_PUBLIC_KEY matches.
#
# Needs: curl, jq, and the eatsights binary ($EATSIGHTS_BIN, default target/debug/eatsights).
# In mode 1 the server runs with EATSIGHTS_TRUST_MAX_DEPTH=2 (E2E_TRUST_MAX_DEPTH) so the
# depth limit is reachable; in mode 2 set E2E_TRUST_MAX_DEPTH to the server value to test it.
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
  E2E_TRUST_MAX_DEPTH="${E2E_TRUST_MAX_DEPTH:-2}"
  EXPORT_DIR="$TMP/exports"
  EATSIGHTS_ROOT_PUBLIC_KEY="$(echo "$ROOT" | jq -r .public_key)" \
  EATSIGHTS_PSEUDONYM_SALT="${EATSIGHTS_PSEUDONYM_SALT:-e2e-only-salt-0123456789abcdef0123456789}" \
  EATSIGHTS_TRUST_MAX_DEPTH="$E2E_TRUST_MAX_DEPTH" \
  EATSIGHTS_EXPORT_DIR="$EXPORT_DIR" \
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

# ---- trust and vendors (M4)
wait_for() { # wait_for <description> <jq expr> <url>
  local body=""
  for _ in $(seq 1 50); do
    body="$(get "$3")"
    echo "$body" | jq -e "$2" >/dev/null 2>&1 && break
    sleep 0.2
  done
  check "$1" "$2" "$body"
}
attest() { # attest <issuer secret> <json without signature>
  echo "$2" | sign "$1" | jq -s . | post /v1/attestations
}
mkatt() { # mkatt <type> <issuer id> <subject id> [extra json]
  local x="${4:-}"; [ -n "$x" ] || x="{}"
  jq -n --arg id "$(newid at_)" --arg ty "$1" --arg i "$2" --arg s "$3" --arg t "$(now)" --argjson x "$x" \
    '{id:$id,type:$ty,issuer_key_id:$i,subject_key_id:$s,issued_at:$t} + $x'
}
V="$("$BIN" keygen)"; K="$("$BIN" keygen)"; K2="$("$BIN" keygen)"; T="$("$BIN" keygen)"; M="$("$BIN" keygen)"; X="$("$BIN" keygen)"
for k in "$V" "$K" "$K2" "$T" "$M" "$X"; do
  R="$(register "$k")"; check "register trust key" '.level == "unverified"' "$R"
done
id_of() { echo "$1" | jq -r .key_id; }
sk_of() { echo "$1" | jq -r .private_key; }
V_ID="$(id_of "$V")"; K_ID="$(id_of "$K")"; K2_ID="$(id_of "$K2")"; T_ID="$(id_of "$T")"; M_ID="$(id_of "$M")"; X_ID="$(id_of "$X")"

R="$(attest "$ROOT_PRIVATE_KEY" "$(mkatt vouch "$ROOT_KEY_ID" "$V_ID" '{"level_granted":"verifier"}')")"
check "root grants verifier V" '.results[0].status == "accepted"' "$R"
wait_for "V is published" "[.items[].key_id] | index(\"$V_ID\") != null" /v1/keys/published

BIND="$(mkatt vendor_of "$V_ID" "$K_ID" "{\"place_id\":\"$PL\"}")"
BIND_ID="$(echo "$BIND" | jq -r .id)"
R="$(attest "$(sk_of "$V")" "$BIND")"; check "V binds vendor key K to the place" '.results[0].status == "accepted"' "$R"
wait_for "vendor binding listed with default expiry" "[.items[] | select(.id == \"$BIND_ID\" and .active and (.expires_at | type == \"string\"))] | length == 1" "/v1/attestations?type=vendor_of&place_id=$PL"

vobs() { # vobs <key json>
  jq -n --arg id "$(newid ob_)" --arg pl "$PL" --arg mi "$MI" --arg t "$(now)" --arg k "$(id_of "$1")" \
    '{id:$id,schema_version:"0.3",kind:"value",property:"price.amount",
      subject:{scope:"menu_item",place_id:$pl,menu_item_id:$mi},
      value:{amount_minor:1400,currency:"INR",fulfilment:"dine_in"},
      observed_at:$t,submitted_at:$t,method:"in_person",source_type:"vendor",license:"CC0-1.0",
      contributor:{key_id:$k}}' | sign "$(sk_of "$1")"
}
VO="$(vobs "$K")"; VO_ID="$(echo "$VO" | jq -r .id)"
R="$(echo "[$VO]" | post /v1/submissions)"; check "vendor observation accepted" '.results[0].status == "accepted"' "$R"
wait_for "vendor observation has class vendor" "[.items[] | select(.id == \"$VO_ID\") | .contributor.class] == [\"vendor\"]" "/v1/observations?menu_item_id=$MI&limit=500"

R="$(attest "$(sk_of "$K")" "$(mkatt rotate "$K_ID" "$K2_ID")")"; check "rotate K to K2" '.results[0].status == "accepted"' "$R"
R="$(echo "[$(vobs "$K")]" | post /v1/submissions)"; check "rotated key K rejected" '.results[0].status == "rejected" and .results[0].problem.status == 403' "$R"
R="$(echo "[$(vobs "$K2")]" | post /v1/submissions)"; check "successor K2 keeps the vendor binding" '.results[0].status == "accepted"' "$R"

R="$(attest "$(sk_of "$V")" "$(mkatt revoke "$V_ID" "$K_ID" "{\"revokes\":\"$BIND_ID\"}")")"
check "V revokes the binding" '.results[0].status == "accepted"' "$R"
R="$(echo "[$(vobs "$K2")]" | post /v1/submissions)"
check "vendor submissions rejected after revoke" '.results[0].status == "rejected" and (.results[0].problem.type | endswith("not-vendor"))' "$R"
wait_for "revoked binding hidden by default" "[.items[].id] | index(\"$BIND_ID\") == null" "/v1/attestations?type=vendor_of&place_id=$PL"
wait_for "revoked binding listed with include_inactive" "[.items[] | select(.id == \"$BIND_ID\") | .active] == [false]" "/v1/attestations?type=vendor_of&place_id=$PL&include_inactive=true"

VT="$(mkatt vouch "$V_ID" "$T_ID" '{"level_granted":"trusted"}')"
VX="$(mkatt vouch "$V_ID" "$X_ID" '{"level_granted":"trusted"}')"
R="$( (echo "$VT" | sign "$(sk_of "$V")"; echo "$VX" | sign "$(sk_of "$V")") | jq -s . | post /v1/attestations)"
check "V vouches T and X as trusted (depth 2)" '[.results[].status] == ["accepted","accepted"]' "$R"
if [ -n "${E2E_TRUST_MAX_DEPTH:-}" ] && [ "$E2E_TRUST_MAX_DEPTH" -le 2 ]; then
  R="$(attest "$(sk_of "$T")" "$(mkatt vouch "$T_ID" "$M_ID" '{"level_granted":"member"}')")"
  check "vouch beyond max depth rejected" '.results[0].status == "rejected" and (.results[0].problem.type | endswith("depth-exceeded"))' "$R"
fi

R="$(attest "$ROOT_PRIVATE_KEY" "$(mkatt revoke "$ROOT_KEY_ID" "$T_ID" "{\"revokes\":$(echo "$VT" | jq .id)}")")"
check "root revokes V's vouch for T (strike 1)" '.results[0].status == "accepted"' "$R"
R="$(attest "$ROOT_PRIVATE_KEY" "$(mkatt revoke "$ROOT_KEY_ID" "$X_ID" "{\"revokes\":$(echo "$VX" | jq .id)}")")"
check "root revokes V's vouch for X (strike 2)" '.results[0].status == "accepted"' "$R"
wait_for "demoted V is no longer published" "[.items[].key_id] | index(\"$V_ID\") == null" /v1/keys/published
R="$(attest "$(sk_of "$V")" "$(mkatt vouch "$V_ID" "$M_ID" '{"level_granted":"member"}')")"
check "demoted V is in vouch cooldown" '.results[0].status == "rejected" and (.results[0].problem.type | endswith("vouch-cooldown"))' "$R"

TRUST_PUBLIC="$(for u in "/v1/attestations?type=vendor_of&include_inactive=true" "/v1/keys/published" "/v1/changes?after=0&limit=500" "/v1/observations?place_id=$PL&limit=500"; do get "$u"; done)"
if echo "$TRUST_PUBLIC" | grep -q -e "$T_ID" -e "$M_ID" -e "$X_ID" -e '"signature"' -e vouch -e rotate -e '"revokes"'; then
  die "public responses leak the trust graph"
fi
ok "no vouch, revoke or rotate data in public responses"


# ---- exports: snapshot, manifest checksums, privacy, bootstrap + change feed
if [ -n "${EXPORT_DIR:-}" ]; then
  "$BIN" export "$EXPORT_DIR" >/dev/null || die "eatsights export"
  MAN="$(get /v1/export)"
  check "export manifest" '(.files | length == 8) and (.changes_seq > 0) and .license == "CC0-1.0"' "$MAN"
  SEQ="$(echo "$MAN" | jq -r .changes_seq)"
  for NAME in $(echo "$MAN" | jq -r '.files[].name'); do
    curl -sS -D "$TMP/x.h" -o "$TMP/$NAME" "$BASE/v1/export/$NAME"
    grep -qi '^content-type: application/gzip' "$TMP/x.h" || die "$NAME content type"
    SUM="$( (sha256sum "$TMP/$NAME" 2>/dev/null || shasum -a 256 "$TMP/$NAME") | cut -d' ' -f1)"
    [ "$SUM" = "$(echo "$MAN" | jq -r ".files[] | select(.name == \"$NAME\") | .sha256")" ] || die "$NAME checksum"
    [ "$(gunzip -c "$TMP/$NAME" | wc -l | tr -d ' ')" = "$(echo "$MAN" | jq -r ".files[] | select(.name == \"$NAME\") | .records")" ] || die "$NAME records"
    if gunzip -c "$TMP/$NAME" | grep -q -e "$A_ID" -e "$B_ID" -e '"signature"'; then die "$NAME leaks key ids or signatures"; fi
  done
  ok "export files match manifest checksums and record counts"
  ok "export contains no key ids or signatures"
  gunzip -c "$TMP/observations.ndjson.gz" | jq -e -s "[.[].id] | index($(echo "$OA" | jq .id)) != null" >/dev/null || die "export lacks observation"
  ok "export contains the observations"
  CODE="$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/v1/export/..%2Flatest")"
  [ "$CODE" = 404 ] || die "path traversal returned $CODE"
  ok "export path traversal -> 404"
  OC="$(obs "$A_ID" "$A_SK" 1700)"
  R="$(echo "[$OC]" | post /v1/submissions)"; check "observation after the snapshot" '.results[0].status == "accepted"' "$R"
  R="$(get "/v1/changes?after=$SEQ&limit=500")"
  check "bootstrap: changes after manifest seq" "(.items | all(.seq > $SEQ)) and ([.items[].entity_id] | index($(echo "$OC" | jq .id)) != null) and (.items | map(.entity_id) | index($(echo "$OA" | jq .id)) == null)" "$R"
fi
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

# ---- M5: signed admin actions (merge, undo, removal)
admin() { # admin <action> <params json>
  jq -n --arg id "$(newid es_)" --arg a "$1" --argjson p "$2" --arg t "$(now)" --arg i "$ROOT_KEY_ID" \
    '{id:$id,action:$a,params:$p,issued_at:$t,issuer_key_id:$i}' | sign "$ROOT_PRIVATE_KEY"
}
code_of() { curl -sS -o "$TMP/out.json" -w '%{http_code}' "$@"; }
PL2="$(newid pl_)"
R="$(es place "{\"id\":\"$PL2\",\"names\":{\"en\":\"E2E Chaya Kada\"},\"category\":\"tea_stall\",\"location\":{\"lat\":$LAT,\"lng\":$LNG},\"city\":\"ernakulam\"}" | jq -s . | post /v1/entities)"
check "create a duplicate place" '.results[0].status == "accepted"' "$R"
R="$(admin merge "{\"a_id\":\"$PL2\",\"b_id\":\"$PL\"}" | post /v1/admin/merges)"
check "admin merge (older record survives)" ".status == \"accepted\" and .result.merged == \"$PL2\" and .result.merged_into == \"$PL\"" "$R"
R="$(jq -n --arg id "$(newid es_)" --arg p "$PL2" --arg t "$(now)" --arg i "$A_ID" \
  '{id:$id,action:"unmerge",params:{id:$p},issued_at:$t,issuer_key_id:$i}' | sign "$A_SK" | post /v1/admin/merges)"
check "admin action by a member key is forbidden" '.status == 403' "$R"
LOC=""
for _ in $(seq 1 50); do
  LOC="$(curl -sS -o /dev/null -w '%{http_code} %{redirect_url}' "$BASE/v1/places/$PL2")"
  [ "$LOC" = "301 $BASE/v1/places/$PL" ] && break
  sleep 0.1
done
[ "$LOC" = "301 $BASE/v1/places/$PL" ] || die "merged place did not redirect: $LOC"
ok "GET merged place -> 301 to survivor"
R="$(admin unmerge "{\"id\":\"$PL2\"}" | post /v1/admin/merges)"
check "admin undo merge" ".status == \"accepted\" and .result.unmerged == \"$PL2\"" "$R"
CODE=""
for _ in $(seq 1 50); do
  CODE="$(code_of "$BASE/v1/places/$PL2")"
  [ "$CODE" = 200 ] && break
  sleep 0.1
done
[ "$CODE" = 200 ] || die "unmerged place returned $CODE"
check "unmerged place is active again" '.record_state == "active" and (has("merged_into") | not)' "$(cat "$TMP/out.json")"
OA_ID="$(echo "$OA" | jq -r .id)"
REM="$(admin remove_observation "{\"observation_id\":\"$OA_ID\",\"reason\":\"legal\"}")"
R="$(echo "$REM" | post /v1/admin/removals)"
check "admin removal" '.status == "accepted" and .result.reason == "legal"' "$R"
R="$(echo "$REM" | post /v1/admin/removals)"
check "replayed removal is a duplicate" '.status == "duplicate"' "$R"
R=""
for _ in $(seq 1 50); do
  R="$(get "/v1/observations?menu_item_id=$MI")"
  echo "$R" | jq -e "[.items[] | select(.id == \"$OA_ID\")][0].redacted == true" >/dev/null 2>&1 && break
  sleep 0.1
done
check "removed observation is a tombstone" "[.items[] | select(.id == \"$OA_ID\")][0] | .redacted == true and (has(\"value\") | not)" "$R"

echo "e2e: all checks passed"
