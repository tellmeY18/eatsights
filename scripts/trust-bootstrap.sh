#!/usr/bin/env bash
# Bootstrap the production trust chain: a root key and a first verifier.
#
#   scripts/trust-bootstrap.sh sign <dir>     # OFFLINE machine: make keys, pre-sign everything
#   scripts/trust-bootstrap.sh post <dir> <base-url>   # anywhere: send the two public files
#
# `sign` writes into <dir>:
#   root.json, verifier.json          PRIVATE keys (mode 600). Keep offline / on a hardware key.
#   root.public                       43-char public key -> EATSIGHTS_ROOT_PUBLIC_KEY secret
#   verifier-registration.json        public, signed by the verifier (proof of possession)
#   verifier-vouch.json               public, signed by the root: grants `verifier`
# Neither registration nor attestation has a server-side age limit, so the two public files
# can be signed days before they are posted. `post` needs no private key.
#
# Needs: the eatsights binary ($EATSIGHTS_BIN, default target/release/eatsights) and jq.
set -euo pipefail

BIN="${EATSIGHTS_BIN:-target/release/eatsights}"
die() { echo "trust-bootstrap: $*" >&2; exit 1; }
now() { date -u +%Y-%m-%dT%H:%M:%SZ; }

cmd="${1:-}"; dir="${2:-}"
[ -n "$cmd" ] && [ -n "$dir" ] || die "usage: $0 sign <dir> | post <dir> <base-url>"

case "$cmd" in
sign)
  [ -x "$BIN" ] || die "no binary at $BIN (cargo build --release -p eatsights)"
  umask 077
  mkdir -p "$dir"
  for k in root verifier; do
    if [ -e "$dir/$k.json" ]; then
      echo "keeping existing $dir/$k.json"
    else
      "$BIN" keygen > "$dir/$k.json"
    fi
  done
  chmod 600 "$dir/root.json" "$dir/verifier.json"
  ROOT_SK="$(jq -r .private_key "$dir/root.json")"
  VER_SK="$(jq -r .private_key "$dir/verifier.json")"
  VER_PK="$(jq -r .public_key "$dir/verifier.json")"
  VER_ID="$(jq -r .key_id "$dir/verifier.json")"
  ROOT_ID="$(jq -r .key_id "$dir/root.json")"
  jq -r .public_key "$dir/root.json" > "$dir/root.public"

  jq -n --arg pk "$VER_PK" --arg t "$(now)" \
    '{type: "key_registration", algorithm: "ed25519", public_key: $pk, created_at: $t}' |
    "$BIN" sign --key="$VER_SK" > "$dir/verifier-registration.json"

  jq -n --arg id "$("$BIN" id at_)" --arg iss "$ROOT_ID" --arg sub "$VER_ID" --arg t "$(now)" \
    '{id: $id, type: "vouch", issuer_key_id: $iss, subject_key_id: $sub,
      level_granted: "verifier", issued_at: $t}' |
    "$BIN" sign --key="$ROOT_SK" > "$dir/verifier-vouch.json"
  chmod 644 "$dir/root.public" "$dir/verifier-registration.json" "$dir/verifier-vouch.json"

  echo "root key_id:     $ROOT_ID"
  echo "verifier key_id: $VER_ID"
  echo "EATSIGHTS_ROOT_PUBLIC_KEY=$(cat "$dir/root.public")"
  echo "Copy ONLY root.public, verifier-registration.json and verifier-vouch.json off this machine."
  ;;
post)
  base="${3:-}"; [ -n "$base" ] || die "usage: $0 post <dir> <base-url>"
  for f in verifier-registration.json verifier-vouch.json; do
    [ -f "$dir/$f" ] || die "missing $dir/$f"
    if jq -e 'has("private_key")' "$dir/$f" >/dev/null; then die "$f contains a private key"; fi
  done
  echo "register verifier key:"
  curl -fsS -X POST -H 'content-type: application/json' \
    --data-binary @"$dir/verifier-registration.json" "$base/v1/keys" | jq -c .
  echo "root vouches verifier:"
  r="$(jq -s . "$dir/verifier-vouch.json" | curl -fsS -X POST -H 'content-type: application/json' \
    --data-binary @- "$base/v1/attestations")"
  echo "$r" | jq -c .
  echo "$r" | jq -e '.results[0].status | IN("accepted", "duplicate")' >/dev/null ||
    die "vouch not accepted (see the problem above; is EATSIGHTS_ROOT_PUBLIC_KEY this root.public?)"
  echo "published keys:"
  curl -fsS "$base/v1/keys/published" | jq -c '[.items[] | {key_id, level}]'
  ;;
*) die "unknown command $cmd" ;;
esac
