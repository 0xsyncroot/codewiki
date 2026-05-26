#!/usr/bin/env bash
# run-savings.sh — CodeWiki agent-savings BEFORE/AFTER harness (canonical, unified).
#
# THE single savings runner. It drives every case in the canonical benchmark/cases.tsv
# (157 cases over 11 real repos) and measures, on a COLD index, both:
#   - CodeWiki: tool-call count, output bytes (tokens = bytes/4), recall vs the frozen
#     oracle (benchmark/oracle/<lang>_<case_id>.json), scored by lib/score.py.
#   - Baseline: a grep + read-N-oracle-files agent, same metrics, scored on the same
#     oracle so recall is comparable.
# It is the ONLY savings runner: the per-repo baseline source maps for every repo are
# folded into the single SRC map below (covering 8 per-language repos + a large .NET repo
# + 2 enterprise repos), so there are no per-batch sibling runners to keep in sync.
#
# A repo whose `init` exits non-zero OR whose post-index node count is trivial is marked
# BROKEN and SKIPPED loudly — a silently-broken index is never scored.
#
# Each case row carries an 8th `case_id` column; the oracle is resolved as
# oracle/<lang>_<case_id>.json (so multiple cases sharing an archetype within a language
# never collide). Build the oracles with lib/build_oracle_extra.py (the shared,
# language-agnostic builder) against this same cases.tsv.
#
# IMPORTANT — measured over MCP, NOT the CLI. Every CodeWiki tool call goes through the
# MCP stdio server (`codewiki serve --mcp`, via lib/mcp_call.py): initialize ->
# notifications/initialized -> tools/call. The agent-savings optimisations (relative-path
# rendering, node include* options, context dedup/density) live ONLY in the MCP tool
# renderers — the CLI output is unchanged BETWEEN BEFORE and AFTER, so a CLI measurement
# would under-report the win. MCP is the surface agents use, so MCP is what we compare.
#
# Tool mapping (CW, all MCP):
#   locate  -> codewiki_search     callers -> codewiki_callers   callees -> codewiki_callees
#   feature -> codewiki_context(x3) impl   -> codewiki_impact     blast   -> codewiki_impact
# MCP coverage: every case ALSO issues one `codewiki_explore` (median-of-3) + one
# `codewiki_node` call so the harness exercises those two MCP-only tools. Their bytes are
# recorded in a SEPARATE column (mcp_coverage_bytes), NOT folded into the primary
# call/token comparison, so the per-archetype reduction stays apples-to-apples vs the
# baseline's same task. node is deterministic; explore is median-of-3.
#
# Recall means EXCLUDE empty-oracle UNSCORABLE cases (deliberate honest graph-gap cases)
# so one expected-failure case doesn't skew recall; those rows STILL count in the
# call/token totals.
#
# Usage:
#   benchmark/run-savings.sh                              # cold clone+index -> results-savings.tsv
#   NO_CLONE=1 ...     skip git clone (use repos already in $BENCH_ROOT)
#   NO_REINDEX=1 ...   skip cold re-index (reuse existing .codewiki)
#   CW=/tmp/codewiki-before-opt OUT=results-savings-before.tsv benchmark/run-savings.sh   # BEFORE capture
#
# Env:
#   CW          CodeWiki binary (default: ../target/release/codewiki if present, else codewiki)
#   BENCH_ROOT  repo checkout root (default: /tmp/bench)
#   OUT         output TSV basename under benchmark/ (default: results-savings.tsv)

set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -z "${CW:-}" ]; then
  if [ -x "$HERE/../target/release/codewiki" ]; then CW="$HERE/../target/release/codewiki"; else CW="codewiki"; fi
fi
BENCH_ROOT="${BENCH_ROOT:-/tmp/bench}"
OUT_NAME="${OUT:-results-savings.tsv}"
OUT="$HERE/$OUT_NAME"
SUMMARY="${OUT%.tsv}-summary.txt"
CASES="${CASES:-$HERE/cases.tsv}"
ORACLE="$HERE/oracle"
SCORE="python3 $HERE/lib/score.py"
MCP="python3 $HERE/lib/mcp_call.py"
PRICE_PER_MTOK="${PRICE_PER_MTOK:-3.00}"   # Claude Sonnet input $/1M tokens

