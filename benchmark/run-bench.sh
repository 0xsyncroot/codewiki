#!/usr/bin/env bash
# Index benchmark: cold-index each corpus repo, capture time/memory/DB metrics.
set -u
BIN=/root/develop/code-wiki/target/release/codewiki
CORPUS=/root/bench-corpus
OUT=/root/develop/code-wiki/benchmark/results-index.tsv
echo -e "repo\tlang\tfiles\tnodes\tedges\tresolved\tindex_s\trss_mb\tdb_kb\tfiles_per_s\tsync_ms" > "$OUT"

for d in requests lodash flask ripgrep express mediatr zod vuecore tokio django; do
  dir="$CORPUS/$d"
  [ -d "$dir" ] || continue
  rm -rf "$dir/.codewiki"
  # cold index with time -v
  t=$( { /usr/bin/time -v "$BIN" init --path "$dir" ; } 2>&1 )
  elapsed=$(echo "$t" | grep -oP 'Elapsed.*: \K[0-9:.]+' | tail -1)
  rss_kb=$(echo "$t" | grep -oP 'Maximum resident set size \(kbytes\): \K[0-9]+' | tail -1)
  # parse "Indexed N files, M nodes, K edges in Xs" + "Resolved R references"
  idx_line=$(echo "$t" | grep -oP 'Indexed \K[0-9]+ files, [0-9]+ nodes, [0-9]+ edges' | tail -1)
  files=$(echo "$idx_line" | grep -oP '^\K[0-9]+')
  nodes=$(echo "$idx_line" | grep -oP 'files, \K[0-9]+')
  edges=$(echo "$idx_line" | grep -oP 'nodes, \K[0-9]+')
  resolved=$(echo "$t" | grep -oP 'Resolved \K[0-9]+' | tail -1)
  index_s=$(echo "$t" | grep -oP 'in \K[0-9.]+(?=s)' | tail -1)
  db_kb=$(du -k "$dir/.codewiki/codewiki.db" 2>/dev/null | cut -f1)
  rss_mb=$(( ${rss_kb:-0} / 1024 ))
  fps=$(awk "BEGIN{ if(${index_s:-0}>0) printf \"%.0f\", ${files:-0}/${index_s}; else print 0 }")
  # incremental sync: touch one source file
  one=$(find "$dir" -type f \( -name '*.py' -o -name '*.rs' -o -name '*.js' -o -name '*.ts' -o -name '*.cs' -o -name '*.vue' \) ! -path '*/.codewiki/*' | head -1)
  touch "$one" 2>/dev/null
  st=$( { /usr/bin/time -v "$BIN" sync --path "$dir" ; } 2>&1 )
  sync_el=$(echo "$st" | grep -oP 'Elapsed.*: \K[0-9:.]+' | tail -1)
  # convert m:ss.ss → ms (assume <1min)
  sync_ms=$(awk "BEGIN{ split(\"${sync_el:-0}\",a,\":\"); s=(length(a)==2)?a[1]*60+a[2]:a[1]; printf \"%.0f\", s*1000 }")
  echo -e "$d\t-\t${files:-0}\t${nodes:-0}\t${edges:-0}\t${resolved:-0}\t${index_s:-0}\t${rss_mb}\t${db_kb:-0}\t${fps}\t${sync_ms}" >> "$OUT"
  echo "done $d: ${files} files, ${index_s}s, ${rss_mb}MB"
done
echo "=== results-index.tsv ==="
column -t "$OUT"
