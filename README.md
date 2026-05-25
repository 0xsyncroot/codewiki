# CodeWiki

**Local code knowledge graph for AI agents. Tree-sitter parsed, SQLite stored, MCP served.**

[![CI](https://github.com/0xsyncroot/codewiki/actions/workflows/ci.yml/badge.svg)](https://github.com/0xsyncroot/codewiki/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.1.0-green.svg)](CHANGELOG.md)
[![Rust](https://img.shields.io/badge/rust-1.78%2B-orange.svg)](https://www.rust-lang.org)

Instead of reading whole source files, your AI agent asks the graph:

```
codewiki_callers("OrderService")   → 3 ms, exact answer
codewiki_impact("AuthService")     → 6 ms, full blast radius
codewiki_context("basket checkout")→ 4 ms, ranked entry points + key code
```

**Measured on real .NET codebases: ~69% fewer tool calls, ~86% fewer tokens, ~$0.013 saved per task.**  
100% local. Single static binary. No cloud, no telemetry, no API keys.

---

## Table of contents

- [What it is](#what-it-is)
- [Why it pays off](#why-it-pays-off)
- [Install](#install)
- [Quick start](#quick-start)
- [MCP tools](#mcp-tools)
- [Editor and agent support](#editor-and-agent-support)
- [CLI commands](#cli-commands)
- [Graph UI](#graph-ui)
- [Maintenance and incremental sync](#maintenance-and-incremental-sync)
- [Performance](#performance)
- [Languages and frameworks](#languages-and-frameworks)
- [Enterprise](#enterprise)
- [Architecture](#architecture)
- [Roadmap](#roadmap)
- [License](#license)

---

## What it is

CodeWiki parses your codebase with [tree-sitter](https://tree-sitter.github.io/tree-sitter/)
and stores the result in a local SQLite knowledge graph: nodes (functions, classes,
interfaces, enums, structs, routes, components) and typed edges (calls, imports,
inherits, implements, route-to-handler, DI bindings).

The graph is exposed to AI coding agents via a
[Model Context Protocol](https://modelcontextprotocol.io) (MCP) server —
9 `codewiki_*` tools that Claude Code, Cursor, Codex, and other MCP-capable agents call over
stdio JSON-RPC. Agents get sub-millisecond structural answers instead of reading files.

Key properties:

- **18 languages** — Python, Rust, TypeScript/JavaScript, C#, Java, Go, Kotlin, PHP,
  Ruby, Swift, C, C++, Vue, Svelte, Scala, Dart, Lua, Pascal and more.
- **16 framework resolvers** — including Angular, ASP.NET / Razor, Django, Express,
  NestJS, React, Vue, Spring, Rails, Flask, FastAPI, Laravel, Cargo, and more.
- **FTS5 full-text search** — Unicode-aware BM25 ranking with hybrid graph-path scoring.
- **Fully incremental** — 1-file change syncs in 20–150 ms.
- **Docstring extraction** — Python, Rust, Go, TypeScript/JavaScript, C#.
- **100% local** — grammars are bundled in the binary; no network access after install.

---

## Why it pays off

Measured across 5 realistic tasks on eShopOnWeb (254 .cs) and jellyfin (2,065 .cs).
All byte counts from real CLI output. Pricing: Claude Sonnet $3.00 / 1M input tokens.
Full methodology: [`benchmark/DOTNET-REPORT.md §7`](benchmark/DOTNET-REPORT.md).

| Task | CW calls | Baseline calls | Call reduction | CW tokens | Baseline tokens | Token reduction | $ saved |
|------|:--------:|:--------------:|:--------------:|:---------:|:---------------:|:--------------:|:-------:|
| DI consumers (`IBasketService`) | 2 | 6 | **66%** | 400 | 3,498 | **88%** | $0.009 |
| Feature comprehension (basket checkout) | 1 | 6 | **83%** | 1,035 | 6,934 | **85%** | $0.018 |
| Interface→impls (`IRepository`) | 2 | 6 | **66%** | 449 | 4,335 | **89%** | $0.012 |
| Blast radius (`OrderService` refactor) | 1 | 5 | **80%** | 264 | 1,977 | **86%** | $0.005 |
| Cross-cutting: auth config (jellyfin) | 2 | 4 | **50%** | 1,296 | 8,332 | **84%** | $0.021 |
| **Average** | **1.6** | **5.4** | **69%** | **689** | **5,015** | **86%** | **$0.013** |

A developer session with 20 agent interactions saves roughly **$0.26** while the index
stays current at 20–150 ms per file change. The savings compound: the index is built
once, maintained automatically, and every subsequent query is free.

---

## Install

**One-liner (Linux / macOS):**

```sh
curl -fsSL https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.sh | sh
```

**Homebrew (macOS):**

```sh
brew install --formula dist/homebrew/codewiki.rb
```

*(Formula will be submitted to homebrew-core at v1.0 GA.)*

**Build from source (Rust 1.78+):**

```sh
git clone https://github.com/0xsyncroot/codewiki
cd codewiki
cargo build --release
# binary: ./target/release/codewiki
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.ps1 | iex
```

Check the installed version:

```sh
codewiki --version
# codewiki 0.1.0
```

The installed binary includes all features — graph UI, FTS5, 18-language support — by
default. No extra flags needed.

---

## Quick start

**One command does everything** — detects project, indexes, wires MCP into your agent:

```sh
codewiki setup
# Indexed 141 files — 2025 nodes, 1884 edges (0.3s).
# Restart Claude Code (or any configured agent) to activate the MCP tools.
```

Or run the steps individually:

```sh
# 1. Index this project
codewiki init

# 2. Wire into your agent (Claude, Cursor, Codex, opencode, Hermes)
codewiki install --target claude

# 3. Verify everything is healthy
codewiki doctor
```

`codewiki doctor` runs 6 checks: binary on PATH, index initialized, index non-empty,
freshness, agent wired, git hook installed.

---

## MCP tools

All 9 tools are served by `codewiki serve --mcp` (wired automatically by `codewiki install`).
The MCP server keeps the SQLite connection open — query latency is sub-millisecond.

| Tool | Description |
|------|-------------|
| `codewiki_search` | Find a symbol by name — exact, fuzzy, or namespace-qualified |
| `codewiki_context` | AI-focused context for a task description — ranked entry points + key code |
| `codewiki_callers` | All call sites of a symbol |
| `codewiki_callees` | Everything a symbol calls |
| `codewiki_impact` | Transitive blast radius of changing a symbol (depth 3) |
| `codewiki_node` | Symbol signature, source span, and docstring |
| `codewiki_explore` | Several related symbols' source in one capped call |
| `codewiki_files` | Indexed files under a directory path with metadata |
| `codewiki_status` | Index health: file count, node/edge counts, DB size |

---

## Editor and agent support

CodeWiki ships a standard [Model Context Protocol](https://modelcontextprotocol.io)
stdio server (`codewiki serve --mcp`, JSON-RPC over stdin/stdout, protocol `2024-11-05`),
so it works with **any MCP-compatible agent**. For the agents below, a one-command
installer writes the MCP config (and agent instructions) for you:

| Agent | Install command | Notes |
|-------|-----------------|-------|
| Claude Code | `codewiki install --target claude` | Writes `~/.claude.json` (global) or `./.mcp.json` (local); adds a CodeWiki block to `CLAUDE.md`. |
| Cursor | `codewiki install --target cursor` | Writes `~/.cursor/mcp.json` (global) or `./.cursor/mcp.json` (local); local installs also add `./.cursor/rules/codewiki.mdc`. |
| Codex CLI | `codewiki install --target codex` | Global only — writes `[mcp_servers.codewiki]` to `~/.codex/config.toml` and a block to `~/.codex/AGENTS.md`. |
| opencode | `codewiki install --target opencode` | Writes `~/.config/opencode/opencode.jsonc` (global) or `./opencode.jsonc` (local); adds a block to `AGENTS.md`. |
| Hermes Agent | `codewiki install --target hermes` | Global only — writes `mcp_servers.codewiki` to `$HERMES_HOME/config.yaml` (defaults to `~/.hermes/config.yaml`). |

Pass `--location local` to wire the current project instead of your user-wide config
(Codex CLI and Hermes Agent are global-only). `codewiki install --target all` configures
every detected agent at once, and `codewiki setup` indexes the project and wires agents
in a single step. Run `codewiki uninstall --target <name>` to cleanly remove a config.

### Any other MCP client (manual)

Any MCP-capable client — including editors and assistants without a first-class
installer above — can use CodeWiki by registering the stdio server directly. The
canonical invocation is `codewiki serve --mcp`. A typical `mcpServers` entry:

```json
{
  "mcpServers": {
    "codewiki": {
      "type": "stdio",
      "command": "codewiki",
      "args": ["serve", "--mcp"]
    }
  }
}
```

If your client launches MCP servers without inheriting the project working directory,
add `--path` so CodeWiki finds the right index, e.g.
`"args": ["serve", "--mcp", "--path", "/abs/path/to/project"]`. Place the entry in
whatever config file your client reads (the schema varies by client) and restart it.

---

## CLI commands

**Common — start here:**

```
codewiki setup        Index + wire MCP in one step  [START HERE]
codewiki status       Index statistics
codewiki doctor       Diagnostics and health checks
codewiki query        Search a symbol by name
codewiki context      AI-focused context for a task
```

**Advanced:**

```
codewiki init         Initialize .codewiki/ for this project
codewiki index        Re-index all files (or --path <dir>)
codewiki sync         Sync changed files into the index
codewiki serve        MCP server (--mcp for stdio JSON-RPC)
codewiki files        List indexed files
codewiki callers      Callers of a symbol
codewiki callees      Callees of a symbol
codewiki impact       Blast radius of a symbol change
codewiki affected     Symbols affected by a set of changed files
```

**Management:**

```
codewiki install      Wire agent configs (subset of setup)
codewiki uninstall    Remove from agent configs
codewiki uninit       Remove .codewiki/ from this project
codewiki snapshot     Export index to a portable SQLite file
codewiki restore      Restore index from a snapshot
```

Run `codewiki <command> --help` for full option details.

---

## Graph UI

`codewiki graph` launches a local web viewer for interactive exploration of the
knowledge graph:

- **Force-directed graph** — nodes (functions, classes, routes, components) and edges
  (calls, imports, inherits, implements) rendered in the browser
- **Neighbourhood explorer** — click any node to focus on its immediate call graph
- **Filter by kind and language** — narrow the view to classes, interfaces, routes, or
  a specific language
- **Node detail panel** — signature, file location, docstring, and related symbols
- **Impact view** — highlight the transitive blast radius of a selected symbol

The graph UI is included in every default build:

```sh
# Launch (opens http://localhost:7007 by default):
codewiki graph

# Custom port, no auto-open:
codewiki graph --port 8080 --no-open
```

For a minimal binary without the graph UI (rare, e.g. server-side MCP-only deploys):

```sh
cargo build --release --no-default-features --features bundled-sqlite,wasmtime-grammars
```

---

## Maintenance and incremental sync

**The graph stays current automatically — at negligible cost.**

On `codewiki init`, git hooks are installed in `.git/hooks/` (`post-commit`,
`post-merge`, `post-checkout`). Each hook runs `codewiki sync` after a commit,
merge, or branch switch. The MCP server also runs an internal file watcher with a
~1 s debounce that syncs on any file save.

Incremental sync is truly incremental: only changed files and their direct dependants
are re-extracted and re-resolved. The rest of the graph is untouched.

**Measured sync times (1 file changed):**

| Repo | Files | Sync time |
|------|------:|:---------:|
| flask | 83 | 20 ms |
| express | 141 | 20 ms |
| zod | 408 | 30 ms |
| vuecore | 535 | 40 ms |
| django | 3,019 | 150 ms |
| jellyfin (.cs, 2,065 files) | 2,065 | 61 ms |

Before the incremental-sync optimisation, django sync was **3.13 s** per file change
(24× slower). The fix scopes re-resolution to changed files only, making sync
proportional to the change size, not the repo size.

**The cost story:** index once (~0.2–11.8 s depending on repo size), then maintain at
20–150 ms per change. Every subsequent query reuses the same graph for sub-ms
responses. The $0.013/task savings repeat every session.

---

## Performance

Full tables and wave-by-wave optimisation history: [`benchmark/README.md`](benchmark/README.md).
Machine: 28 cores, 31 GB RAM.

**Cold-index speed — 10 real repos (post-Wave-4 build):**

| Repo | Lang | Files | Index time | files/s |
|------|------|------:|:---------:|--------:|
| requests | Python | 37 | 0.1 s | 370 |
| flask | Python | 83 | 0.2 s | 415 |
| express | JavaScript | 141 | 0.3 s | 470 |
| mediatr | C# | 151 | 0.1 s | 1510 |
| zod | TypeScript | 408 | 0.6 s | 680 |
| vuecore | Vue/TS | 535 | 1.3 s | 412 |
| tokio | Rust | 778 | 1.8 s | 53 |
| django | Python | 3,019 | **11.8 s** | 256 |

**Optimisation gains (baseline → final):**

| Repo | Index | Speedup | Sync | Speedup |
|------|:-----:|:-------:|:----:|:-------:|
| tokio (Rust) | 19.4 s → 1.8 s | **11×** | 1.24 s → 40 ms | **31×** |
| django (Python) | 36.2 s → 11.0 s | **3.3×** | 3.13 s → 130 ms | **24×** |

**100k-file scaling:** ~14 min cold-index extrapolated (O(n^1.50), target ≤ 20 min, PASS).
Full analysis: [`benchmark/ANALYSIS-SCALE.md`](benchmark/ANALYSIS-SCALE.md).

**Search latency** (CLI p50, includes binary cold-start):
- Exact / fuzzy / callers / callees: **2–7 ms** on all repos including django (53k nodes)
- Context query: **3–29 ms**
- Via persistent MCP server: sub-millisecond

---

## Languages and frameworks

**18 languages:**

C, C++, C#, Dart, Go, Java, JavaScript, Kotlin, Lua, Luau, Pascal, PHP, Python,
Ruby, Rust, Scala, Swift, TypeScript, Vue, Svelte, Liquid, Razor.

**16 framework resolvers:**

| Framework | Extracted |
|-----------|-----------|
| **Angular** | `@Component` / `@Directive` / `@Pipe` / `@Injectable` / `@NgModule` nodes; DI constructor injection; routing (`loadComponent`, lazy routes, guards); standalone `imports:[]` bindings |
| **ASP.NET / Razor** | HTTP routes (`[HttpGet/Post/Delete]`, minimal API, `MapGroup`, SignalR hubs); DI registrations; namespace-qualified symbols; interface / struct / enum / record discrimination; method signatures; `is_async` detection |
| **Django** | URL patterns, views, models |
| **Express** | Route handlers, middleware chains |
| **FastAPI** | Route decorators, dependency injection |
| **Flask** | Blueprint routes, view functions |
| **NestJS** | Controllers, guards, interceptors, modules |
| **Rails** | Routes (`resources`, `get/post`), controllers |
| **Laravel** | Route facades, controllers |
| **React** | Component exports, hook calls |
| **Vue** | SFC components, `<script setup>`, Composition API |
| **Svelte** | Component exports, stores |
| **Spring** | `@RequestMapping`, `@GetMapping`, beans |
| **Cargo workspace** | Inter-crate `use` resolution, workspace members |
| **Swift Package Manager** | Target dependencies, module imports |
| **Go modules** | Package imports, module graph |

---

## Enterprise

CodeWiki is production-tested on enterprise-scale codebases:

| Metric | Result |
|--------|--------|
| 100k-file cold index | ~14 min (extrapolated from 3k / 10k / 16k measured runs) |
| 100k-file peak RAM | ~4 GB (extrapolated; sub-linear O(n^0.62) memory scaling) |
| orchardcore (5,203 .cs) | 9.2 s index, 505 interfaces, 97 enums, fully qualified names |
| jellyfin (2,065 .cs) | 2.2 s index, 209 interfaces, 6,165 import edges, 385 routes |
| ABP framework (3,497 .cs) | 2.6 s index, 623 interfaces, namespace-qualified (`Volo.Abp.Domain.Services::DomainService`) |
| .NET enterprise verdict | **READY** — interfaces/enums/structs correctly classified, namespaces qualified, signatures stored, async detected |

Full .NET audit: [`benchmark/DOTNET-REPORT.md`](benchmark/DOTNET-REPORT.md).

**Privacy:** all indexing and querying runs locally. No data leaves the machine.
No embedding model, no external API, no telemetry by default. Single static binary —
copy it anywhere, run it, works.

---

## Architecture

Eight-crate Cargo workspace under `crates/`:

| Crate | Role |
|-------|------|
| `codewiki-cli` | Binary entry point, `clap` CLI, installer, onboarding UI |
| `codewiki-mcp` | MCP server (rmcp, stdio JSON-RPC), 9 tool handlers |
| `codewiki-extraction` | tree-sitter AST walker, 18-language extractors, docstring extraction |
| `codewiki-storage` | SQLite schema, FTS5 Unicode search, WAL mode, graph query API |
| `codewiki-resolution` | Import resolver, 16 framework resolvers, name matcher, incremental pipeline |
| `codewiki-sync` | File watcher (`notify`), gitignore walk, git hook installation |
| `codewiki-core` | Shared types: `Node`, `Edge`, `CodeWikiError`, `Config` |
| `codewiki-graph` | Graph web UI (axum HTTP, embedded force-graph frontend) |

Data flow: **tree-sitter extraction** → **SQLite / FTS5** → **reference resolution** → **graph edges** → **MCP tools** / **graph UI**.

---

## Roadmap

- **wiki-md generation** — generate structured Markdown documentation from the graph
  (per-module API docs, architecture diagrams, call-graph narratives) as a first-class
  output format. This is the next major capability after the graph itself.

---

## License

MIT licensed (© 2026 0xsyncroot). See [LICENSE](LICENSE). As a derivative work of
CodeGraph, the CodeGraph attribution and a verbatim copy of its original MIT copyright
notice (© 2026 Colby McHenry) are reproduced in [NOTICE](NOTICE), as the MIT License
requires. Third-party crate and tree-sitter grammar attributions are listed in
[LICENSE-THIRD-PARTY.md](LICENSE-THIRD-PARTY.md) (generated by `cargo about`).

---

## Credits

CodeWiki is a Rust port and derivative of [CodeGraph](https://github.com/colbymchenry/codegraph)
by Colby McHenry (MIT). It reimplements CodeGraph's architecture in Rust — including
the SQLite schema, resolution algorithms, search/scoring, and MCP tool interface —
with added optimisations, framework resolvers (Angular, full ASP.NET / .NET), a graph
web UI, and i18n / Unicode search. See [NOTICE](NOTICE) for the full attribution,
which reproduces CodeGraph's original MIT copyright notice as required.
