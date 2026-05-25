#!/usr/bin/env bash
# Search/MCP latency: for each repo, run query types 5x, report p50/p95 (ms).
set -u
BIN=/root/develop/code-wiki/target/release/codewiki
CORPUS=/root/bench-corpus
OUT=/root/develop/code-wiki/benchmark/results-search.tsv
echo -e "repo\tquery_type\tp50_ms\tp95_ms" > "$OUT"

declare -A SYM=(
  [requests]=Session [flask]=Flask [django]=Model [ripgrep]=search
  [express]=Router [zod]=ZodType [mediatr]=Mediator [vuecore]=createApp
  [tokio]=spawn [lodash]=debounce
)

pctl() { local p=$1; sort -n | awk -v p="$p" '{a[NR]=$1} END{ if(NR==0){print 0;exit} i=int((p/100)*NR); if(i<1)i=1; if(i>NR)i=NR; print a[i] }'; }
time_ms() { local s e; s=$(date +%s%N); "$@" >/dev/null 2>&1; e=$(date +%s%N); echo $(( (e-s)/1000000 )); }

run_type() { # repo dir qtype sym partial  → emits 5 timings via $@ command builder
  local dir=$1 qt=$2 sym=$3 partial=$4
  local vals=()
  for i in 1 2 3 4 5; do
    case "$qt" in
      query_exact) vals+=( "$(time_ms "$BIN" query "$sym" --path "$dir")" );;
      query_fuzzy) vals+=( "$(time_ms "$BIN" query "$partial" --path "$dir")" );;
      callers)     vals+=( "$(time_ms "$BIN" callers "$sym" --path "$dir")" );;
      callees)     vals+=( "$(time_ms "$BIN" callees "$sym" --path "$dir")" );;
      impact)      vals+=( "$(time_ms "$BIN" impact "$sym" --path "$dir")" );;
      context)     vals+=( "$(time_ms "$BIN" context "how does $sym work" --path "$dir")" );;
    esac
  done
  local p50 p95
  p50=$(printf '%s\n' "${vals[@]}" | pctl 50)
  p95=$(printf '%s\n' "${vals[@]}" | pctl 95)
  echo -e "$d\t$qt\t$p50\t$p95" >> "$OUT"
}

for d in requests flask django ripgrep express zod mediatr vuecore tokio lodash; do
  dir="$CORPUS/$d"
  [ -d "$dir/.codewiki" ] || continue
  sym=${SYM[$d]:-main}; partial=${sym:0:4}
  for qt in query_exact query_fuzzy callers callees impact context; do
    run_type "$dir" "$qt" "$sym" "$partial"
  done
  echo "searched $d"
done
echo "=== results-search.tsv ==="
column -t "$OUT"
