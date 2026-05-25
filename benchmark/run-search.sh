#!/usr/bin/env bash
# Search/query latency: for each repo, run each query type 7x, report p50 (ms).
# CLI path = cold DB open per call (worst case); MCP server keeps DB open (sub-ms).
# Reproduces benchmark/results-search.tsv (one row per repo, p50 per query type).
set -u
BIN="${CW:-codewiki}"
ROOT="${BENCH_ROOT:-/tmp/bench}"
OUT="$(dirname "$0")/results-search.tsv"
echo -e "repo\tlang\tquery_exact\tquery_fuzzy\tcallers\tcallees\timpact\tcontext" > "$OUT"

declare -A SYM=([flask]=Flask [ripgrep]=search [express]=Router [zod]=ZodType \
  [eShopOnWeb]=OrderService [gson]=Gson [json]=parse [gin]=Engine)
declare -A LANG=([flask]=Python [ripgrep]=Rust [express]=JavaScript [zod]=TypeScript \
  [eShopOnWeb]="C#" [gson]=Java [json]="C++" [gin]=Go)

p50() { sort -n | awk '{a[NR]=$1} END{ if(NR==0){print 0;exit} print a[int((NR+1)/2)] }'; }
time_ms() { local s e; s=$(date +%s%N); "$@" >/dev/null 2>&1; e=$(date +%s%N); echo $(( (e-s)/1000000 )); }

measure() { # dir qtype sym partial  → p50 over 7 runs
  local dir=$1 qt=$2 sym=$3 partial=$4 vals=()
  for i in 1 2 3 4 5 6 7; do
    case "$qt" in
      query_exact) vals+=( "$(time_ms "$BIN" query "$sym" --path "$dir")" );;
      query_fuzzy) vals+=( "$(time_ms "$BIN" query "$partial" --path "$dir")" );;
      callers)     vals+=( "$(time_ms "$BIN" callers "$sym" --path "$dir")" );;
      callees)     vals+=( "$(time_ms "$BIN" callees "$sym" --path "$dir")" );;
      impact)      vals+=( "$(time_ms "$BIN" impact "$sym" --path "$dir")" );;
      context)     vals+=( "$(time_ms "$BIN" context "how does $sym work" --path "$dir")" );;
    esac
  done
  printf '%s\n' "${vals[@]}" | p50
}

for d in flask ripgrep express zod eShopOnWeb gson json gin; do
  dir="$ROOT/$d"
  [ -d "$dir/.codewiki" ] || { echo "skip $d (no index)"; continue; }
  sym=${SYM[$d]:-main}; partial=${sym:0:4}
  qe=$(measure "$dir" query_exact "$sym" "$partial")
  qf=$(measure "$dir" query_fuzzy "$sym" "$partial")
  cr=$(measure "$dir" callers "$sym" "$partial")
  ce=$(measure "$dir" callees "$sym" "$partial")
  im=$(measure "$dir" impact "$sym" "$partial")
  cx=$(measure "$dir" context "$sym" "$partial")
  echo -e "$d\t${LANG[$d]}\t$qe\t$qf\t$cr\t$ce\t$im\t$cx" >> "$OUT"
  echo "searched $d"
done
echo "=== results-search.tsv ==="
column -t "$OUT"