# repo -> github slug (for cold clone). 8 per-language repos (small/medium tier), a
# large .NET repo (jellyfin ~2,065 .cs) that gives C# a genuine large-tier data point,
# PLUS 2 large/enterprise-tier repos (kubernetes ~17k files, microsoft/TypeScript ~39k
# files) that substantiate the enterprise size tier and the "saving grows with repo size"
# claim. jellyfin specifically validates the C# type-ref + impact fixes at scale.
declare -A SLUG=(
  [flask]=pallets/flask [ripgrep]=BurntSushi/ripgrep [express]=expressjs/express
  [zod]=colinhacks/zod [gson]=google/gson [json]=nlohmann/json
  [gin]=gin-gonic/gin [eShopOnWeb]=dotnet-architecture/eShopOnWeb
  [jellyfin]=jellyfin/jellyfin
  [kubernetes]=kubernetes/kubernetes [TypeScript]=microsoft/TypeScript
)
# repo -> source subdir(s) the baseline agent would grep (avoids vendored/test noise
# dominating the grep byte count; mirrors what a real agent scopes to). For the two
# enterprise repos the baseline is scoped to the production source tree (k8s pkg/, TS
# src/) exactly as a real agent would — NOT vendor/ or generated test baselines.
declare -A SRC=(
  [flask]="src" [ripgrep]="crates" [express]="lib" [zod]="packages/zod/src"
  [gson]="gson/src/main" [json]="include" [gin]="." [eShopOnWeb]="src"
  [kubernetes]="pkg" [TypeScript]="src"
  # jellyfin's production source is split across several top-level project trees; a real
  # agent scopes its grep to those (NOT tests/ or fuzz/). Space-separated multi-dir SRC.
  [jellyfin]="Emby.Server.Implementations MediaBrowser.Controller MediaBrowser.Model Jellyfin.Api MediaBrowser.Providers src"
)

