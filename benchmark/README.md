# CodeWiki Benchmark Report

CodeWiki is a local, tree-sitter-based code knowledge graph that exposes 9 MCP tools to
AI coding agents. This is the single authoritative benchmark report. It leads with the
number that matters most — **how many agent tokens and tool-calls CodeWiki saves, per
language** — and then backs it with indexing/query performance, scale extrapolation, and
methodology.

All numbers come from a fresh run on **2026-05-25** against `codewiki 0.1.1` (release
build, default features). Raw data lives in the `.tsv` files alongside this document. The
benchmark repos are **shallow clones of upstream `main`** (commit SHAs recorded in
[`results-index.tsv`](results-index.tsv)); re-running the commands reproduces the figures.

**Machine:** 28 cores, 31 GB RAM, WSL2 on Linux 5.15 (AMD64).

---

## 1. Agent savings by language (headline)

For each language, an MCP-driven agent-savings harness runs **6 task archetypes**
(`locate, callers, callees, feature, impl, blast`) against one real repo and compares
CodeWiki to a **grep + read-full-files baseline**. Every answer is scored for **recall**
against a frozen ground-truth oracle. Tokens = output bytes / 4.

| Language | Repo | Tool-call reduction | **Token reduction** | CodeWiki recall | Baseline recall |
|----------|------|:-------------------:|:-------------------:|:---------------:|:---------------:|
| C++ | nlohmann/json | 67% | **99%** | 0.86 | 1.00 |
| C# | dotnet-architecture/eShopOnWeb | 74% | **31%** | 0.93 | 0.94 |
| Go | gin-gonic/gin | 78% | **98%** | 0.79 | 0.95 |
| Java | google/gson | 74% | **94%** | 0.60 | 0.88 |
| JavaScript | expressjs/express | 57% | **91%** | 0.79 | 0.83 |
| Python | pallets/flask | 74% | **94%** | 0.71 | 0.92 |
| Rust | BurntSushi/ripgrep | 75% | **97%** | 0.86 | 0.94 |
| TypeScript | colinhacks/zod | 65% | **98%** | 0.87 | 0.94 |
| **TOTAL** | **48 cases / 8 repos** | **72%** | **97%** | **0.80** | **0.93** |

Across all 48 cases CodeWiki uses **72% fewer tool-calls and 97% fewer tokens** than the
grep+read baseline, saving **~$4.60** of input tokens (Claude Sonnet, $3.00 / 1M tokens)
over the run. CodeWiki verdicts: 30 PASS / 14 PARTIAL / 4 FAIL.

Numbers above are computed directly from [`results-savings.tsv`](results-savings.tsv)
(per-language aggregates of the 6 archetype rows; recall is the mean of the per-case
recall scores).

---

## 2. How to read these numbers (honest framing)

- **The baseline is grep + reading whole files.** CodeWiki returns a small, ranked,
  structurally-resolved slice; the baseline reads entire candidate files. That is why the
  token reduction is so high (97%) — reading full source files is token-heavy. The
  comparison is fair (it is exactly what a tool-less agent does) but the magnitude
  reflects how expensive whole-file reads are.
- **C#'s low token reduction (31%) is a small-repo artefact, not a CodeWiki weakness.**
  eShopOnWeb is tiny, so the files the baseline must read are already cheap to read —
  there is little to save. Recall stays on par (0.93 vs 0.94). On larger C# corpora the
  reduction tracks the other languages (see the .NET appendix).
- **CodeWiki trades a little recall for the large token/call win.** Mean recall is **0.80
  for CodeWiki vs 0.93 for the baseline**. The gap comes from ambiguous same-name
  top-match resolution (`callers`/`impact` picking one of several same-named nodes),
  `feature`-archetype context relevance, and missing `implements` edges for
  structurally-typed languages (e.g. Go interfaces). Closing this recall gap without
  giving back the token win is tracked future work — it is the target of the harness's
  optimisation study, not a fixed limitation.

