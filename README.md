# CodeWiki

**Local code knowledge graph for AI agents. Tree-sitter parsed, SQLite stored, MCP served.**

[![CI](https://github.com/0xsyncroot/codewiki/actions/workflows/ci.yml/badge.svg)](https://github.com/0xsyncroot/codewiki/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.1.1-green.svg)](CHANGELOG.md)
[![Rust](https://img.shields.io/badge/rust-1.78%2B-orange.svg)](https://www.rust-lang.org)

Instead of reading whole source files, your AI agent asks the graph:

```
codewiki_callers("OrderService")   → 3 ms, exact answer
codewiki_impact("AuthService")     → 6 ms, full blast radius
codewiki_context("basket checkout")→ 4 ms, ranked entry points + key code
```

**Measured on real .NET codebases: ~70% fewer tool calls, ~83% fewer tokens, ~$0.012 saved per task.**  
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
- [Contributing](#contributing)
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
- **Fully incremental** — 1-file change syncs in 21–66 ms (even a 2,065-file .NET repo).
- **Docstring extraction** — Python, Rust, Go, TypeScript/JavaScript, C#.
- **100% local** — grammars are bundled in the binary; no network access after install.

---

## Why it pays off

Measured across 5 realistic tasks on eShopOnWeb (254 .cs) and jellyfin (2,065 .cs),
freshly re-run on `codewiki 0.1.1`. All byte counts from real CLI output and real file
sizes; tokens = bytes ÷ 4 (conservative). Pricing: Claude Sonnet $3.00 / 1M input tokens.
Full methodology and per-task breakdown: [`benchmark/README.md`](benchmark/README.md).

| Task | CW calls | Baseline calls | Call reduction | CW tokens | Baseline tokens | Token reduction | $ saved |
|------|:--------:|:--------------:|:--------------:|:---------:|:---------------:|:--------------:|:-------:|
| DI consumers (`IBasketService`) | 2 | 6 | **67%** | 380 | 3,476 | **89%** | $0.009 |
| Feature comprehension (basket checkout) | 1 | 6 | **83%** | 1,017 | 6,842 | **85%** | $0.018 |
| Interface→impls (`IRepository`) | 2 | 6 | **67%** | 624 | 4,219 | **85%** | $0.011 |
| Blast radius (`OrderService` refactor) | 1 | 5 | **80%** | 246 | 1,959 | **87%** | $0.005 |
| Cross-cutting: auth config (jellyfin) | 2 | 4 | **50%** | 2,034 | 8,158 | **75%** | $0.018 |
| **Average** | **1.6** | **5.4** | **70%** | **860** | **4,930** | **83%** | **$0.012** |

A developer session with 20 agent interactions saves roughly **$0.24** while the index
stays current at 21–66 ms per file change. The savings compound: the index is built
once, maintained automatically, and every subsequent query is free. Interface→impl
queries now traverse real `implements` edges — `codewiki impact IRepository` returns the
concrete `EfRepository` implementation directly (Task 3).

---

## Install

CodeWiki ships as a **single self-contained binary** — all tree-sitter grammars are
bundled, with no runtime dependencies. Every GitHub Release attaches CI-built binaries
for five targets, each with a matching SHA-256 checksum. **No toolchain, no compiling,
no cloning required.**

### 1. One-liner (recommended)

The install scripts auto-detect your platform, download the matching pre-built binary
from the **latest** GitHub Release, verify its SHA-256 checksum, and place `codewiki`
on your PATH. Nothing is compiled.

**Linux / macOS:**

```sh
curl -fsSL https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.ps1 | iex
```

**Install location:** `~/.local/bin` on Unix, `%LOCALAPPDATA%\Programs\codewiki` on
Windows (added to your user PATH automatically on Windows). Override with `--dir <path>`
(sh) or `-Dir <path>` (PowerShell).

**Pin a version** instead of taking the latest:

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.sh | sh -s -- --version v0.1.1
```

```powershell
# Windows PowerShell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.ps1))) -Version v0.1.1
```

### 2. Manual download

Prefer not to pipe a script to your shell? Download the asset for your platform from the
[Releases page](https://github.com/0xsyncroot/codewiki/releases), verify it, extract, and
put `codewiki` on your PATH.

| OS | Arch | Release asset |
|----|------|---------------|
| Linux | x86_64 | `codewiki-x86_64-unknown-linux-gnu.tar.gz` |
| Linux | ARM64 | `codewiki-aarch64-unknown-linux-gnu.tar.gz` |
| macOS | Intel | `codewiki-x86_64-apple-darwin.tar.gz` |
| macOS | Apple Silicon | `codewiki-aarch64-apple-darwin.tar.gz` |
| Windows | x86_64 | `codewiki-x86_64-pc-windows-msvc.zip` |

Each asset has a matching `<asset>.sha256`. Download both, then verify and install:

**Linux / macOS:**

```sh
# Replace <asset> with the filename for your platform from the table above.
sha256sum -c <asset>.sha256        # macOS: shasum -a 256 -c <asset>.sha256
tar -xzf <asset>                   # extracts ./codewiki
install -m 755 codewiki ~/.local/bin/codewiki
```

**Windows (PowerShell):**

```powershell
# Verify: the printed hash must match the contents of the .sha256 file.
Get-FileHash codewiki-x86_64-pc-windows-msvc.zip -Algorithm SHA256
Expand-Archive codewiki-x86_64-pc-windows-msvc.zip -DestinationPath $env:LOCALAPPDATA\Programs\codewiki
# Add %LOCALAPPDATA%\Programs\codewiki to your PATH.
```

### Verify the install

```sh
codewiki --version
# codewiki 0.1.1
```

The binary includes every feature — graph UI, FTS5 search, full language support — by
default. No extra flags needed.

<details>
<summary><b>3. Build from source</b> (optional — for development only)</summary>

End users do not need this; the installers above ship a verified pre-built binary.
Building from source requires a stable Rust toolchain (1.78+):

```sh
git clone https://github.com/0xsyncroot/codewiki
cd codewiki
cargo build --release   # binary: ./target/release/codewiki
```

</details>

---

## Quick start

CodeWiki is **wired into your agent once, machine-wide**, and then works in
every project you index. Two steps get you productive:

**1. Wire your agent once (machine-wide).** Run this a single time per machine —
not per project:

```sh
codewiki install --target claude
# (or: cursor | codex | opencode | hermes | all)
```

This registers `codewiki serve --mcp` as an MCP server in your agent's global
config. One registration serves **every** project you open.

**2. Index each project.** In each repository you want code intelligence for:

```sh
codewiki init
# Creates + indexes ./.codewiki/ (DB + git hooks).
# Indexed 141 files, 2025 nodes, 1884 edges in 0.2s.
```

That's it — restart your agent once after step 1, and from then on every
`codewiki init`'d project is instantly available.

**Why this works.** The MCP server is **cwd-aware**: it resolves the
`.codewiki/` index from the project the agent is working in. So a single global
registration transparently serves all your projects. Open a project you haven't
indexed yet and the tools stay live — they simply prompt you to run
`codewiki init` to enable code intelligence there. After you `codewiki init` a
project that was already open, restart your agent (or reconnect the MCP server)
so it picks up the new index.

**Shortcut.** To do both steps for the current project in one command:

```sh
codewiki setup
# Indexes this project and wires your detected agent(s).
```

Verify everything is healthy at any time:

```sh
codewiki doctor
```

`codewiki doctor` runs 6 checks: binary on PATH, index initialized, index non-empty,
freshness, agent wired (global or local), git hook installed.

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

**Measured sync times (1 file changed, fresh on `codewiki 0.1.1`):**

| Repo | Files | Sync time |
|------|------:|:---------:|
| flask (Python) | 83 | 22 ms |
| express (JavaScript) | 141 | 21 ms |
| zod (TypeScript) | 408 | 32 ms |
| json (C++) | 499 | 43 ms |
| jellyfin (C#, 2,065 .cs) | 2,065 | 66 ms |
| django (Python) | 3,020 | 31 ms |

Sync is scoped to the changed file and its direct dependants only, so it stays
proportional to the change size — not the repo size. A 2,065-file .NET repo re-syncs a
single edit in 66 ms.

**The cost story:** index once (~0.2–2.1 s for these repos), then maintain at
21–66 ms per change. Every subsequent query reuses the same graph for sub-ms
responses. The $0.012/task savings repeat every session.

---

## Performance

Full tables, scale analysis, and the .NET agent-cost study live in
[`benchmark/README.md`](benchmark/README.md). All figures below are a fresh run on
`codewiki 0.1.1` (2026-05-25). Machine: 28 cores, 31 GB RAM, WSL2.

**Cross-language results — 8 real repos (one per language, shallow clones of `main`):**

This is a **performance** benchmark — cold-index speed, search latency, and incremental
sync — across languages. (The dollar savings figure is from the .NET agent study in
[*Why it pays off*](#why-it-pays-off); it is **not** a per-language number.)

| Repo | Language | Files | Cold-index | files/s | Search p50 | Sync (1 file) |
|------|----------|------:|:----------:|--------:|:----------:|:-------------:|
| pallets/flask | Python | 83 | 0.19 s | 430 | 3 ms | 22 ms |
| BurntSushi/ripgrep | Rust | 101 | 0.36 s | 278 | 3 ms | 24 ms |
| expressjs/express | JavaScript | 141 | 0.26 s | 542 | 2 ms | 21 ms |
| colinhacks/zod | TypeScript | 408 | 0.57 s | 720 | 2 ms | 32 ms |
| dotnet-architecture/eShopOnWeb | C# | 269 | 0.15 s | 1,793 | 2 ms | 27 ms |
| google/gson | Java | 262 | 0.49 s | 538 | 3 ms | 33 ms |
| nlohmann/json | C++ | 499 | 1.01 s | 494 | 4 ms | 43 ms |
| gin-gonic/gin | Go | 99 | 0.26 s | 376 | 2 ms | 22 ms |

All 8 index cleanly — zero crashes, zero FK errors — including C++ (`nlohmann/json`,
499 files, 13,438 graph nodes). "Search p50" is the `query` latency; `callers`/`callees`
are comparable, `impact`/`context` run 2–33 ms depending on fan-out.

**Scale:** holds across size. django (3,020 Python files) cold-indexes in ~5.8 s into a
52,722-node / 165,582-edge graph (fresh); the 100k-file cold-index extrapolates to
**~14 min** (O(n^1.50), target ≤ 20 min — PASS) at ~4 GB peak RSS. Full scale table:
[`benchmark/README.md`](benchmark/README.md).

**Search latency** (CLI p50, includes binary cold-start):
- Exact / fuzzy / callers / callees: **2–4 ms** across all 8 languages
- Impact / context query: **2–33 ms** (varies with graph fan-out)
- Via persistent MCP server: sub-millisecond

---

## Languages and frameworks

**18 languages:**

C, C++, C#, Dart, Go, Java, JavaScript, Kotlin, Lua, Pascal, PHP, Python,
Ruby, Rust, Scala, Swift, TypeScript, and Vue.

Plus additional grammars for component and template dialects — Svelte, Luau, Liquid,
and Razor.

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

All figures below are fresh `codewiki 0.1.1` runs on shallow clones (2026-05-25):

| Metric | Result |
|--------|--------|
| 100k-file cold index | ~14 min (extrapolated from 3k / 10k / 16k measured runs) |
| 100k-file peak RAM | ~4 GB (extrapolated; sub-linear O(n^0.62) memory scaling) |
| OrchardCore (5,203 .cs) | 9.0 s index, 72,345 nodes / 227,444 edges, 505 interfaces, 97 enums, 3,663 implements edges, fully qualified names |
| jellyfin (2,065 .cs) | 2.1 s index, 19,911 nodes / 46,648 edges, 209 interfaces, 385 routes, 5,852 import edges, 1,019 async methods |
| ABP framework (3,497 .cs) | 1.9 s index, 26,133 nodes / 50,632 edges, 623 interfaces, namespace-qualified (`Volo.Abp.Domain.Services::DomainService`) |
| .NET enterprise verdict | **READY** — interfaces/enums/structs correctly classified, namespaces qualified, signatures stored, async detected, `implements` edges resolved |

Full benchmark suite and .NET audit: [`benchmark/README.md`](benchmark/README.md).

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

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build, test, and
pull-request guidelines, and the [code of conduct](CODE_OF_CONDUCT.md) for community
expectations. To report a security issue, follow the process in [SECURITY.md](SECURITY.md).
Benchmark methodology and how to reproduce the numbers live in
[`benchmark/README.md`](benchmark/README.md).

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