bytes() { wc -c | tr -d ' '; }
median3() { printf '%s\n' "$1" "$2" "$3" | sort -n | sed -n 2p; }
# JSON-encode a bash string into a quoted JSON scalar (handles quotes/backslashes).
jstr() { python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$1"; }
# One MCP tool call over stdio; prints the tool's text payload.
jsonq() { $MCP --bin "$1" --path "$2" --tool "$3" --args "$4" 2>/dev/null; }
oracle_files() {  # echo the required_files of an oracle (space-separated)
  python3 -c "import json,sys;print(' '.join(json.load(open(sys.argv[1])).get('required_files',[])))" "$1"
}

# Minimum post-index node count below which an index is treated as broken (real repos
# here yield 1.6k–312k nodes; a silent FK/parse collapse drops the count to ~0). This is a
# floor, NOT a per-repo expectation — any non-trivial repo clears it by orders of magnitude.
MIN_NODES="${MIN_NODES:-50}"
# Repos whose index failed (non-zero init exit or trivial node count). These are recorded
# loudly here and skipped in the scoring loop so a silently-broken index is NEVER measured.
declare -A BROKEN_REPOS=()

# Run `codewiki init`, capturing its exit code AND its output (so an FK/parse failure is
# printed, not swallowed). Returns init's exit code.
do_init() {
  local dir="$1" log; log="$(mktemp)"
  "$CW" init --path "$dir" >"$log" 2>&1
  local rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "  !! init FAILED (exit $rc) for $dir — error output:"
    sed 's/^/       /' "$log"
  fi
  rm -f "$log"
  return $rc
}

clone_and_index() {
  echo "## Preparing repos (clone=$([ -n "${NO_CLONE:-}" ] && echo skip || echo yes), reindex=$([ -n "${NO_REINDEX:-}" ] && echo skip || echo cold))"
  mkdir -p "$BENCH_ROOT"
  for repo in "${!SLUG[@]}"; do
    local dir="$BENCH_ROOT/$repo"
    if [ -z "${NO_CLONE:-}" ] && [ ! -d "$dir/.git" ]; then
      echo "  clone ${SLUG[$repo]} -> $dir"
      git clone --depth 1 "https://github.com/${SLUG[$repo]}" "$dir" >/dev/null 2>&1
    fi
    if [ ! -d "$dir" ]; then
      echo "  MISSING $dir — marking BROKEN (will not be scored)"
      BROKEN_REPOS[$repo]="missing-checkout"; continue
    fi
    # Index, capturing the exit code. A non-zero init (e.g. an FK failure) used to be
    # swallowed by `>/dev/null 2>&1`, letting a broken index get scored silently — that
    # exact bug once hid a C# collapse. Now: print the error and mark the repo BROKEN.
    local rc=0
    if [ -z "${NO_REINDEX:-}" ]; then
      rm -rf "$dir/.codewiki"
      do_init "$dir"; rc=$?
    elif [ ! -f "$dir/.codewiki/codewiki.db" ]; then
      do_init "$dir"; rc=$?
    fi
    if [ "$rc" -ne 0 ]; then
      BROKEN_REPOS[$repo]="init-exit-$rc"; echo "  $repo: BROKEN (init exit $rc) — SKIPPED"; continue
    fi
    # Assert the index exists and has a non-trivial node count. A DB that is missing or
    # near-empty after a "successful" init is a silent collapse and must never be scored.
    if [ ! -f "$dir/.codewiki/codewiki.db" ]; then
      BROKEN_REPOS[$repo]="no-db"; echo "  $repo: BROKEN (no codewiki.db after init) — SKIPPED"; continue
    fi
    local n; n=$(sqlite3 "$dir/.codewiki/codewiki.db" "select count(*) from nodes" 2>/dev/null || echo 0)
    if [ -z "$n" ] || [ "$n" -lt "$MIN_NODES" ]; then
      BROKEN_REPOS[$repo]="too-few-nodes-${n:-0}"
      echo "  $repo: BROKEN (only ${n:-0} nodes < MIN_NODES=$MIN_NODES — index collapsed) — SKIPPED"
      continue
    fi
    echo "  $repo: $n nodes"
  done
  if [ "${#BROKEN_REPOS[@]}" -ne 0 ]; then
    echo ""
    echo "## !! ${#BROKEN_REPOS[@]} repo(s) have a BROKEN index and will NOT be scored:"
    for r in "${!BROKEN_REPOS[@]}"; do echo "     - $r (${BROKEN_REPOS[$r]})"; done
    echo "   (A silently-broken index is never measured. Fix the index, then re-run.)"
  fi
}

# ------- CW side per archetype: writes output to $6 (CWOUT), prints "<calls>\t<bytes>"
run_cw() {
  local lang="$1" repo="$2" arch="$3" symbol="$4" feature="$5" CWOUT="$6"
  local dir="$BENCH_ROOT/$repo"; local calls=0; : > "$CWOUT"
  case "$arch" in
    locate)  jsonq "$CW" "$dir" codewiki_search "{\"query\":$(jstr "$symbol"),\"limit\":10}" > "$CWOUT"; calls=1 ;;
    callers) jsonq "$CW" "$dir" codewiki_callers "{\"symbol\":$(jstr "$symbol"),\"limit\":20}" > "$CWOUT"; calls=1 ;;
    callees) jsonq "$CW" "$dir" codewiki_callees "{\"symbol\":$(jstr "$symbol"),\"limit\":20}" > "$CWOUT"; calls=1 ;;
    impl|blast) jsonq "$CW" "$dir" codewiki_impact "{\"symbol\":$(jstr "$symbol"),\"depth\":3}" > "$CWOUT"; calls=1 ;;
    feature)
      # context is ranked/non-deterministic -> median-of-3 by output bytes; keep the
      # median run's text for scoring.
      local t1 t2 t3 b1 b2 b3 med
      t1="$(mktemp)"; t2="$(mktemp)"; t3="$(mktemp)"
      jsonq "$CW" "$dir" codewiki_context "{\"task\":$(jstr "$feature")}" > "$t1"
      jsonq "$CW" "$dir" codewiki_context "{\"task\":$(jstr "$feature")}" > "$t2"
      jsonq "$CW" "$dir" codewiki_context "{\"task\":$(jstr "$feature")}" > "$t3"
      b1=$(wc -c < "$t1"); b2=$(wc -c < "$t2"); b3=$(wc -c < "$t3")
      med=$(median3 "$b1" "$b2" "$b3")
      if [ "$med" = "$b1" ]; then cp "$t1" "$CWOUT"; elif [ "$med" = "$b2" ]; then cp "$t2" "$CWOUT"; else cp "$t3" "$CWOUT"; fi
      rm -f "$t1" "$t2" "$t3"; calls=1 ;;
  esac
  local b; b=$(wc -c < "$CWOUT"); echo -e "${calls}\t${b}"
}

