#!/usr/bin/env bash
# Cross-language cold-index benchmark on fresh shallow clones under /tmp/bench.
# Reproduces benchmark/results-index.tsv: files, final graph nodes/edges, unresolved,
# cold-index wall time (3-run avg, ms), peak RSS, DB size, files/s, incremental sync.
#
# Usage:
#   for r in pallets/flask BurntSushi/ripgrep expressjs/express colinhacks/zod \
#            dotnet-architecture/eShopOnWeb google/gson nlohmann/json gin-gonic/gin; do
#     git clone --depth 1 https://github.com/$r /tmp/bench/$(basename $r)
#   done
#   CW=/path/to/codewiki bash run-index.sh
set -u
BIN="${CW:-codewiki}"
ROOT="${BENCH_ROOT:-/tmp/bench}"
OUT="$(dirname "$0")/results-index.tsv"
echo -e "repo\tlang\tsha\tsrc_files\tnodes\tedges\tunresolved\tindex_ms\trss_mb\tdb_kb\tfiles_per_s\tsync_ms" > "$OUT"

declare -A LANG=(
  [flask]=Python [ripgrep]=Rust [express]=JavaScript [zod]=TypeScript
  [eShopOnWeb]="C#" [gson]=Java [json]="C++" [gin]=Go
)

for d in flask ripgrep express zod eShopOnWeb gson json gin; do
  dir="$ROOT/$d"; db="$dir/.codewiki/codewiki.db"
  [ -d "$dir" ] || { echo "MISSING $dir"; continue; }
  sha=$(git -C "$dir" rev-parse --short HEAD 2>/dev/null)

  # 3 clean cold-index runs; record ms each, average for the published figure.
  times=(); rss_kb=0
  for r in 1 2 3; do
    rm -rf "$dir/.codewiki"
    t=$( { /usr/bin/time -v "$BIN" init --path "$dir" ; } 2>&1 )
    el=$(echo "$t" | grep -oP 'Elapsed.*: \K[0-9:.]+' | tail -1)
    ms=$(awk "BEGIN{ split(\"${el:-0}\",a,\":\"); s=(length(a)==2)?a[1]*60+a[2]:a[1]; printf \"%.0f\", s*1000 }")
    times+=( "$ms" )
    k=$(echo "$t" | grep -oP 'Maximum resident set size \(kbytes\): \K[0-9]+' | tail -1)
    [ "${k:-0}" -gt "$rss_kb" ] && rss_kb=$k
  done
  avg=$(printf '%s\n' "${times[@]}" | awk '{t+=$1;n++} END{printf "%.0f", t/n}')

  # Final resolved graph totals (what the agent queries).
  sf=$(sqlite3 "$db" "SELECT COUNT(*) FROM nodes WHERE kind='file';")
  n=$(sqlite3 "$db" "SELECT COUNT(*) FROM nodes;")
  e=$(sqlite3 "$db" "SELECT COUNT(*) FROM edges;")
  u=$(sqlite3 "$db" "SELECT COUNT(*) FROM unresolved_refs;")
  db_kb=$(du -k "$db" 2>/dev/null | cut -f1)
  rss_mb=$(( rss_kb / 1024 ))
  fps=$(awk "BEGIN{ if($avg>0) printf \"%.0f\", $sf/($avg/1000); else print 0 }")

  # Incremental sync: median of 3 single-file touches (skip test dirs).
  st=()
  for r in 1 2 3; do
    one=$(find "$dir" -type f \( -name '*.py' -o -name '*.rs' -o -name '*.js' -o -name '*.ts' \
          -o -name '*.cs' -o -name '*.java' -o -name '*.hpp' -o -name '*.cpp' -o -name '*.go' \) \
          ! -path '*/.codewiki/*' ! -path '*/test*' | sed -n "${r}p")
    [ -z "$one" ] && one=$(find "$dir" -type f ! -path '*/.codewiki/*' | head -1)
    touch "$one"
    s=$(date +%s%N); "$BIN" sync --path "$dir" >/dev/null 2>&1; ee=$(date +%s%N)
    st+=( $(( (ee-s)/1000000 )) )
  done
  sync_ms=$(printf '%s\n' "${st[@]}" | sort -n | awk '{a[NR]=$1} END{print a[int(NR/2)+1]}')

  echo -e "$d\t${LANG[$d]}\t$sha\t$sf\t$n\t$e\t$u\t$avg\t$rss_mb\t${db_kb:-0}\t$fps\t$sync_ms" >> "$OUT"
  echo "done $d (${LANG[$d]}): $sf files, $n nodes, $e edges, ${avg}ms, ${fps} f/s, ${rss_mb}MB, sync ${sync_ms}ms"
done
echo "=== results-index.tsv ==="
column -t "$OUT"
