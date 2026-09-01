# Changelog

All notable changes to CodeWiki are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Fixed

- **`codewiki callers` / `codewiki callees` answer their own question.**
  `--depth` now defaults to 1; transitive rows are labelled `←…← (depth N)`
  instead of sharing the direct-call arrow; rows carry the real call-site line;
  a caller with N call sites shows `xN call sites` instead of N identical rows;
  file-scope and self-recursive callers are labelled; truncation is explicit.
  The traversal no longer drops direct edges first reached on a deeper path,
  and self-recursive functions now list themselves.
- **`callers`/`callees` no longer report one definition and omit the rest.**
  The CLI resolved a bare name to a single node, so for any name carried by
  more than one definition it reported that one definition's neighbours —
  reading as a confident `(none)`. On a fresh index of pallets/flask,
  `codewiki callers create_app` printed `(none)` for a name with 14
  definitions and 5 call edges. Both commands now union the same-name family
  (as the MCP tools already did) while keeping per-call-site lines and hop
  labelling, and say `(aggregated across N definitions with this name)` when
  the name is not unique. A qualified name still resolves to exactly one node.
- **CI green on current stable.** Clippy 1.98 rejects two patterns that passed
  when written (`question_mark` in the name matcher, `useless_format` in the
  Hermes installer target); with `-D warnings` they were hard CI failures on a
  clean checkout. Lint hygiene only, no behavior change.
- **Module-scope initialisers are indexed.** The body of
  `export const handler = async () => {…}` and every `const x = call()` —
  including inside anonymous `it()`/`describe()` callbacks — previously
  produced no call edges at all; the initialiser subtree was skipped outright.
  Calls now attribute to the binding when it names a single identifier.
  Class-field initialisers (`readonly f = () => call()`) are walked too.
- **Anonymous arrows no longer borrow a name.** `a => …` produced a Function
  named `a` (from its `parameter` field) and `() => counter` one named
  `counter` (from its bare-identifier body); because resolution matches by
  name, these junk nodes captured false edges — 12,000 `it(...)` calls
  resolved onto one junk node in a measured corpus. The name fallback is now
  grammar-field-aware, while still accepting assignment-LHS (`left`) names so
  Python/Ruby module variables keep their nodes.
- **`.tsx` parses with the TSX grammar.** JSX broke the plain-TypeScript
  parse and silently dropped declarations (one file kept 2 of 6 top-level
  functions). `LANGUAGE_TSX` is now used for `.tsx`; `.ts` is unchanged and
  `.jsx` was never affected.
- **Incremental indexing no longer destroys edges (data loss).** Re-storing a
  changed file deleted its nodes wholesale, and the `ON DELETE CASCADE`
  silently removed every edge touching them — including incoming edges from
  untouched files, whose consumed unresolved refs could never re-create them.
  Measured: a one-line edit permanently killed 1,926 incoming edges; a file
  delete + restore (branch checkout, stash, revert via the installed git
  hooks) killed 1,862. Re-stores now retarget incoming edges of moved symbols
  in place and degrade edges of genuinely-removed symbols to pending
  unresolved refs, which re-resolve when the symbol returns.
- **Framework nodes survive source edits.** Components/hooks/routes were wiped
  by any edit to their source file and never re-created (stale virtual-manifest
  hash gate; no framework extract on the incremental path). The extract pass
  now runs scoped to changed files, manifests carry an honest source hash, and
  stale manifests are cleared.
- A resolved reference is consumed only when its edge is actually inserted;
  `delete_unresolved_by_node` no longer silently no-ops on a file path.

### Changed

- Call-graph shape improves after a reindex: measured on a 2,085-file
  TypeScript corpus, calls edges 49,670 → 61,877 with false short-named-target
  edges 12,430 → 515.
- **Migration v7** adds a unique edge-identity index (deduping existing rows)
  and schedules a one-time full re-store on the next `index`/`sync`: databases
  written before this fix have silently lost edges that cannot be recovered
  from the database alone, so they are rebuilt from source through the safe
  path.