# ------- MCP coverage probe (explore median-of-3 + node), bytes only.
# Exercises the MCP server path and represents the extra answer surface a real agent
# pulls for the same task. Bytes recorded separately, NOT folded into the primary totals.
run_mcp_coverage() {
  local repo="$1" symbol="$2" query="$3"; local dir="$BENCH_ROOT/$repo"
  local eb1 eb2 eb3 emed nb
  eb1=$($MCP --bin "$CW" --path "$dir" --tool codewiki_explore --args "{\"query\":\"$query\",\"maxFiles\":5}" 2>/dev/null | bytes)
  eb2=$($MCP --bin "$CW" --path "$dir" --tool codewiki_explore --args "{\"query\":\"$query\",\"maxFiles\":5}" 2>/dev/null | bytes)
  eb3=$($MCP --bin "$CW" --path "$dir" --tool codewiki_explore --args "{\"query\":\"$query\",\"maxFiles\":5}" 2>/dev/null | bytes)
  emed=$(median3 "${eb1:-0}" "${eb2:-0}" "${eb3:-0}")
  nb=$($MCP --bin "$CW" --path "$dir" --tool codewiki_node --args "{\"symbol\":\"$symbol\",\"includeCode\":false}" 2>/dev/null | bytes)
  echo $(( ${emed:-0} + ${nb:-0} ))
}

# ------- Baseline side: grep + read N oracle files. writes to $5 (BLOUT), prints "<calls>\t<bytes>"
run_baseline() {
  local repo="$1" arch="$2" regex="$3" oraclepath="$4" BLOUT="$5"
  local dir="$BENCH_ROOT/$repo" src="${SRC[$repo]:-.}"; : > "$BLOUT"
  # 1 grep call over the scoped source dir(s). SRC may name MULTIPLE space-separated
  # subdirs (a multi-project repo like jellyfin scopes to its production project trees,
  # not tests/fuzz) — word-split into per-dir paths so each is a real grep target. A
  # single-dir SRC (the 8 per-language + 2 enterprise repos) behaves exactly as before.
  local scoped=() s
  for s in $src; do scoped+=("$dir/$s"); done
  grep -rnIE "$regex" "${scoped[@]}" 2>/dev/null >> "$BLOUT"
  local gcalls=1
  # read N files = the oracle's required_files (what the agent must open to answer)
  local files; files=$(oracle_files "$oraclepath")
  local nfiles=0 f hit
  for f in $files; do
    # Deterministic resolution: prefer a non-test source path, sorted by path length
    # (shortest = closest to the canonical definition), then lexically.
    hit=$(find "$dir" -name "$f" -type f 2>/dev/null \
            | grep -vE '/(test|tests|spec|__tests__|example|examples|benchmark)/' \
            | awk '{print length, $0}' | sort -n | head -1 | cut -d' ' -f2-)
    [ -z "$hit" ] && hit=$(find "$dir" -name "$f" -type f 2>/dev/null | sort | head -1)
    if [ -n "$hit" ]; then cat "$hit" >> "$BLOUT" 2>/dev/null; nfiles=$((nfiles+1)); fi
  done
  local b; b=$(wc -c < "$BLOUT"); echo -e "$(( gcalls + nfiles ))\t${b}"
}

