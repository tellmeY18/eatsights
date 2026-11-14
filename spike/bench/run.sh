#!/usr/bin/env bash
# usage (inside nix develop): spike/bench/run.sh  -> p50/p99 per script, 4 clients, 15 s
set -euo pipefail
cd "$(dirname "$0")"
out=${OUT:-/tmp/pgb}; mkdir -p "$out"
for s in near_postgis near_geohash5 near_geohash6 near_earth latest_fact insert_obs; do
  rm -f "$out"/pgbench_log.*
  tps=$(cd "$out" && pgbench -n -h localhost -p "${PGPORT:-55432}" -U eatsights -c 4 -j 4 -T 15 -l -f "$OLDPWD/$s.sql" eatsights | awk '/^tps/{print $3}')
  cat "$out"/pgbench_log.* | awk '{print $3/1000}' | sort -n | awk -v s="$s" -v t="$tps" \
    '{a[NR]=$1} END{printf "%-14s tps=%8.0f  p50=%.2fms  p99=%.2fms  n=%d\n", s, t, a[int(NR*.5)], a[int(NR*.99)], NR}'
done