### Changed

- `callers`/`callees` CLI output format (see above) — scripts parsing the old
  format need updating. Call-site lines require a reindex to appear.

---

## [0.2.1] - 2026-05-26

Small UX release: the `init`/`index` indexing display is now a determinate,
colorized progress bar instead of an indeterminate spinner.

### Changed

- **Determinate progress bar for `init`/`index`.** The indexing display now
  fills 0→100% as files are processed, showing the live phase, a filled bar +
  percent, `pos / len files`, and running node/edge counts with the file
  currently streaming by — ending in two `✓` summary lines (Indexed, Resolved).
  Uses a cyan/green/magenta accent palette with a smooth fractional-block bar
  edge on a TTY; degrades to ASCII glyphs and 16-color ANSI on basic terminals,
  honors `NO_COLOR`, and stays plain and non-spammy when piped/CI. The total is
  sized up front via a new no-op-by-default `ExtractionStore::begin_index` hook,
  so the bar fills exactly to 100% without a second filesystem walk.

### Removed

- **Dead `-i` / `--interactive` flag on `codewiki init`.** It was parsed then
  discarded and never did anything; `codewiki init` behaves exactly as before
  (auto-index unless `--no_index`). Scripts that passed `-i` should drop it.

---

## [0.2.0] - 2026-05-26

