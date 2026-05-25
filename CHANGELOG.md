# Changelog

All notable changes to CodeWiki are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
  (acceptance target: ≤ 20 min). See [`benchmark/ANALYSIS-SCALE.md`](benchmark/ANALYSIS-SCALE.md).
- Optimisation waves 1–6: key wins — in-memory name-index cache, streaming bulk writes,
  FTS drop/rebuild, incremental sync scope, CargoWorkspace map caching, parallel
  resolution, cursor pagination, full-core parse pool.

#### 649 tests, clippy clean
All 649 tests pass. `cargo clippy --all-features -- -D warnings` clean.
Parity harness validates node/edge counts against golden outputs for elasticsearch
and synthetic-120 corpora.

---

[0.1.0]: https://github.com/0xsyncroot/codewiki/releases/tag/v0.1.0