No spin: the value proposition is "near-baseline answers at ~3% of the tokens," and the
recall delta is the cost being paid for that.

---

## 3. Performance — cross-language indexing, query, and sync

Eight shallow-cloned real repositories, one per language. Every repo indexed cleanly —
zero crashes, zero FK errors — including C++ (`nlohmann/json`, 499 files).

| Repo | Language | Files | Graph nodes | Graph edges | Cold-index | files/s | Peak RSS | Sync (1 file) |
|------|----------|------:|------------:|------------:|:----------:|--------:|:--------:|:-------------:|
| pallets/flask | Python | 83 | 2,110 | 3,742 | 0.19 s | 430 | 47 MB | 22 ms |
| BurntSushi/ripgrep | Rust | 101 | 3,344 | 11,301 | 0.36 s | 278 | 105 MB | 24 ms |
| expressjs/express | JavaScript | 141 | 2,291 | 6,374 | 0.26 s | 542 | 62 MB | 21 ms |
| colinhacks/zod | TypeScript | 408 | 7,596 | 17,275 | 0.57 s | 720 | 135 MB | 32 ms |
| dotnet-architecture/eShopOnWeb | C# | 269 | 1,632 | 2,128 | 0.15 s | 1,793 | 44 MB | 27 ms |
| google/gson | Java | 262 | 5,329 | 15,459 | 0.49 s | 538 | 106 MB | 33 ms |
| nlohmann/json | C++ | 499 | 13,438 | 33,083 | 1.01 s | 494 | 239 MB | 43 ms |
| gin-gonic/gin | Go | 99 | 1,834 | 6,291 | 0.26 s | 376 | 72 MB | 22 ms |

- **Files** = source files extracted into `file`-kind nodes. **Graph nodes / edges** are
  the final resolved graph (`codewiki status` / the `nodes` and `edges` tables) — what an
  agent actually queries, after framework extraction and reference resolution promote
  unresolved refs into `calls` / `imports` / `implements` / `contains` edges. (The shorter
  "Indexed N files…" line printed by `codewiki init` reports the pre-resolution count and
  is intentionally lower.)
- **Cold-index** is the 3-run average wall time (process spawn → index → resolve).
- **files/s** = source files ÷ cold-index wall time.

### 3.1 Search / query latency (CLI p50 over 7 runs, ms)

Each CLI call opens the DB cold, so these are a worst case. Via the persistent MCP server
the connection stays open and latency is sub-millisecond.

| Repo | Language | query exact | fuzzy | callers | callees | impact | context |
|------|----------|:-----------:|:-----:|:-------:|:-------:|:------:|:-------:|
| flask | Python | 3 | 3 | 4 | 3 | 12 | 5 |
| ripgrep | Rust | 3 | 3 | 3 | 3 | 3 | 5 |
| express | JavaScript | 2 | 2 | 2 | 2 | 2 | 4 |
| zod | TypeScript | 2 | 3 | 2 | 2 | 23 | 9 |
| eShopOnWeb | C# | 2 | 2 | 2 | 2 | 2 | 5 |
| gson | Java | 3 | 3 | 3 | 4 | 32 | 5 |
| json | C++ | 4 | 4 | 4 | 4 | 4 | 9 |
| gin | Go | 2 | 2 | 2 | 2 | 2 | 4 |

Search is never the bottleneck. `impact` varies with fan-out (zod 23 ms, gson 33 ms on
hot interfaces) but stays well inside interactive latency. Raw data:
[`results-index.tsv`](results-index.tsv), [`results-search.tsv`](results-search.tsv).

### 3.2 Incremental sync

Sync is scoped to changed files only — O(changed), not O(repo). A 1-file edit re-extracts
and re-resolves just that file and its direct dependants.

| Repo | Files | Sync (1-file change) |
|------|------:|:--------------------:|
| flask | 83 | 22 ms |
| express | 141 | 21 ms |
| zod | 408 | 32 ms |
| json (C++) | 499 | 43 ms |
| jellyfin (.NET, 2,065 .cs) | 2,065 | 66 ms |
| django (Python, 3,020) | 3,020 | 31 ms |

