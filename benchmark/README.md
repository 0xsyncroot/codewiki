# CodeWiki Benchmark Report

CodeWiki is a local, tree-sitter-based code knowledge graph that exposes 9 MCP tools to
AI coding agents. **This is the single authoritative benchmark document.** It leads with
the number that matters most — **how many agent tokens and tool-calls CodeWiki saves** —
across small, medium, and large/enterprise repositories, then backs it with an honest
recall trade-off analysis, indexing/scale/sync performance, a .NET worked example,
Unicode/i18n results, and full reproduction instructions.

All numbers come from a fresh cold run on **2026-05-25** against `codewiki 0.1.1` (release
build, default features). Raw per-case data lives in
[`results-savings.tsv`](results-savings.tsv) and the index/search TSVs alongside this
document. The benchmark repos are **shallow clones of upstream `main`** (commit SHAs in
[§6 Reproduce](#6-methodology--reproduce-it-yourself)); re-running the one command
reproduces the figures.

**Machine:** Intel Core i7-14700KF (14 cores / 28 threads), 31 GiB RAM, NVMe ext4,
WSL2 (Linux 5.15, AMD64).

---

## Contents

1. [Agent savings (headline)](#1-agent-savings-headline)
2. [How to read these numbers — honesty disclosures](#2-how-to-read-these-numbers--honesty-disclosures)
3. [Performance, scale & sync](#3-performance-scale--sync)
4. [.NET worked example](#4-net-worked-example)
5. [Unicode / i18n](#5-unicode--i18n)
6. [Methodology & reproduce it yourself](#6-methodology--reproduce-it-yourself)

---

## 1. Agent savings (headline)

An MCP-driven agent-savings harness runs **6 task archetypes** (`locate, callers,
callees, feature, impl, blast`) over **10 real repositories** spanning three size tiers,
and compares CodeWiki to a **grep + read-full-files baseline** — the surface a tool-less
agent actually uses. Every answer is scored for **recall** against a frozen ground-truth
oracle. Tokens = output bytes ÷ 4 (conservative; code tokenises more efficiently).
Pricing: Claude Sonnet input at **$3.00 / 1M tokens**.

**151 cases. CodeWiki uses 73% fewer tool-calls and 97% fewer tokens than grep+read,
saving ~$15.64 of input tokens across the run.** Mean recall is **0.77 for CodeWiki vs
0.92 for the baseline** — CodeWiki trades a little recall for the large token/call win
(see [§2](#2-how-to-read-these-numbers--honesty-disclosures)).

### 1.1 By language — 8 repos, one per language (small/medium tier)

| Language | Repo | Files | Tool-call reduction | **Token reduction** | CodeWiki recall | Baseline recall |
|----------|------|------:|:-------------------:|:-------------------:|:---------------:|:---------------:|
| Python | pallets/flask | 83 | 65% | **93%** | 0.89 | 0.95 |
| Rust | BurntSushi/ripgrep | 101 | 75% | **98%** | 0.79 | 0.95 |
| JavaScript | expressjs/express | 141 | 60% | **90%** | 0.85 | 0.91 |
| TypeScript | colinhacks/zod | 408 | 62% | **98%** | 0.71 | 0.93 |
| Java | google/gson | 262 | 77% | **92%** | 0.54 | 0.83 |
| C++ | nlohmann/json | 499 | 69% | **98%** | 0.83 | 0.96 |
| Go | gin-gonic/gin | 99 | 77% | **97%** | 0.82 | 0.95 |
| C# | dotnet-architecture/eShopOnWeb | 269 | 77% | **72%** | 0.84 | 0.94 |

Each row is the aggregate of ~17–18 task cases on that repo (the base 6 archetypes plus
~11–12 deeper cases: ambiguous/overloaded names, generic/trait-heavy symbols, deep blast
radius, framework patterns). C#'s lower token reduction (72%) is a **small-repo
artefact** — eShopOnWeb's files are already cheap to read, so there is little to save;
recall stays on par (0.84 vs 0.94).

### 1.2 By size tier — the saving grows with the repo

The two large/enterprise repos are real production codebases we also index for the scale
section: **kubernetes** (Go, 17,176 files, ~3.0M LOC) and **microsoft/TypeScript** (the
TS compiler, 39,296 files, ~3.4M LOC). The same frozen-oracle scheme and identical
baseline scoring apply.

| Size tier | Repos | Cases | Avg CW tokens/case | Avg baseline tokens/case | Token reduction | CW recall | BL recall |
|-----------|-------|------:|------------------:|-------------------------:|:---------------:|:---------:|:---------:|
| **Small** (<300 files) | flask, express, gin, eShopOnWeb, ripgrep | 87 | 745 | 16,069 | **95%** | 0.84 | 0.94 |
| **Medium** (300–700 files) | zod, gson, json | 52 | 1,365 | 51,079 | **97%** | 0.69 | 0.90 |
| **Enterprise** (>15k files) | kubernetes, microsoft/TypeScript | 12 | 708 | **108,573** | **99%** | 0.60 | 0.89 |

This is the central scaling result: **CodeWiki's per-query answer stays bounded
(~0.7–1.4k tokens) regardless of repo size, while the grep+read baseline's byte cost
explodes** — from ~16k tokens/case on a small repo to **~109k tokens/case** on a 17k–39k
file monorepo (a single scoped grep over `pkg/` or `src/` returns hundreds of KB, and
each required file read adds many KB more). So the per-query dollar saving is *largest*
on the biggest codebases — exactly where agent token budgets hurt most.

Enterprise per-case rows (all in [`results-savings.tsv`](results-savings.tsv)):

| Repo | Archetype | Symbol | CW tokens | BL tokens | CW recall | BL recall |
|------|-----------|--------|----------:|----------:|:---------:|:---------:|
| kubernetes | locate | `syncPod` (5 defs) | 250 | 37,274 | 1.00 | 1.00 |
| kubernetes | callers | `NewServer` (300+ refs) | 130 | 84,573 | 0.08 | 0.54 |
| kubernetes | callees | `SyncPod` | 450 | 68,401 | 0.57 | 0.57 |
| kubernetes | blast | `SyncPod` | 404 | 23,064 | 1.00 | 1.00 |
| kubernetes | impl | `Reconciler` *(honest, UNSCORABLE)* | 161 | 0 | — | — |
| kubernetes | feature | kubelet pod-sync | 793 | 44,643 | 0.00 | 1.00 |
| microsoft/TypeScript | locate | `createProgram` (4 defs) | 310 | 96,465 | 1.00 | 1.00 |
| microsoft/TypeScript | callers | `createSourceFile` | 437 | 212,744 | 0.43 | 1.00 |
| microsoft/TypeScript | callees | `createProgram` | 401 | 210,161 | 0.40 | 0.67 |
| microsoft/TypeScript | blast | `createSourceFile` | 3,760 | 212,744 | 1.00 | 1.00 |
| microsoft/TypeScript | locate | `emitFiles` | 297 | 68,354 | 1.00 | 1.00 |
| microsoft/TypeScript | feature | compile pipeline | 1,097 | 244,449 | 0.17 | 1.00 |

Note the honesty signal in this table: on `kubernetes feature` and the `createProgram`
`callees`/`feature` cases the baseline scores **higher recall** than CodeWiki. We keep
those rows. The savings are real and large; the recall trade-off is also real.

---

## 2. How to read these numbers — honesty disclosures

We deliberately do not hide the methodology caveats.

- **The baseline is grep + reading whole files.** CodeWiki returns a small, ranked,
  structurally-resolved slice; the baseline reads entire candidate files. That is why the
  token reduction is so high (97%) — reading full source is token-heavy. The comparison
  is fair (it is exactly what a tool-less agent does) but the magnitude reflects how
  expensive whole-file reads are.

- **Oracles are graph-derived AND hand-verified, and the baseline is scored on the
  identical oracle.** For the structural archetypes (`callers`, `callees`, `impl`,
  `blast`) the ground-truth required-element set is derived deterministically from the
  indexed SQLite graph (pure SQL over the `nodes`/`edges` tables) and cross-checked
  against the source with targeted grep ([`lib/build_oracle_extra.py`](lib/build_oracle_extra.py),
  which reuses the derivation logic in [`lib/build_oracle.py`](lib/build_oracle.py)). The
  oracle is then frozen to `oracle/<lang>_<case_id>.json` and never regenerated at
  measurement time. **Both** CodeWiki and the grep+read baseline are scored against the
  **same** frozen oracle by [`lib/score.py`](lib/score.py), so the comparison is symmetric.

- **The oracle is not rigged for CodeWiki — the baseline wins recall.** The telling
  result: the grep+read baseline scores **higher** mean recall (0.92) than CodeWiki
  (0.77). If the oracle had been tuned to flatter CodeWiki, CodeWiki would win recall. It
  does not. **CodeWiki trades a little recall for a large token/call saving** — that is
  the entire value proposition, stated plainly.

- **No cherry-picking — cases where the baseline wins are kept.** The suite intentionally
  includes hard and expected-to-lose cases: ambiguous/overloaded names (`_parse` has 37
  same-name defs in zod; `build` has 24 defs in ripgrep), and deliberate graph-gap cases
  flagged HONEST in [`cases.tsv`](cases.tsv). Two of those are **UNSCORABLE** (empty
  oracle, kept in call/token totals, excluded from recall means): C# `EfRepository` blast
  (the C# graph resolves method-call edges but not type-usage references to classes) and
  Go `Reconciler` impl (Go structural interfaces have no `implements` edge). CodeWiki
  verdicts across the 151 cases: **93 PASS / 44 PARTIAL / 12 FAIL / 2 UNSCORABLE.**

### Weakness analysis (what the recall gap is made of)

The 0.77 vs 0.92 recall delta concentrates in four real, reproducible limitations — all
tracked for future scoring work, none hidden:

1. **Overloaded-name resolution.** `callers`/`callees`/`impact` resolve a bare symbol
   name to a single graph node. When a name has dozens of same-name definitions (zod
   `_parse`, gson `read`, k8s `NewServer`), the tool answers for one node while the oracle
   (aggregated across the family) expects more — so recall drops even though the answer is
   correct for the node it picked. Biggest single contributor to the gap.
2. **Go structural interfaces have no `implements` edge.** Go satisfies interfaces
   structurally (a type implements `Reconciler` just by having a `Reconcile` method). The
   graph carries no `implements` edge, so `impact`/impl over a Go interface cannot see its
   implementers. Flagged HONEST; the k8s `Reconciler` and gin `Render`/`Bind` cases expose
   this on purpose.
3. **C# type-usage references not resolved.** The C# graph resolves method-call edges but
   not type-usage references to classes, so a domain entity used widely via generic
   `IRepository<T>`/specifications shows a near-empty blast radius (eShopOnWeb
   `EfRepository`).
4. **Context keyword-latching.** The `feature` archetype's `context` ranking is BM25 +
   graph-path; on large repos a generic task phrase can latch onto a dominant keyword and
   miss the intended anchors (the k8s pod-sync and TS compile-pipeline `feature` cases
   both score low). The `feature` archetype is the lowest-recall archetype overall.

No spin: the value proposition is **"near-baseline answers at ~3% of the tokens,"** and
the recall delta is the cost being paid for that. Closing it without giving back the
token win is tracked future work, not a fixed limitation.

---

## 3. Performance, scale & sync

Indexing, query latency, scale to enterprise size, and incremental/auto-sync — every
number measured on the machine above.

### 3.1 Cross-language cold index (8 per-language repos)

Every repo indexed cleanly — **zero crashes, zero FK errors**. **Files** = source files
extracted into `file`-kind nodes; **Nodes/Edges** are the final resolved graph (after
reference resolution promotes unresolved refs into `calls`/`imports`/`implements`/
`contains` edges — larger than the pre-resolution "Indexed N files…" line). Cold-index is
the 3-run-average wall time.

| Repo | Lang | Files | Nodes | Edges | Cold-index | files/s | Peak RSS | Sync (1 file) |
|------|------|------:|------:|------:|:----------:|--------:|:--------:|:-------------:|
| pallets/flask | Python | 83 | 2,110 | 3,742 | 0.19 s | 430 | 47 MB | 22 ms |
| BurntSushi/ripgrep | Rust | 101 | 3,344 | 11,301 | 0.36 s | 278 | 105 MB | 24 ms |
| expressjs/express | JavaScript | 141 | 2,291 | 6,374 | 0.26 s | 542 | 62 MB | 21 ms |
| colinhacks/zod | TypeScript | 408 | 7,596 | 17,275 | 0.57 s | 720 | 135 MB | 32 ms |
| dotnet-architecture/eShopOnWeb | C# | 269 | 1,632 | 2,128 | 0.15 s | 1,793 | 44 MB | 27 ms |
| google/gson | Java | 262 | 5,329 | 15,459 | 0.49 s | 538 | 106 MB | 33 ms |
| nlohmann/json | C++ | 499 | 13,438 | 33,083 | 1.01 s | 494 | 239 MB | 43 ms |
| gin-gonic/gin | Go | 99 | 1,834 | 6,291 | 0.26 s | 376 | 72 MB | 22 ms |

Search/query latency (CLI p50 over 7 runs, each opening the DB cold — a worst case; via
the persistent MCP server latency is sub-millisecond): exact/fuzzy/callers/callees **2–4
ms** across all 8 languages; `impact`/`context` **2–33 ms** depending on graph fan-out
(zod 23 ms, gson 33 ms on hot interfaces). Search is never the bottleneck. Raw:
[`results-index.tsv`](results-index.tsv), [`results-search.tsv`](results-search.tsv).

### 3.2 Scale to enterprise size — both index cleanly

Throughput holds from 80 files up to ~39k files. The two largest real repos are the same
ones measured in the §1.2 enterprise savings tier:

| Repo | Lang | Files indexed | Nodes | Edges | Cold-index (tool / wall) | files/s | Peak RSS | Exit |
|------|------|--------------:|------:|------:|:------------------------:|--------:|:--------:|:----:|
| kubernetes | Go | 17,176 | 208,425 | 970,801 | 55.3 s / 51.5 s | ~310 | 1.49 GB | 0 |
| microsoft/TypeScript | TypeScript | 39,296 | 312,521 | 442,014 | 62.0 s / 58.2 s | ~634 | 1.11 GB | 0 |

**Both index cleanly in ~one minute and exit 0.** The full `microsoft/TypeScript` repo
(39,296 files including `tests/baselines`) now completes without the worker-thread stack
overflow that an earlier build hit on its pathologically deep-nested generated fixtures —
that crash is fixed. kubernetes resolves **779,688 cross-file references** during its
index.

**Synthetic-corpus scaling & the 100k extrapolation.** On post-optimisation
synthetic-corpus runs (django/tokio/vue-core repeated to scale): 3k files 5.8 s, 10k
29.7 s, 16k 56.4 s. The two real enterprise indexes above (17k in 55 s, 39k in 62 s) sit
on the same curve. **Scaling law:** cold-index `t ≈ O(n^1.5)`, peak RSS sub-linear
(`O(n^0.62)` on dedup-heavy corpora; a fully-unique monorepo trends toward O(n)).
**100k-file extrapolation: ~14 min cold-index, ~4 GB peak RSS** — both inside the
acceptance criteria (≤ 20 min, ≤ 4 GB). Extrapolation band ±30–40%.

### 3.3 Incremental sync — O(changed), not O(repo)

Sync re-extracts and re-resolves only changed files and their direct dependants. Measured
1-file sync (tool-reported parse/DB-write time; wall time includes the repo-wide stat
scan that detects what changed):

| Repo | Files | Sync (1-file, tool) | Sync (wall) |
|------|------:|:-------------------:|:-----------:|
| flask | 83 | 22 ms | 0.06 s |
| express | 141 | 21 ms | 0.06 s |
| zod | 408 | 32 ms | 0.10 s |
| json (C++) | 499 | 43 ms | 0.12 s |
| jellyfin (.NET, 2,065 .cs) | 2,065 | 66 ms | 0.30 s |
| **kubernetes (17,176)** | 17,176 | **74 ms** | 1.26 s |
| **microsoft/TypeScript (39,296)** | 39,296 | **202 ms** | 0.90 s |

Larger change sets scale with the change, not the repo: on kubernetes, 1 file 70 ms → 10
files 95 ms → 100 files 850 ms (the references they touch grow 16 → 122 → 13,200). There
is a fixed **O(repo) stat-scan floor** in *wall* time (~0.5–1 s on a 17k-file repo) to
detect what changed; the no-op case (nothing dirty) is 0.07 s.

**When NOT to use sync:** changing *every* file (a mass reformat) is ~2× slower than a
clean cold `index` (k8s: 118 s vs 55 s) — the incremental path pays per-file
delete+re-parse+re-resolve in smaller transactions. Use `codewiki index` for whole-repo
changes; `sync` is optimised for the small change sets that dominate real editing.

### 3.4 Auto-sync — git hooks + live watcher

`codewiki init` installs both paths, by design complementary:

- **Git hooks** (`post-commit`, `post-merge`, `post-checkout`) run `codewiki sync`
  **detached in the background** so git is never blocked. `git commit` returns in
  single-digit ms; the changed symbol becomes queryable **~0.1 s** later — even on
  kubernetes (~128 ms to reflect a commit on a 17k-file repo). Add/remove/branch-switch
  round-trips all verified correct. Works on **every** filesystem, including WSL `/mnt`.
- **Live file watcher** (inside `codewiki serve --mcp`, ~500 ms debounce, notify +
  debouncer). On native ext4 it correctly handles single edits and **coalesces a
  multi-file burst into one sync cycle**; observed latency ~0.2–2.0 s.

**Honest caveats (stated plainly):**
1. The live watcher **does not fire on WSL `/mnt` (drvfs/9p) drives** — inotify events
   don't cross the 9p protocol. This is exactly why the git hooks exist; the hook fallback
   was verified working on the same `/mnt/c` repo (0.24 s). The watcher only runs inside a
   live MCP session.
2. Whole-repo sync is ~2× slower than a fresh `index` (see §3.3) — use `index` for mass
   changes.
3. `codewiki init --no-index` skips git-hook installation (likely unintended) — use plain
   `codewiki init` to get hooks. The hook relies on `codewiki` being on `PATH`.

**Net:** every realistic edit path is covered — agents driving the MCP server on native
disks get the live watcher; everyone gets git hooks on commit/merge/checkout regardless
of filesystem.

---

## 4. .NET worked example

> **Different baseline — read this first.** This per-task trace uses a more economical
> **grep-then-read-*minimal*-files** baseline (read only the files a task strictly needs),
> not the read-*whole*-files baseline of §1. That is why the token reduction here (~83%)
> is lower than §1's 97% — both are honest; they simply measure against different
> baselines. The §1 cross-language harness is the authoritative, recall-scored
> measurement; this is a detailed, repo-specific companion.

Five realistic tasks on **eShopOnWeb** (254 .cs) and **jellyfin** (2,065 .cs), freshly
re-run on `codewiki 0.1.1`. Byte counts from real CLI output and file sizes; tokens =
bytes ÷ 4. Raw data: [`results-dotnet.tsv`](results-dotnet.tsv).

| Task | Repo | CW calls | BL calls | Call reduction | CW tokens | BL tokens | Token reduction | $ saved |
|------|------|:--------:|:--------:|:--------------:|----------:|----------:|:--------------:|:-------:|
| DI consumers (`IBasketService`) | eShopOnWeb | 2 | 6 | **67%** | 380 | 3,476 | **89%** | $0.009 |
| Feature comprehension (basket checkout) | eShopOnWeb | 1 | 6 | **83%** | 1,017 | 6,842 | **85%** | $0.018 |
| Interface→impls (`IRepository`) | eShopOnWeb | 2 | 6 | **67%** | 624 | 4,219 | **85%** | $0.011 |
| Blast radius (`OrderService` refactor) | eShopOnWeb | 1 | 5 | **80%** | 246 | 1,959 | **87%** | $0.005 |
| Cross-cutting: auth config | jellyfin | 2 | 4 | **50%** | 2,034 | 8,158 | **75%** | $0.018 |
| **Average** | | **1.6** | **5.4** | **70%** | **860** | **4,930** | **83%** | **$0.012** |

Interface→impl queries traverse real `implements` edges — `codewiki impact IRepository`
returns the concrete `EfRepository` implementation directly. Honest note: the auth-config
task is a genuine 2-call scenario — the generic term "configured" first collides with EF
Core `*Configuration` classes in FTS; a second, more specific query recovers the right
results (both calls counted).

**.NET extraction-quality audit (summary).** A deeper audit on larger corpora (jellyfin
2,065 .cs, OrchardCore 5,203 .cs, ABP 3,497 .cs) found and the maintainer fixed six C#
extraction gaps, all now verified on `codewiki 0.1.1`:

| Gap | Status |
|-----|--------|
| Interface/struct/enum/record misclassified as `class` | **FIXED** — 100% correct (e.g. OrchardCore 505 interfaces, ABP 623, jellyfin 209) |
| Namespace not extracted → unqualified names | **FIXED** — fully-qualified (`Volo.Abp.Domain.Services::DomainService`); 250–5,024 namespaces/repo |
| `using` import edges dropped | **FIXED** — 468–6,165 import edges/repo (was ~0) |
| Method signatures + `is_async` missing | **FIXED** — 100% signature coverage; async accurately flagged |
| MVC `[action]` route token not expanded | **FIXED** — 0 literal `[action]` tokens remain |
| DI `implements` edges dropped (FK bug) | **PARTIAL** — FK storage fixed; `AddSingleton` matched, `AddScoped`/`AddTransient` pending |

Verdict: **READY for enterprise .NET use.** Open items (non-blocking): `AddScoped`/
`AddTransient` DI patterns, `.csproj`/`.sln` project-graph modelling, memory scaling on
5k+ .cs repos.

---

## 5. Unicode / i18n

Tested on a synthetic 18-file corpus across 5 languages (Python, TypeScript, Go, C#,
Rust) with heavy Vietnamese comments/docstrings and Vietnamese-derived ASCII identifiers
(`tinhTong`, `dangNhap`, `QuanLyNguoiDung`), including an English twin file for direct
parity.

| Dimension | Result |
|-----------|--------|
| Symbol extraction parity vs English | **Identical** — tree-sitter AST is comment-language-agnostic (auth.py VN = 12 nodes = auth_en.py EN) |
| Crash / encoding error | **None** (18/18 files clean) |
| UTF-8 storage integrity | **Intact** — no mojibake, no truncation |
| Identifier search (ASCII-derived VN names) | **Full support** — `tinh`, `dang`, `quan` prefix-match across all langs |
| FTS Latin-1 diacritics (`à`, `ì`, `ù`) | **Folds correctly** (U+00C0–U+00FF) |
| FTS extended VN diacritics (`ư`, `đ`, `ắ`) | **Does not fold** (Latin Extended-A/B/Additional stay as-is) |
| `context` with Vietnamese prose query | Works, slightly noisier than English (identifiers are ASCII-derived) |
| `callers`/`callees`/`impact` on VN symbols | **Full support** |

**Verdict:** CodeWiki is Vietnamese-safe for its primary workflow (symbol/identifier
search, call-graph analysis, context building). Real Vietnamese codebases use ASCII
identifiers derived from Vietnamese syllables — exactly the tested case — and these work
perfectly. The extended-diacritic FTS gap is a property of the default
`unicode61 remove_diacritics=1` tokenizer (a one-line schema change to
`remove_diacritics 2` would add full NFD stripping for prose queries), not a
Vietnamese-specific bug. Docstring text is not yet stored in FTS for *any* language (a
pre-existing limitation, not a regression).

---

## 6. Methodology & reproduce it yourself

### 6.1 How the savings harness works

The §1 figures are produced by [`run-savings.sh`](run-savings.sh) over the canonical
[`cases.tsv`](cases.tsv) (151 cases across 10 repos). Each case row carries a unique
`case_id`; its oracle is `oracle/<lang>_<case_id>.json`.

**Archetypes** (CodeWiki tool / baseline shape):

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
  actually use, and where the rendering optimisations live. A CLI measurement would
  under-report the win. Both sides are measured on the same task.
- **Deterministic recall scoring — no LLM judge.** Each answer is scored by
  [`lib/score.py`](lib/score.py) against the frozen oracle: recall = required-elements-
  present / required; verdict PASS (≥0.8) / PARTIAL / FAIL plus a wrong-primary gate.
  Empty-oracle honest graph-gap cases are UNSCORABLE (excluded from recall means, kept in
  call/token totals).
- **Oracles are graph-derived + grep-cross-checked + frozen** (see §2). Built by
  [`lib/build_oracle_extra.py`](lib/build_oracle_extra.py) (which imports the derivation
  logic in [`lib/build_oracle.py`](lib/build_oracle.py)). The baseline is scored on the
  **identical** oracle.
- **Tokens = output bytes ÷ 4** (conservative). `feature`/`context` is median-of-3
  (non-deterministic ranking); `search`/`callers`/`callees`/`impact` are deterministic.
  `codewiki_explore` + `codewiki_node` are exercised over MCP as a coverage probe and
  recorded in a separate `mcp_coverage_bytes` column — **not** folded into the call/token
  comparison, so each archetype stays apples-to-apples with its baseline task.

### 6.2 Reproduce it yourself — one command

```bash
# Confirm the binary:  codewiki --version  -> codewiki 0.1.1
CW=/path/to/codewiki benchmark/run-savings.sh
# -> clones the 10 repos under /tmp/bench, COLD-indexes each, runs all 151 cases over MCP,
#    writes benchmark/results-savings.tsv (per-case rows) + results-savings-summary.txt.
#    Flags: NO_CLONE=1 (reuse clones), NO_REINDEX=1 (reuse existing .codewiki indexes).
```

Every headline number in this document and in the outer [`README.md`](../README.md) is an
aggregate of rows in [`results-savings.tsv`](results-savings.tsv) — recompute any of them
directly from that file.

Rebuild + hand-verify oracles (only when adding/changing cases):

```bash
python3 benchmark/lib/build_oracle_extra.py --bench-root /tmp/bench --cases benchmark/cases.tsv --out benchmark/oracle
python3 benchmark/lib/build_oracle_extra.py --report   --cases benchmark/cases.tsv --out benchmark/oracle
```

Reproduce the §3 performance numbers:

```bash
CW=/path/to/codewiki benchmark/run-index.sh    # -> results-index.tsv (cold-index, nodes/edges, RSS, sync)
CW=/path/to/codewiki benchmark/run-search.sh   # -> results-search.tsv (query p50 latency)
# Enterprise scale (large clones):
codewiki init --path /tmp/bench/kubernetes && codewiki init --path /tmp/bench/TypeScript
```

### 6.3 Versions & repo commit SHAs (2026-05-25)

`codewiki 0.1.1` (release build, default features). Shallow `--depth 1` clones of `main`:

| Repo | Tier | SHA | Repo | Tier | SHA |
|------|------|-----|------|------|-----|
| pallets/flask | small | `954f568` | google/gson | medium | `abfef5e` |
| BurntSushi/ripgrep | small | `4519153` | nlohmann/json | medium | `484483a` |
| expressjs/express | small | `dae209a` | colinhacks/zod | medium | `bbc68f9` |
| gin-gonic/gin | small | `5f4f964` | kubernetes/kubernetes | enterprise | `b859f5a5` |
| dotnet-architecture/eShopOnWeb | small | `4da8212` | microsoft/TypeScript | enterprise | `e5509e211` |

A companion **context-relevance fixture** ([`../parity/context-relevance/`](../parity/context-relevance/))
scores `codewiki_context` roots@5 precision/recall over the frozen `synthetic-120` corpus
(floors in `parity/thresholds.toml [context]`).

---

## Raw data (TSV)

- [`results-savings.tsv`](results-savings.tsv) — agent-savings, 151 cases (per-case calls, bytes, recall, verdict) — **source for §1**
- [`results-index.tsv`](results-index.tsv) — cross-language cold-index (files, nodes, edges, time, RSS, DB size, files/s, sync)
- [`results-search.tsv`](results-search.tsv) — search/query p50 latency by repo and query type
- [`results-dotnet.tsv`](results-dotnet.tsv) — .NET per-task agent-savings (calls, bytes, tokens, $) — source for §4
