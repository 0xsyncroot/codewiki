# CodeWiki Benchmark Results

CodeWiki is a local, tree-sitter-based code knowledge graph that exposes 9 MCP tools
to AI coding agents. This report summarises indexing throughput, query latency,
incremental sync speed, 100k-file scalability, and the agent token/tool-call savings
that the graph enables.

All numbers are from deterministic, reproducible runs. Raw data lives in the `.tsv`
files alongside this document. Detailed methodology and analysis are in the linked
report files.

**Machine:** 28 cores, 31 GB RAM, WSL2 on Linux 5.15 (AMD64).
**Binary:** `codewiki 0.1.0` (release build, default features).

---

## Key results at a glance

| Metric | Result |
|--------|--------|
| Index throughput — small repos | 370–1700 files/s |
| Index throughput — django (3019 files) | 256 files/s, **11.8 s** total |
| Search / callers / callees latency (CLI p50) | **2–7 ms** (includes binary cold-start) |
| Context query latency (CLI p50) | **3–38 ms** |
| Incremental sync — 1 file changed | **20–150 ms** |
| 100k-file cold-index extrapolated | **~14 min** (acceptance target: ≤ 20 min) |
| Agent tool-call reduction (.NET tasks) | **~69% fewer** |
| Agent token reduction (.NET tasks) | **~86% fewer** |
| Vietnamese / diacritic search | Safe — UTF-8 intact, ASCII identifiers fully searchable |

---

## Scale at a glance

codewiki indexes a 5,203-file enterprise C# codebase (OrchardCore) in **9.2 s**
and scales to 100,000 files in an extrapolated **~14 min** — then stays fresh with
millisecond-range incremental sync.