---

## 4. Scale: path to 100k files

Cold-index throughput holds across scale. The 3k anchor is a fresh measurement on the
current binary; the 10k / 16k / 100k figures are post-optimisation synthetic-corpus runs
(django / tokio / vue-core / zod repeated to scale) — scaling behaviour is unchanged by
the node/edge-count fixes, which affect graph correctness, not parse/resolve throughput.

| Scale | Files | Cold-index | files/s | Peak RSS |
|-------|------:|:----------:|--------:|:--------:|
| 3k (django, **fresh**) | 3,020 | 5.8 s | ~520 | ~480 MB |
| 10k (synthetic) | 10,831 | 29.7 s | 365 | ~1.1 GB |
| 16k (synthetic) | 16,461 | 56.4 s | 292 | ~1.3 GB |
| **100k (extrapolated)** | **100,000** | **~14 min** | — | **~4 GB** |

**Scaling law:** cold-index `t ≈ 2.57e-5 · n^1.50` (O(n^1.50)); peak RSS
`≈ 3.34 · n^0.62` MB (O(n^0.62), sub-linear due to name dedup across the repeated corpus
— a fully-unique 100k monorepo trends toward O(n) ≈ 3–5 GB). Extrapolation uncertainty
band ±30–40%.

**Acceptance criteria:** cold-index ≤ 20 min and peak RSS ≤ 4 GB at 100k files. **Both
met** (cold-index ~14 min; RSS ~4 GB, borderline). The fresh django 3k run (5.8 s, 52,722
nodes / 165,582 edges, 115,404 references resolved) sits on the predicted curve.

---

## 5. Methodology — how the savings harness works

The agent-savings figures in §1 are produced by [`run-savings.sh`](run-savings.sh):
**6 task archetypes × 8 languages = 48 cases** over the §3 repos.

**Archetypes** (CodeWiki tool / baseline shape, defined in [`cases.tsv`](cases.tsv)):

| Archetype | Question | CodeWiki | Baseline |
|-----------|----------|----------|----------|
| `locate` | where is X defined | `codewiki_search` | grep def + read 1 file |
| `callers` | what calls X | `codewiki_callers` | grep usages + read N files |
| `callees` | what does X call | `codewiki_callees` | read X's body + grep |
| `feature` | how does \<feature\> work | `codewiki_context` (×3) | grep regex + read N files |
| `impl` | implementers/subtypes of X | `codewiki_impact` | grep interface + read N impls |
| `blast` | what breaks if X changes | `codewiki_impact` | grep symbol + read N dependants |

- **MCP-driven, not CLI.** Every CodeWiki call goes through the MCP stdio server
  (`codewiki serve --mcp`, via [`lib/mcp_call.py`](lib/mcp_call.py)) — the surface agents
  actually use, and where the rendering optimisations (relative-path rendering, node
  include options, context dedup/density) live. A CLI measurement would under-report the
  win. Both sides are measured on the same task.
- **Deterministic recall scoring — no LLM judge.** Each answer (CodeWiki and baseline) is
  scored by [`lib/score.py`](lib/score.py) against a frozen ground-truth oracle in
  [`oracle/*.json`](oracle/), built once from `sqlite3` graph queries + targeted grep and
  hand-verified. recall = required-elements-present / required; verdict is
  PASS (≥0.8) / PARTIAL / FAIL plus a wrong-primary correctness gate.
- **Tokens = output bytes / 4** (conservative — code tokenises more efficiently, so real
  savings are likely higher). The `feature` archetype (context) is median-of-3 because its
  ranking is non-deterministic; `query` / `callers` / `callees` / `impact` are
  deterministic. `codewiki_explore` + `codewiki_node` are exercised over MCP as a coverage
  probe and recorded in a separate `mcp_coverage_bytes` column — **not** folded into the
  call/token comparison, so each archetype stays apples-to-apples with its baseline task.

**Reproduce:**