main() {
  clone_and_index
  echo -e "lang\trepo\tarchetype\tcase_id\tsymbol\tcw_calls\tcw_bytes\tcw_recall\tcw_verdict\tbl_calls\tbl_bytes\tbl_recall\tbl_verdict\tmcp_coverage_bytes" > "$OUT"

  # NOTE: read with IFS=tab collapses consecutive tabs (tab is IFS-whitespace), which
  # eats empty middle columns. Extract each column with cut from the raw line instead
  # so empty feature/regex/anchors fields stay aligned.
  local raw lang repo arch symbol feature regex anchors case_id
  while IFS= read -r raw; do
    lang="$(printf '%s' "$raw" | cut -f1)"
    case "$lang" in ""|\#*|lang) continue;; esac
    repo="$(printf '%s' "$raw" | cut -f2)"
    arch="$(printf '%s' "$raw" | cut -f3)"
    symbol="$(printf '%s' "$raw" | cut -f4)"
    feature="$(printf '%s' "$raw" | cut -f5)"
    regex="$(printf '%s' "$raw" | cut -f6)"
    anchors="$(printf '%s' "$raw" | cut -f7)"
    case_id="$(printf '%s' "$raw" | cut -f8)"
    [ -z "$case_id" ] && { echo "  [skip] no case_id for $lang/$arch/$symbol"; continue; }
    local oraclepath="$ORACLE/${lang}_${case_id}.json"
    if [ ! -f "$oraclepath" ]; then echo "  [skip] no oracle $oraclepath"; continue; fi
    local dir="$BENCH_ROOT/$repo"
    if [ -n "${BROKEN_REPOS[$repo]:-}" ]; then echo "  [skip] $repo BROKEN (${BROKEN_REPOS[$repo]}) — not scored"; continue; fi
    if [ ! -f "$dir/.codewiki/codewiki.db" ]; then echo "  [skip] $repo not indexed"; continue; fi

    # explore probe query: feature query if present else the symbol terms
    local probe="${feature:-$symbol}"; [ -z "$probe" ] && probe="$symbol"
    local CWOUT BLOUT; CWOUT="$(mktemp)"; BLOUT="$(mktemp)"

    # --- CW primary tool (the one that answers this archetype) ---
    local cwline cw_calls cw_bytes
    cwline=$(run_cw "$lang" "$repo" "$arch" "$symbol" "$feature" "$CWOUT")
    cw_calls=$(echo "$cwline" | cut -f1); cw_bytes=$(echo "$cwline" | cut -f2)
    local cw_score cw_recall cw_verdict
    cw_score=$($SCORE --oracle "$oraclepath" --output "$CWOUT" --json 2>/dev/null)
    cw_recall=$(echo "$cw_score" | python3 -c "import json,sys;print(json.load(sys.stdin)['recall'])" 2>/dev/null || echo 0)
    cw_verdict=$(echo "$cw_score" | python3 -c "import json,sys;print(json.load(sys.stdin)['verdict'])" 2>/dev/null || echo UNSCORABLE)

    # --- MCP coverage probe (explore median-of-3 + node), measured but NOT folded in ---
    local mcp_bytes; mcp_bytes=$(run_mcp_coverage "$repo" "$symbol" "$probe")

    # --- Baseline (grep + read N oracle files), same task, scored on same oracle ---
    local blline bl_calls bl_bytes bl_score bl_recall bl_verdict
    blline=$(run_baseline "$repo" "$arch" "$regex" "$oraclepath" "$BLOUT")
    bl_calls=$(echo "$blline" | cut -f1); bl_bytes=$(echo "$blline" | cut -f2)
    bl_score=$($SCORE --oracle "$oraclepath" --output "$BLOUT" --json 2>/dev/null)
    bl_recall=$(echo "$bl_score" | python3 -c "import json,sys;print(json.load(sys.stdin)['recall'])" 2>/dev/null || echo 0)
    bl_verdict=$(echo "$bl_score" | python3 -c "import json,sys;print(json.load(sys.stdin)['verdict'])" 2>/dev/null || echo UNSCORABLE)

    echo -e "${lang}\t${repo}\t${arch}\t${case_id}\t${symbol}\t${cw_calls}\t${cw_bytes}\t${cw_recall}\t${cw_verdict}\t${bl_calls}\t${bl_bytes}\t${bl_recall}\t${bl_verdict}\t${mcp_bytes}" >> "$OUT"
    printf "  %-11s %-8s %-30s CW %dc/%dB r=%s %-10s  BL %dc/%dB r=%s %-10s\n" \
      "$lang" "$arch" "$case_id" "$cw_calls" "$cw_bytes" "$cw_recall" "$cw_verdict" \
      "$bl_calls" "$bl_bytes" "$bl_recall" "$bl_verdict"
    rm -f "$CWOUT" "$BLOUT"
  done < "$CASES"

  summarize
}

