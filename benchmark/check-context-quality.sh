#!/usr/bin/env bash
# B5 context quality check — Wave-3 gate
# Usage: bash benchmark/check-context-quality.sh [--verbose]
# Runs 8 NL-query tuples against indexed corpora and checks that the expected
# anchor symbol appears in the context output.
# Pass criterion: ≥6/8 queries include the expected symbol.

set -euo pipefail

BINARY="${BINARY:-/root/develop/code-wiki/target/release/codewiki}"
CORPUS_ROOT="${CORPUS_ROOT:-/root/bench-corpus}"
VERBOSE="${1:-}"

pass=0
fail=0

check() {
    local repo="$1"
    local query="$2"
    local expected="$3"
    local path="${CORPUS_ROOT}/${repo}"

    if [[ ! -d "$path/.codewiki" ]]; then
        echo "SKIP  [$repo] no .codewiki index at $path"
        return
    fi

    local output
    output=$("$BINARY" context --path "$path" "$query" 2>/dev/null || true)

    if echo "$output" | grep -qi "$expected"; then
        echo "PASS  [$repo] '$query' → found '$expected'"
        ((pass++)) || true
    else
        echo "FAIL  [$repo] '$query' → missing '$expected'"
        if [[ "$VERBOSE" == "--verbose" ]]; then
            echo "--- output ---"
            echo "$output" | head -40
            echo "---"
        fi
        ((fail++)) || true
    fi
}

echo "=== B5 Context Quality Check ==="
echo "Binary: $BINARY"
echo "Corpus: $CORPUS_ROOT"
echo ""

# 8 baseline tuples: (repo, NL-query, anchor-symbol)
check "django"   "how does request routing work"              "URLResolver"
check "django"   "how are migrations applied"                  "MigrationExecutor"
check "tokio"    "how does the async runtime schedule tasks"   "spawn"
check "zod"      "how does schema validation work"             "ZodType"
check "vuecore"  "how does component reactivity work"          "ReactiveEffect"
check "django"   "MigrationExecutor apply_migration"           "apply_migration"
check "zod"      "ZodType parse safeParse"                     "ZodType"
check "vuecore"  "effect track reactive ref"                   "ReactiveEffect"

echo ""
echo "=== Results: $pass pass, $fail fail out of $((pass+fail)) ==="

if (( pass >= 6 )); then
    echo "GATE: PASS (≥6/8)"
    exit 0
else
    echo "GATE: FAIL (<6/8)"
    exit 1
fi