Query-quality release: CodeWiki now returns the right answer **as completely as
reading whole files** (recall 0.88 vs the grep+read baseline's 0.92, up from 0.77)
while still cutting ~98% of tokens — plus a fixed web graph, a lively install/index
UX, and a re-runnable upgrade path.

### Added

- **Go structural-interface resolution.** Go satisfies interfaces structurally
  (no `implements` keyword); CodeWiki now synthesizes `implements` edges by
  method-set superset matching (name + arity), so `impact`/impl over a Go
  interface returns its real implementers.
- **C# type-usage references.** Field, property, parameter, return, constructor,
  and generic-argument types now emit `references` edges, so blast/impact over a
  widely-used C# type sees its real dependants.
- **`codewiki upgrade [--check]`** subcommand: checks GitHub for a newer release
  and self-updates by re-invoking the platform installer. Offline-safe (network
  failures print a friendly note and exit 0); `--check` reports without installing.
- **Installer upgrade path.** Re-running `install.sh` / `install.ps1` now detects
  the installed version, resolves the target (latest release or a pinned
  `--version`/`-Version`), and compares them numerically (`v0.10.0 > v0.9.0`).
  Same version is a no-op; a newer target upgrades; an older target downgrades
  with a notice. Upgrades back up the current binary, atomically replace it,
  smoke-test the new binary's `--version`, and roll back on failure. On Windows
  a locked (running) `codewiki.exe` is detected and reported clearly with the
  original install left intact.
- **Lively init/index progress.** A colored, TTY-aware progress display streams
  the current file + live node/edge counts during `init`/`index`; falls back to
  clean plain output when piped (no escape-code spam), redraws throttled.
- **Large .NET benchmark tier** (jellyfin, 2,065 .cs) and a harness that now
  **fails loudly** on an `init`/index error instead of silently scoring a broken
  index.

### Changed

- **Query recall raised 0.77 → 0.88** (overall, on frozen oracles): overloaded-name
  aggregation unions the same-name family for `callers`/`callees`/`impact`;
  `context` ranking uses multi-term/IDF coverage to stop a dominant keyword from
  crowding out the real anchors; `impact` output is provenance-aware (keeps real
  dependants/implementers, drops file/namespace markers + context-only children).
  Per-archetype: impl 0.81 → 1.00, blast 0.74 → 0.92, callers 0.60 → 0.79.
- **Web graph viewer.** The initial view is now a connected subgraph (was 20
  isolated nodes with no edges); the default node cap is raised 200 → 2000; and
  truncation keeps a connected core instead of an arbitrary set, fixing the
  near-edgeless render on larger neighborhoods.
- **Benchmark consolidated** into a single report and expanded to 157 cases over
  11 repos across four size tiers; oracles are frozen and integrity-restored.

### Fixed

- **C# index crash** on repositories containing Blazor `.razor` `@code` blocks: a
  type-usage ref anchored to the dropped `@code` wrapper caused a foreign-key
  violation that rolled back the entire C# batch (only `contains` edges survived).
- **Deeply-nested ASTs** (e.g. microsoft/TypeScript, 39,296 files) now index
  without a worker-thread stack overflow (64 MB worker stacks).

---

## [0.1.1] - 2026-05-25

First functional public release with cross-platform binaries. Hardens the
initial rewrite after a multi-agent QA pass on real repositories.

### Fixed

- **C++ extraction on real codebases.** Previously a template specialization
  with a base class crashed indexing (orphan edge → foreign-key violation →
  the entire repository indexed 0 files, silently). C++ now resolves template
  and qualified names, recovers class names through export/visibility macros
  (e.g. `SPDLOG_API`), classifies out-of-line definitions as methods, and routes
  `.h` headers to the C++ grammar. nlohmann/json and spdlog now index fully
  (hundreds of classes, thousands of methods, inheritance edges; no crash).
- **`implements` / `extends` edges** are now emitted from class heritage for
  C#, TypeScript, and Java (previously only DI-derived edges existed).
- **Java** `interface`/`enum` are no longer mis-typed as `class`.
- **Rust** trait/impl methods are no longer double-emitted as both a `function`
  and a `method` node (with duplicate `calls` edges).
- **Unicode identifiers** (e.g. Vietnamese) are stored verbatim instead of being
  stripped to ASCII; search stays diacritic-insensitive via the FTS tokenizer.
- **Framework routes** are detected from minimal in-file patterns (Express,
  Django, ASP.NET), not only from projects with a manifest.
- **MCP server no longer crashes in an un-indexed project** — it serves a
  friendly "run `codewiki init`" message, so one global registration works
  across every project.
- **`doctor`** recognizes `--location local` installs; cross-platform path and
  test-isolation fixes make the suite pass on Linux, macOS, and Windows.

### Added

- **Robustness:** a foreign-key backstop skips orphan edges (with a warning)
  instead of rolling back a whole repository; `index`/`init` exit non-zero when
  a repository with source files indexes none (no more silent data loss).
- **Live auto-sync:** the MCP server starts a file watcher that re-indexes on
  change (in addition to the git hooks installed by `init`).
- Cross-platform CI (Linux, macOS, Windows); MIT license detection; community
  health files (`CONTRIBUTING`, `CODE_OF_CONDUCT`, `SECURITY`, issue/PR
  templates); honest CodeGraph derivative-work attribution in `NOTICE`.

### Removed

- The unimplemented `telemetry` Cargo feature and its dependencies — CodeWiki is
  100% local with no telemetry.

---

## [0.1.0] - 2026-05-25

### Added

**Rust rewrite — full rewrite from TypeScript prototype to a production Rust binary.**

#### Core knowledge graph
- SQLite knowledge graph with FTS5 full-text search. Stores nodes (functions, classes,
  methods, interfaces, enums, structs, routes, components) and typed edges (calls,
  imports, inherits, implements, route-to-handler).
- WAL journal mode with online backup (`snapshot` / `restore` commands).
- Incremental sync pipeline: only re-extracts and re-resolves changed files
  (O(changed), not O(repo)). 1-file change syncs in 20–150 ms across all tested repos.

#### 18 language extractors (tree-sitter)
C, C++, C#, Dart, Go, Java, JavaScript, Kotlin, Lua, Luau, Pascal, PHP, Python,
Ruby, Rust, Scala, Swift, TypeScript, Vue, Svelte, Liquid, Razor.

#### 16 framework resolvers
Angular (components, DI, routing, standalone), ASP.NET / Razor (HTTP routes, DI,
namespace qualification, interface discrimination), Cargo workspace, Django, Express,
FastAPI, Flask, Go modules, Laravel, NestJS, Rails, React, Spring, Svelte,
Swift Package Manager, Vue.

#### Full ASP.NET / .NET support
- Correct interface / struct / enum / record node classification (was misclassified as
  `class` in prior versions).
- Namespace-qualified names (`Microsoft.eShopWeb.ApplicationCore.Interfaces::IBasketService`).
- Method signatures (100% coverage) and `is_async` detection.
- ASP.NET route extraction: `[HttpGet/Post/Delete]`, minimal API, `MapGroup`,
  SignalR `MapHub<T>`, MVC `[action]` token expansion (zero unexpanded tokens).
- `using` import edges (fixed field-name probe; was returning ~1% of expected edges).
- Verified on eShopOnWeb, jellyfin (2,065 .cs), orchardcore (5,203 .cs), ABP framework.

#### Angular support
- Component nodes with selector, standalone flag, inputs/outputs in metadata.
- Directive, pipe, service (`@Injectable`), and `@NgModule` node extraction.
- Route nodes: `loadComponent`, lazy-loaded routes, route guards.
- DI graph: constructor-injection unresolved refs + `imports:[]` binding edges.
- Corpus acceptance thresholds validated on angular-realworld-example-app,
  ng-matero, and angular/components.

#### MCP server — 9 tools
`codewiki_search`, `codewiki_context`, `codewiki_callers`, `codewiki_callees`,
`codewiki_impact`, `codewiki_node`, `codewiki_explore`, `codewiki_files`,
`codewiki_status`.

All tools run over a persistent stdio JSON-RPC server. Latency is sub-millisecond via
the MCP server (CLI adds ~2 ms binary cold-start on top).

#### Graph web UI
`codewiki graph` launches a local axum HTTP server with an embedded force-directed
graph frontend. Features: neighbourhood explorer, filter by node kind / language,
node detail panel, impact view. Behind the `web` feature flag.

#### FTS5 Unicode / i18n search
- `unicode61` tokeniser with diacritic folding for Latin-1 characters.
- Vietnamese-safe: ASCII-derived identifiers (`tinhTong`, `dangNhap`, `QuanLyNguoiDung`)
  fully searchable. UTF-8 storage is intact across all tested languages.
- Hybrid BM25 + graph-path scoring for `context` queries (NL relevance 6/8 on test suite).

#### Docstring extraction
- Python (`"""..."""`, `'''...'''` with PEP 257 placement detection).
- Rust (`///` and `/** */` doc comments).
- Go (`//` block comments preceding declarations).
- TypeScript / JavaScript (`/** JSDoc */`).
- C# (`///` XML doc comments).

#### Onboarding and tooling
- `codewiki setup`: one-command setup — detects project, indexes or syncs, wires MCP
  into selected agents. Prints indexed file / node / edge counts and elapsed time on
  completion.
- `codewiki doctor`: 6 health checks (binary on PATH, index exists, index non-empty,
  freshness, agent wired, git hook).
- `codewiki install` / `uninstall`: wire / unwire MCP config for Claude Code, Cursor,
  Windsurf, Codex, Hermes.
- `codewiki snapshot` / `restore`: portable SQLite backup using the online backup API
  (WAL checkpoint before export).

#### Performance
- Cold-index: 370–1510 files/s on small/medium repos; django (3,019 files) in 11.8 s.
- 100k-file cold-index: ~14 min extrapolated from measured 3k / 10k / 16k data points
  (acceptance target: ≤ 20 min). See [`benchmark/README.md`](benchmark/README.md).
- Optimisation waves 1–6: key wins — in-memory name-index cache, streaming bulk writes,
  FTS drop/rebuild, incremental sync scope, CargoWorkspace map caching, parallel
  resolution, cursor pagination, full-core parse pool.

#### 649 tests, clippy clean
All 649 tests pass. `cargo clippy --all-features -- -D warnings` clean.
Parity harness validates node/edge counts against golden outputs for elasticsearch
and synthetic-120 corpora.

---

[0.1.0]: https://github.com/0xsyncroot/codewiki/releases/tag/v0.1.0