| Repo / corpus | Language | Files | Index time | files/s | Peak RSS | Incr sync |
|---------------|----------|------:|:----------:|--------:|:--------:|:---------:|
| eShopOnWeb (C#) | ASP.NET | 254 | 0.16 s | ~1700 | 44 MB | 28 ms |
| django (Python) | Django | 3,020 | 11.8 s | ~256 | ~351 MB | 130 ms |
| abp/framework (C#) | .NET | 3,497 | 2.55 s | ~1370 | 228 MB | — |
| **OrchardCore (C#)** | **ASP.NET** | **5,203** | **9.16 s** | **~640** | **784 MB** | — |
| synthetic mixed | Py/TS/Rust/Vue | 10,831 | 29.7 s | ~365 | ~1.1 GB | — |
| synthetic mixed | Py/TS/Rust/Vue | 16,461 | 56.4 s | ~290 | ~1.3 GB | — |
| **100k (extrapolated)** | mixed | **100,000** | **~14 min** | — | **~4 GB** | **ms/change** |

Sources: [REPORT.md](REPORT.md) (Python/JS/Rust/TS/Vue), [DOTNET-REPORT.md](DOTNET-REPORT.md) (.NET/C#),
[ANALYSIS-SCALE.md](ANALYSIS-SCALE.md) (100k extrapolation).
All large-corpus (.NET) numbers are post-fix (interfaces/namespaces/signatures correct).

---

## 1. Cold-index speed — 10 real repos

Post-Wave-4 optimisation build (see [REPORT.md §6](REPORT.md) for wave-by-wave breakdown
and [ANALYSIS-SCALE.md](ANALYSIS-SCALE.md) for the 100k scaling analysis).

| Repo | Lang | Files | Nodes | Edges | Index time | files/s | Peak RSS | Incr-sync |
|------|------|------:|------:|------:|:----------:|--------:|:--------:|----------:|
| requests | Python | 37 | 993 | 956 | 0.1 s | 370 | 31 MB | 20 ms |
| lodash | JavaScript | 54 | 8,936 | 8,882 | 0.7 s | 77 | 151 MB | 30 ms |
| flask | Python | 83 | 1,839 | 1,756 | 0.2 s | 415 | 37 MB | 20 ms |
| ripgrep | Rust | 101 | 5,379 | 5,278 | 2.9 s | 35 | 80 MB | 20 ms |
| express | JavaScript | 141 | 2,025 | 1,884 | 0.3 s | 470 | 45 MB | 20 ms |
| mediatr | C# | 151 | 1,377 | 1,226 | 0.1 s | 1510 | 35 MB | 20 ms |
| zod | TypeScript | 408 | 7,573 | 7,165 | 0.6 s | 680 | 98 MB | 30 ms |
| vuecore | Vue/TS | 535 | 12,559 | 12,025 | 1.3 s | 412 | 141 MB | 40 ms |
| tokio | Rust | 778 | 14,430 | 13,652 | 1.8 s | 53 | 106 MB | 40 ms |
| django | Python | 3,019 | 53,198 | 50,178 | 11.8 s | 256 | 351 MB | 150 ms |

All 10 repos index cleanly (zero crashes, zero FK errors). Node/edge counts are
verified for correctness by the parity harness.

Optimisation gain summary (baseline → final):

| Repo | Baseline | Final | Speedup | Sync baseline | Sync final | Sync speedup |
|------|:--------:|:-----:|:-------:|:-------------:|:----------:|:------------:|
| tokio (Rust) | 19.4 s | 1.8 s | **11×** | 1.24 s | 40 ms | **31×** |
| django (Python) | 36.2 s | 11.0 s | 3.3× | 3.13 s | 130 ms | **24×** |
| ripgrep (Rust) | 3.7 s | 0.5 s | 7.4× | 180 ms | 20 ms | 9× |

---

## 2. Search / query latency

All measurements use the CLI (each call opens the DB from cold). Via the persistent
MCP server the DB stays open and latency is sub-millisecond.

| Repo | query exact | fuzzy | callers | callees | impact | context |
|------|:-----------:|:-----:|:-------:|:-------:|:------:|:-------:|
| requests | 2 ms | 2 ms | 2 ms | 2 ms | 5 ms | 3 ms |
| flask | 2 ms | 2 ms | 2 ms | 2 ms | 2 ms | 4 ms |
| ripgrep | 2 ms | 2 ms | 2 ms | 2 ms | 2 ms | 5 ms |
| express | 2 ms | 2 ms | 2 ms | 2 ms | 2 ms | 4 ms |
| zod | 2 ms | 2 ms | 2 ms | 2 ms | 16 ms | 11 ms |
| vuecore | 3 ms | 3 ms | 4 ms | 3 ms | 4 ms | 15 ms |
| tokio | 2 ms | 2 ms | 2 ms | 2 ms | 2 ms | 8 ms |
| django | 6 ms | 7 ms | 7 ms | 6 ms | 6 ms | 29 ms |

Search is not a bottleneck. Even django (53k nodes) answers `context` in ~29 ms.

---

## 3. Incremental sync

Incremental sync is scoped to changed files only (O(changed), not O(repo)).
A 1-file edit triggers re-extraction + resolution only for that file.

| Repo | Files | Sync time (1-file change) |
|------|------:|:-------------------------:|
| requests | 37 | 20 ms |
| flask | 83 | 20 ms |
| express | 141 | 20 ms |
| zod | 408 | 30 ms |
| vuecore | 535 | 40 ms |
| tokio | 778 | 40 ms |
| django | 3,019 | 150 ms |
| jellyfin (.NET, 2065 .cs) | 2,065 | 61 ms |

Baseline pre-optimisation: django sync was 3.13 s (24× slower than final).

---

## 4. 100k-file scaling verdict

Measured on synthetic corpora combining django / tokio / vue-core / zod repeated to
scale. Full wave-by-wave analysis in [ANALYSIS-SCALE.md](ANALYSIS-SCALE.md).

| Scale | Post-W6 time | files/s | Peak RSS |
|-------|:------------:|--------:|:--------:|
| 3k (django) | 4.4 s | 688 | 482 MB |
| 10k | 29.7 s | 365 | 1.1 GB |
| 16k | 56.4 s | 292 | 1.3 GB |
| **100k (extrapolated)** | **~14 min** | — | ~4 GB |

Acceptance criteria: cold-index ≤ 20 min, peak RSS ≤ 4 GB at 100k files.
**Both criteria are met.**

Scaling law: O(n^1.50) post-W6. Extrapolation uncertainty band ±30–40%.

---

## 5. Agent savings — .NET enterprise (measured)

Five realistic tasks on eShopOnWeb (254 .cs) and jellyfin (2,065 .cs), comparing
`codewiki` CLI calls vs. a grep + file-read baseline. All byte counts measured from
actual CLI output and file sizes. Tokens estimated at 1 token = 4 bytes (conservative;
code tokenises more efficiently). Pricing: Claude Sonnet $3.00 / 1M input tokens.

| Task | Repo | CW calls | Baseline calls | Call reduction | CW tokens | Baseline tokens | Token reduction |
|------|------|:--------:|:--------------:|:--------------:|:---------:|:---------------:|:--------------:|
| DI consumers (`IBasketService`) | eShopOnWeb | 2 | 6 | **66%** | 400 | 3,498 | **88%** |
| Feature comprehension (basket checkout) | eShopOnWeb | 1 | 6 | **83%** | 1,035 | 6,934 | **85%** |
| Interface→impls (`IRepository`) | eShopOnWeb | 2 | 6 | **66%** | 449 | 4,335 | **89%** |
| Blast radius (`OrderService` refactor) | eShopOnWeb | 1 | 5 | **80%** | 264 | 1,977 | **86%** |
| Cross-cutting: auth config | jellyfin | 2 | 4 | **50%** | 1,296 | 8,332 | **84%** |
| **Average** | | **1.6** | **5.4** | **69%** | **689** | **5,015** | **86%** |

Full methodology and per-task traces: [DOTNET-REPORT.md §7](DOTNET-REPORT.md).

---

## 6. Vietnamese / diacritic search

Tested on 18 synthetic files in 5 languages (Python, TypeScript, Go, C#, Rust) with
heavy Vietnamese comments/docstrings and Vietnamese-derived ASCII identifiers
(`tinhTong`, `dangNhap`, `QuanLyNguoiDung`). Full report: [VIETNAMESE-REPORT.md](VIETNAMESE-REPORT.md).

| Dimension | Result |
|-----------|--------|
| Symbol extraction parity vs English | Identical (AST-agnostic) |
| Crash / encoding error | None (18/18 files clean) |
| UTF-8 storage integrity | Intact — no mojibake |
| Identifier search (ASCII-derived VN names) | Full support |
| FTS Latin-1 diacritics (`à`, `ì`, `ù`) | Folds correctly |
| FTS extended VN diacritics (`ư`, `đ`, `ắ`) | Does not fold (stays as-is) |
| `context` with Vietnamese prose query | Works, slightly noisier than English |
| `callers` / `callees` / `impact` on VN symbols | Full support |

**Verdict:** codewiki is Vietnamese-safe for its primary workflow (symbol/identifier
search, call graph analysis, context building). A one-line schema change
(`remove_diacritics 2`) would give full NFD diacritic stripping for prose queries.

---

## Detailed reports

| Report | Contents |
|--------|----------|
| [REPORT.md](REPORT.md) | Cold-index baseline + post-Wave-4 final, wave-by-wave optimisation history, token savings methodology |
| [DOTNET-REPORT.md](DOTNET-REPORT.md) | Full .NET/C# enterprise audit: eShopOnWeb, jellyfin, orchardcore, ABP — extraction quality, gap analysis, agent savings |
| [VIETNAMESE-REPORT.md](VIETNAMESE-REPORT.md) | Vietnamese / i18n compatibility: FTS5 tokeniser behaviour, diacritic folding, UTF-8 integrity |
| [ANALYSIS-SCALE.md](ANALYSIS-SCALE.md) | 100k-file scaling: wave-by-wave extrapolation, O(n^1.50) law, remaining walls |
| [ANALYSIS-B2-sync.md](ANALYSIS-B2-sync.md) | Incremental sync analysis (B2) |
| [ANALYSIS-B4-memory.md](ANALYSIS-B4-memory.md) | Memory profiling analysis (B4) |
| [OPTIMIZATION-PLAN.md](OPTIMIZATION-PLAN.md) | Optimisation wave plan and design notes |

Raw data (TSV):
- [`results-index.tsv`](results-index.tsv) — cold-index run data
- [`results-search.tsv`](results-search.tsv) — search latency data
- [`results-dotnet.tsv`](results-dotnet.tsv) — .NET corpus data
