# Contributing to CodeWiki

Thanks for your interest in CodeWiki! Contributions of all kinds are welcome —
bug reports, documentation fixes, new language support, framework resolvers, and
performance work. This guide explains how to get set up, what the local quality
gates are, and the conventions we follow.

CodeWiki is a Rust port and derivative of [CodeGraph](https://github.com/colbymchenry/codegraph)
by Colby McHenry (MIT). See [NOTICE](NOTICE) for the full attribution.

---

## Getting started

You'll need a stable Rust toolchain (Rust 1.78+) with `rustfmt` and `clippy`
installed (they ship with `rustup` by default):

```sh
rustup component add rustfmt clippy
```

Clone and build:

```sh
git clone https://github.com/0xsyncroot/codewiki
cd codewiki
cargo build --release
# binary: ./target/release/codewiki
```

Run the full test suite:

```sh
cargo test --workspace
```

---

## Pre-PR gates (mandatory)

CI runs these checks and so should you, locally, before opening a pull request.
**Both must pass with zero output / zero errors** — a PR that fails either will not
be merged:

```sh
# 1. Formatting must be clean
cargo fmt --all -- --check

# 2. Clippy must be warning-free (warnings are treated as errors)
cargo clippy --all-targets --all-features -- -D warnings

# 3. Tests must pass
cargo test --workspace
```

If `cargo fmt --all -- --check` reports diffs, run `cargo fmt --all` to fix them
automatically. Clippy findings should be fixed at the source rather than silenced;
if a lint genuinely needs an `#[allow(...)]`, add a short comment explaining why.

---

## Workspace layout

CodeWiki is an eight-crate Cargo workspace under `crates/` (plus a shared test-helper
crate). Each crate has a single, well-defined responsibility:

| Crate | Role |
|-------|------|
| `codewiki-cli` | Binary entry point, `clap` CLI, installer, onboarding UI |
| `codewiki-mcp` | MCP server (rmcp, stdio JSON-RPC), tool handlers |
| `codewiki-extraction` | tree-sitter AST walker, per-language extractors, docstring extraction |
| `codewiki-storage` | SQLite schema, FTS5 Unicode search, WAL mode, graph query API |
| `codewiki-resolution` | Import resolver, framework resolvers, name matcher, incremental pipeline |
| `codewiki-sync` | File watcher (`notify`), gitignore walk, git hook installation |
| `codewiki-core` | Shared types: `Node`, `Edge`, `CodeWikiError`, `Config` |
| `codewiki-graph` | Graph web UI (axum HTTP, embedded force-graph frontend) |
| `codewiki-testutil` | Shared test fixtures and helpers (dev-dependency only) |

The data flow is: **tree-sitter extraction** (`codewiki-extraction`) →
**SQLite / FTS5** (`codewiki-storage`) → **reference resolution**
(`codewiki-resolution`) → **graph edges** → **MCP tools** (`codewiki-mcp`) /
**graph UI** (`codewiki-graph`). See the
[Architecture section of the README](README.md#architecture) for more detail.

---

## How to add a language

Language support lives in `codewiki-extraction`. Adding a new language involves a
tree-sitter grammar plus an extractor:

1. **Add the grammar.** Wire in the language's tree-sitter grammar so it is bundled
   in the binary (grammars are compiled in — CodeWiki makes no network calls at
   runtime). Follow how existing languages register their grammar.
2. **Write the extractor.** Add an extractor that walks the parsed AST and emits
   `Node`s (functions, classes, interfaces, enums, structs, etc.) and the typed
   `Edge`s between them (calls, imports, inherits, implements). Use an existing
   extractor for a structurally similar language as a template.
3. **Map file extensions** to the new language so the indexer routes the right files
   to your extractor.
4. **Add tests** with a small fixture source file and assert the expected nodes and
   edges are produced. Docstring extraction, if the language supports it, should be
   covered too.

---

## How to add a framework resolver

Framework resolvers live in `codewiki-resolution/src/framework/`. There are 16
resolvers today; use [`angular.rs`](crates/codewiki-resolution/src/framework/angular.rs)
and [`csharp.rs`](crates/codewiki-resolution/src/framework/csharp.rs) as references —
they cover the richer cases (decorators / attributes, DI, routing).

1. Create a new module under `crates/codewiki-resolution/src/framework/` (e.g.
   `myframework.rs`), following the structure of an existing resolver.
2. Register it in `crates/codewiki-resolution/src/framework/mod.rs`.
3. Resolve framework-specific constructs into the graph — for example HTTP routes to
   handler edges, dependency-injection bindings, component/module registrations, or
   convention-based wiring that plain import resolution can't see.
4. Reuse the shared helpers in `scan_utils.rs` where they fit rather than
   re-implementing scanning logic.
5. Add tests against a representative fixture and assert the framework-specific edges
   are produced.

---

## Commit and PR conventions

- **Commit messages:** write a concise, imperative subject line (e.g. "Add Zig
  language extractor", "Fix off-by-one in callers query"). Keep the subject under
  ~72 characters and add a body explaining the *why* when the change isn't obvious.
  Conventional-commit-style prefixes (`feat:`, `fix:`, `perf:`, `docs:`) are welcome
  but not required.
- **Pull requests:** fill out the PR template. Keep PRs focused — one logical change
  per PR is much easier to review. Make sure the three pre-PR gates above pass, update
  the README/docs if behavior changes, and add an entry under the **Unreleased**
  section of [`CHANGELOG.md`](CHANGELOG.md).
- **Tests:** new features and bug fixes should come with tests. Bug fixes ideally
  include a regression test that fails before the fix.
- **Sign-off / DCO:** optional. If you like, add a `Signed-off-by:` line via
  `git commit -s`, but it is not required to contribute.

---

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By participating,
you agree to uphold it. Report unacceptable behavior to work.hiepht@gmail.com.

---

Questions or unsure where to start? Open a
[discussion or issue](https://github.com/0xsyncroot/codewiki/issues) — we're happy
to help. Thanks for contributing!