summarize() {
  echo ""
  echo "=== Per-language + grand-total summary -> $SUMMARY ==="
  PRICE="$PRICE_PER_MTOK" python3 - "$OUT" <<'PY' | tee "$SUMMARY"
import csv, sys, os
rows = list(csv.DictReader(open(sys.argv[1]), delimiter='\t'))
price = float(os.environ.get("PRICE", "3.00"))
def agg(rs):
    # recall mean over SCORABLE cases only (UNSCORABLE empty-oracle honest graph-gap
    # cases stay in call/token totals but are not counted in the recall denominator).
    rs_s = [r for r in rs if r['cw_verdict'] != 'UNSCORABLE' and r['bl_verdict'] != 'UNSCORABLE']
    cwc=sum(int(r['cw_calls']) for r in rs); blc=sum(int(r['bl_calls']) for r in rs)
    cwb=sum(int(r['cw_bytes']) for r in rs); blb=sum(int(r['bl_bytes']) for r in rs)
    cwt=cwb/4; blt=blb/4; ns=len(rs_s) or 1
    cwr=sum(float(r['cw_recall']) for r in rs_s)/ns
    blr=sum(float(r['bl_recall']) for r in rs_s)/ns
    callred=100*(blc-cwc)/blc if blc else 0
    tokred=100*(blt-cwt)/blt if blt else 0
    saved=(blt-cwt)*price/1_000_000
    return dict(n=len(rs),ns=len(rs_s),cwc=cwc,blc=blc,cwt=cwt,blt=blt,cwr=cwr,blr=blr,
                callred=callred,tokred=tokred,saved=saved)
langs=sorted({r['lang'] for r in rows})
print(f"\n{'LANG':<11}{'N':>3}{'sc':>3}  {'CWcalls':>7}{'BLcalls':>8} {'call-red':>9}  "
      f"{'CWtok':>8}{'BLtok':>9} {'tok-red':>8}  {'CWrec':>6}{'BLrec':>6}  {'$saved':>8}")
print("-"*98)
for lg in langs:
    a=agg([r for r in rows if r['lang']==lg])
    print(f"{lg:<11}{a['n']:>3}{a['ns']:>3}  {a['cwc']:>7}{a['blc']:>8} {a['callred']:>8.0f}%  "
          f"{a['cwt']:>8.0f}{a['blt']:>9.0f} {a['tokred']:>7.0f}%  "
          f"{a['cwr']:>6.2f}{a['blr']:>6.2f}  {a['saved']:>8.4f}")
print("-"*98)
g=agg(rows)
print(f"{'TOTAL':<11}{g['n']:>3}{g['ns']:>3}  {g['cwc']:>7}{g['blc']:>8} {g['callred']:>8.0f}%  "
      f"{g['cwt']:>8.0f}{g['blt']:>9.0f} {g['tokred']:>7.0f}%  "
      f"{g['cwr']:>6.2f}{g['blr']:>6.2f}  {g['saved']:>8.4f}")
from collections import Counter
hc=Counter(r['cw_verdict'] for r in rows); hb=Counter(r['bl_verdict'] for r in rows)
order=['PASS','PARTIAL','FAIL','UNSCORABLE']
print(f"\nCW  verdicts: " + "  ".join(f"{k}={hc.get(k,0)}" for k in order))
print(f"BL  verdicts: " + "  ".join(f"{k}={hb.get(k,0)}" for k in order))
print(f"\n(N=total cases, sc=cases scored for recall. UNSCORABLE = empty-oracle honest "
      f"graph-gap cases — excluded from recall means, kept in call/token totals.)")
mcp_b=sum(int(r.get('mcp_coverage_bytes',0) or 0) for r in rows)
print(f"\nMCP coverage (explore med-of-3 + node, exercised over stdio, not in totals above): "
      f"{mcp_b/4:.0f} tokens across {g['n']} cases (~{mcp_b/4/g['n']:.0f} tok/case).")
print(f"\nGrand total: {g['callred']:.0f}% fewer tool-calls, {g['tokred']:.0f}% fewer tokens, "
      f"${g['saved']:.4f} saved across {g['n']} cases "
      f"(~${g['saved']/g['n']*20:.2f} per 20-interaction session).")
print(f"Mean recall (scored): CodeWiki {g['cwr']:.2f} vs grep+read baseline {g['blr']:.2f}.")
PY
  echo ""
  echo "Raw: $OUT"
}

main "$@"