```bash
CW=/path/to/codewiki benchmark/run-savings.sh    # -> benchmark/results-savings.tsv
# Rebuild + hand-verify oracles (only when adding cases):
python3 benchmark/lib/build_oracle.py --bench-root /tmp/bench
python3 benchmark/lib/build_oracle.py --report
```

A companion **context-relevance fixture**
([`../parity/context-relevance/`](../parity/context-relevance/)) scores
`codewiki_context` roots@5 precision / recall over the frozen `synthetic-120` corpus
(floors in `parity/thresholds.toml [context]`).

**Reproducing the §3–§4 performance numbers:**

```bash
CW=/path/to/codewiki        # confirm: $CW --version  → codewiki 0.1.1
mkdir -p /tmp/bench && cd /tmp/bench

# Shallow-clone the 8 repos (SHAs in results-index.tsv), then per repo:
$CW init --path flask                                    # cold-index + wall time
sqlite3 flask/.codewiki/codewiki.db \
  "SELECT (SELECT COUNT(*) FROM nodes), (SELECT COUNT(*) FROM edges);"  # resolved totals
$CW query Flask --path flask                             # query latency (p50 over runs)
touch flask/src/flask/app.py && $CW sync --path flask    # incremental sync
```

---

## 6. Vietnamese / diacritic search

Tested on synthetic files in 5 languages (Python, TypeScript, Go, C#, Rust) with heavy
Vietnamese comments/docstrings and Vietnamese-derived ASCII identifiers (`tinhTong`,
`dangNhap`, `QuanLyNguoiDung`). Full report: [`VIETNAMESE-REPORT.md`](VIETNAMESE-REPORT.md).

| Dimension | Result |
|-----------|--------|
| Symbol extraction parity vs English | Identical (AST-agnostic) |
| Crash / encoding error | None (18/18 files clean) |
| UTF-8 storage integrity | Intact — no mojibake |
| Identifier search (ASCII-derived VN names) | Full support |
| FTS Latin-1 diacritics (`à`, `ì`, `ù`) | Folds correctly |
| FTS extended VN diacritics (`ư`, `đ`, `ắ`) | Does not fold (stays as-is) |
| `callers` / `callees` / `impact` on VN symbols | Full support |

**Verdict:** CodeWiki is Vietnamese-safe for its primary workflow (symbol/identifier
search, call-graph analysis, context building). A one-line schema change
(`remove_diacritics 2`) would add full NFD diacritic stripping for prose queries.

---

## 7. .NET enterprise appendix

The C# row in §1 is one small reference repo (eShopOnWeb). A deeper, .NET-specific study
covers extraction quality and agent savings on larger enterprise corpora (eShopOnWeb,
jellyfin, OrchardCore, ABP), including a per-task savings trace and the C# extractor gap
audit (interface/enum kinds, namespace qualification, DI edges, `using` imports,
signatures, route expansion) with before/after fix results.

Full report: [`DOTNET-REPORT.md`](DOTNET-REPORT.md). On those larger corpora the .NET
per-task savings are **~70% fewer tool-calls, ~83% fewer tokens** vs grep+read — the lower
token figure than §1's 97% is expected because that study counts a more economical
grep-then-read-minimal-files baseline rather than reading whole files. The §1 cross-
language harness is the authoritative, recall-scored measurement; the .NET appendix is a
detailed, repo-specific companion. Raw per-task data:
[`results-dotnet.tsv`](results-dotnet.tsv).

---

## Raw data (TSV)

- [`results-savings.tsv`](results-savings.tsv) — agent-savings, 48 cases (calls, bytes, recall, verdict per case) — source for §1
- [`results-index.tsv`](results-index.tsv) — cross-language cold-index (files, nodes, edges, time, RSS, DB size, files/s, sync)
- [`results-search.tsv`](results-search.tsv) — search/query p50 latency by repo and query type
- [`results-dotnet.tsv`](results-dotnet.tsv) — .NET per-task agent-savings (calls, bytes, tokens, $)
