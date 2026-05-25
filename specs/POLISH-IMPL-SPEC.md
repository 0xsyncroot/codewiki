# POLISH-IMPL-SPEC — Unified Docstring + FTS i18n Implementation Plan

**Status:** Build contract — do not implement before this is signed off  
**Replaces:** `DOCSTRING-SPEC.md` + `FTS-I18N-SPEC.md` (both remain as research references)  
**Schema target:** v6 (current `CURRENT_SCHEMA_VERSION` = 5, confirmed in `migrations.rs` line 9)  
**Verdict:** GREEN with two YELLOW flags noted at the end

---

## 1. Confirmed Codebase State

All facts below were verified directly from source before writing this plan.

| Fact | Confirmed value |
|---|---|
| `CURRENT_SCHEMA_VERSION` | **5** — inode migration, `migrations.rs` line 9 |
| `nodes_fts` tokenizer today | Implicit `unicode61 remove_diacritics=1` — no tokenize clause in `schema.sql` lines 97–105 |
| `docstring` column in `nodes` | Exists (`TEXT`, nullable), line 31 of `schema.sql` |
| `docstring` in `nodes_fts` | Yes — indexed by FTS, trigger `nodes_ai` at `schema.sql` line 109 |
| `insert_nodes_batch` | Delegates to `insert_node` in a loop — `queries/nodes.rs` lines 111–116. All text fields bound at lines 90–107 |
| `parse_query` location | `crates/codewiki-storage/src/search/query_parser.rs`, line 116 |
| `search/mod.rs` re-export | `pub use query_parser::{bounded_edit_distance, parse_query, ParsedQuery};` — `search/mod.rs` line 4 |
| No `normalize_for_fts` exists | Function does not exist anywhere in the codebase |
| `unicode-normalization` crate | **Already in workspace `Cargo.toml`** and in `codewiki-extraction/Cargo.toml` — NOT yet in `codewiki-storage/Cargo.toml` |
| `LanguageConfig` struct | `ast_walker.rs` lines 23–46; no `doc_comment_style` field yet |
| `emit_node` signature | Already accepts `docstring: Option<String>` — `ast_walker.rs` line 102. All call-sites currently pass `None` |
| `codewiki-graph` `queries/nodes.rs` changes | `get_top_nodes_by_degree` + `DegreeMetric` were added to `queries/nodes.rs` (lines 305–388) by the graph work. **This file has already received graph additions** |
| `queries/meta.rs` | Graph added `get_edges_by_kind` — separate file, no conflict with docstring/FTS work |

---

## 2. Consolidation Decisions

### 2.1 Sequencing: docstring first, FTS migration second

Docstring extraction populates `node.docstring` at extraction time via `emit_node`. The FTS v6 migration's `INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild')` repopulates `nodes_fts` entirely from the live `nodes` table. Therefore:

**Correct order:** docstring extraction lands first → FTS v6 migration lands second (or in the same release, strictly after in migration ordering).

If both land in the same release, the migration runner's sequential version ordering guarantees the rebuild sees already-populated docstrings — provided the user runs `codewiki sync` (which calls `insert_nodes_batch`) before the first query. For existing user DBs that have already indexed files, a `codewiki reindex --force` is required to backfill docstrings; the v6 rebuild will then index the backfilled text correctly. Document this in the release note.

**Do NOT reverse the order.** If the v6 rebuild fires before docstrings are extracted, the FTS index will be built on NULL docstrings — correct but suboptimal. The rebuild is cheap (< 1 s for 2000–5000 nodes) but running it twice wastes work. A single combined release with docstring-then-migration ordering is the right model.

### 2.2 `normalize_for_fts` — one home, no duplicate

**Home: `crates/codewiki-storage/src/search/query_parser.rs`**

Rationale: `query_parser.rs` already owns all query-time text transforms (`parse_query`, `bounded_edit_distance`). The write path (`insert_node` in `queries/nodes.rs`) already imports from `crates/codewiki-storage` internally. Making `normalize_for_fts` a `pub fn` in `query_parser.rs` and re-exporting it from `search/mod.rs` gives both consumers a single import path with zero cross-crate complexity.

The function is NOT placed in `codewiki-core` or `codewiki-extraction`. `codewiki-extraction` already has `unicode-normalization` in its Cargo.toml — but the extraction layer must not call `normalize_for_fts` at extraction time. Normalization belongs at the storage write boundary, not in the CST walker. Docstrings should be stored as clean prose (markers stripped, whitespace trimmed) and normalized only when written to the `nodes` table.

**`codewiki-storage/Cargo.toml` must add:**
```toml
unicode-normalization = { workspace = true }
```

No other crate needs this addition for this feature.

### 2.3 Migration version

FTS migration is **v6**. `CURRENT_SCHEMA_VERSION` must be bumped from 5 to 6. No other pending migration claims v6 — confirmed by reading `migrations.rs` which only defines v2–v5.

### 2.4 `queries/nodes.rs` sequencing after graph work

The graph work already added `get_top_nodes_by_degree`, `DegreeMetric`, and their test to `queries/nodes.rs`. That file has diverged from what a pre-graph branch would see. The docstring/FTS implement **must rebase on the current `queries/nodes.rs`** (the post-graph version) before touching `insert_node`. The change to `insert_node` is localized to lines 90–101 (the `params![]` call) — it does not conflict with `get_top_nodes_by_degree` (lines 305–388), but a clean rebase is required to avoid a merge conflict on this single file.

`queries/meta.rs` and `queries/edges.rs` are untouched by this work.

---

## 3. Implementation Order

### Step 1 — `normalize_for_fts` utility (codewiki-storage)

**Files touched:**
- `crates/codewiki-storage/Cargo.toml` — add `unicode-normalization = { workspace = true }`
- `crates/codewiki-storage/src/search/query_parser.rs` — add `pub fn normalize_for_fts(input: &str) -> String`
- `crates/codewiki-storage/src/search/mod.rs` — add `pub use query_parser::normalize_for_fts;` to the existing re-export line

**Function contract:**
```rust
/// Normalize text for FTS matching.
///
/// Two-pass:
///   1. NFD-decompose and strip Unicode combining marks (U+0300–U+036F,
///      U+1DC0–U+1DFF, U+20D0–U+20FF) so that `remove_diacritics=2`-indexed
///      tokens are reachable from accented input and vice versa.
///   2. Explicit character map for non-decomposable Latin strokes:
///        đ (U+0111) → d,  Đ (U+0110) → D
///        ø (U+00F8) → o,  Ø (U+00D8) → O
///        ł (U+0142) → l,  Ł (U+0141) → L
///
/// Applied at both write time (insert_node) and query time (parse_query).
/// ASCII input (U+0000–U+007F) is returned unchanged.
pub fn normalize_for_fts(input: &str) -> String
```

Implementation uses `unicode_normalization::UnicodeNormalization::nfd()`. Combining-mark detection: check `unicode_normalization::char::is_combining_mark(c)` or match char ranges U+0300–U+036F directly (the main block covers all Vietnamese combining marks). After the combining-mark filter, apply the six-entry explicit map with a `match` expression.

### Step 2 — Wire `normalize_for_fts` at write time (codewiki-storage)

**Files touched:**
- `crates/codewiki-storage/src/queries/nodes.rs` — in `insert_node`, wrap `name`, `qualified_name`, and `docstring` before binding as SQL parameters

**Exact change** (rebase on post-graph `nodes.rs`):
```rust
// In insert_node, replace the params! bindings for name/qualified_name/docstring:
use crate::search::normalize_for_fts;

let norm_name = normalize_for_fts(&node.name);
let norm_qname = normalize_for_fts(&node.qualified_name);
let norm_doc = node.docstring.as_deref().map(normalize_for_fts);

stmt.execute(params![
    node.id,
    node_kind_str(node),
    norm_name,       // was: node.name
    norm_qname,      // was: node.qualified_name
    node.file_path,
    node_lang_str(node),
    ...
    norm_doc,        // was: node.docstring
    node.signature,
    ...
])?;
```

`node.signature` is NOT normalized — signatures contain type syntax where diacritic folding could corrupt meaning (e.g. a type named `Đào` is a real identifier; the FTS engine's `remove_diacritics=2` handles query-side folding for signature matches).

**Note:** `insert_nodes_batch` calls `insert_node` in a loop — no change needed there.

### Step 3 — Wire `normalize_for_fts` at query time (codewiki-storage)

**Files touched:**
- `crates/codewiki-storage/src/search/query_parser.rs` — apply `normalize_for_fts` to `out.text` at the end of `parse_query`

**Exact change:**
```rust
// At the end of parse_query, before returning:
out.text = normalize_for_fts(out.text.trim());
out
```

This is a one-liner inserted at line 207, after `out.text = text_parts.join(" ").trim().to_string();`. All callers of `parse_query` — including `search_nodes_fts` in `search/mod.rs` — benefit automatically.

### Step 4 — Docstring extraction: `DocCommentStyle` + `extract_docstring` (codewiki-extraction)

**Files touched:**
- `crates/codewiki-extraction/src/ast_walker.rs` — add `DocCommentStyle` enum, add `doc_comment_style: DocCommentStyle` field to `LanguageConfig`, implement `pub fn extract_docstring`

**`DocCommentStyle` variants** (as designed in DOCSTRING-SPEC §3a — reproduced here as the contract):

```rust
#[derive(Clone, Copy)]
pub enum DocCommentStyle {
    None,
    PrecedingLineComment { node_kind: &'static str, prefix: &'static str },
    PrecedingBlockComment { node_kind: &'static str, prefix: &'static str },
    PrecedingEither {
        block_node_kind: &'static str,
        block_prefix: &'static str,
        line_node_kind: &'static str,
        line_prefix: &'static str,
    },
    PythonFirstBodyString { body_field: &'static str },
}
```

`LanguageConfig` gains:
```rust
pub doc_comment_style: DocCommentStyle,
```

Default for all existing `LanguageConfig` statics that are not yet updated: `DocCommentStyle::None` (use a `Default` impl on the struct or set explicitly).

**`extract_docstring` rules:**
- Blank-line gap check: `decl_node.start_position().row > candidate.end_position().row + 1` → return `None`
- For `PrecedingLineComment`: walk `prev_named_sibling()` chain collecting same-kind, same-prefix, gap-free nodes; reverse; concatenate after stripping prefix + one optional space per line
- For `PrecedingBlockComment`: collect one sibling; strip `/**`/`*/`/leading ` * ` per Javadoc pattern
- For `PrecedingEither`: try block first (one sibling), then line chain; whichever is immediately adjacent wins
- For `PythonFirstBodyString`: `child_by_field_name("body")` → first named child → `expression_statement` → `string` child → strip delimiters + dedent
- Cap all output at 2000 characters
- On any error or mismatch: return `None` silently

### Step 5 — Per-language `CONFIG` updates (codewiki-extraction)

**Files touched** (all in `crates/codewiki-extraction/src/languages/`):

| File | `doc_comment_style` value |
|---|---|
| `rust_lang.rs` | `PrecedingEither { block_node_kind: "block_comment", block_prefix: "/**", line_node_kind: "line_comment", line_prefix: "///" }` |
| `python.rs` | `PythonFirstBodyString { body_field: "body" }` |
| `javascript.rs` | `PrecedingBlockComment { node_kind: "comment", prefix: "/**" }` |
| `typescript.rs` | `PrecedingBlockComment { node_kind: "comment", prefix: "/**" }` |
| `go.rs` | `PrecedingLineComment { node_kind: "comment", prefix: "//" }` |
| `java.rs` | `PrecedingBlockComment { node_kind: "block_comment", prefix: "/**" }` |
| `csharp.rs` | `PrecedingLineComment { node_kind: "comment", prefix: "///" }` |
| `kotlin.rs` | `PrecedingBlockComment { node_kind: "multiline_comment", prefix: "/**" }` |
| `swift.rs` | `PrecedingEither { block_node_kind: "multiline_comment", block_prefix: "/**", line_node_kind: "comment", line_prefix: "///" }` |
| `php.rs` | `PrecedingBlockComment { node_kind: "comment", prefix: "/**" }` |
| `ruby.rs` | `PrecedingLineComment { node_kind: "comment", prefix: "#" }` |
| `c.rs` | `PrecedingBlockComment { node_kind: "comment", prefix: "/**" }` |
| `cpp.rs` | `PrecedingEither { block_node_kind: "comment", block_prefix: "/**", line_node_kind: "comment", line_prefix: "///" }` |
| `scala.rs` | `PrecedingBlockComment { node_kind: "block_comment", prefix: "/**" }` |
| `dart.rs` | `PrecedingLineComment { node_kind: "documentation_comment", prefix: "///" }` |
| `lua.rs` | `PrecedingLineComment { node_kind: "comment", prefix: "---" }` |
| `luau.rs` | `PrecedingLineComment { node_kind: "comment", prefix: "--!" }` |

### Step 6 — Wire `extract_docstring` at all `emit_node` call-sites (codewiki-extraction)

**Files touched:**
- `crates/codewiki-extraction/src/ast_walker.rs` — the generic walkers (`extract_function`, `extract_method`, `extract_class`, `extract_struct`, `extract_interface`, `extract_enum`, `extract_namespace`)
- Any language-specific hooks that construct `Node { ..., docstring: None, ... }` directly

`emit_node` already accepts `docstring: Option<String>` — confirmed at `ast_walker.rs` line 102. No signature change needed. Replace every `None` argument for `docstring` with:
```rust
extract_docstring(&ts_node, ctx.source.as_bytes(), &ctx.config.doc_comment_style)
```

For `PythonFirstBodyString`, the `body_field` is already available on `ctx.config.body_field`, so `extract_docstring` can receive the config ref directly without extra plumbing.

### Step 7 — FTS v6 migration (codewiki-storage)

**Files touched:**
- `crates/codewiki-storage/src/schema.sql` — update `nodes_fts` DDL (new-DB path)
- `crates/codewiki-storage/src/migrations.rs` — add v6 migration entry, bump `CURRENT_SCHEMA_VERSION` to 6

**`schema.sql` change** (lines 97–105): add `tokenize='unicode61 remove_diacritics 2'` as the final option inside the FTS5 definition. This applies to new databases only; existing DBs are handled by the migration.

**Migration v6 SQL block** — the full drop-triggers → drop-table → recreate-with-tokenizer → rebuild → recreate-triggers sequence as specified in FTS-I18N-SPEC §4.2. No alteration is needed; the spec SQL is correct and compatible with the existing `run_migrations` statement-split loop (`INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild')` contains no internal semicolon).

**Transaction safety note:** The existing `run_migrations` runner does NOT wrap each migration in a `BEGIN`/`COMMIT`. For v6 this means an interrupted migration could leave the DB without `nodes_fts`. Mitigate by adding an explicit `BEGIN;` at the start and `COMMIT;` at the end of the v6 SQL block. The statement-split loop will execute these as real statements — SQLite accepts `BEGIN`/`COMMIT` via `execute_batch`. If `BEGIN`/`COMMIT` causes issues with the existing runner logic for DDL (SQLite auto-commits DDL by default), at minimum document in code that an interrupted v6 migration requires `codewiki init --force` to recover.

---

## 4. Combined Test Plan

All tests should pass with `cargo test -p codewiki-storage -p codewiki-extraction` before merging.

### T-NRM: normalize_for_fts unit tests (query_parser.rs)

| Test | Input | Expected output |
|---|---|---|
| `normalize_ascii_passthrough` | `"getUserById"` | `"getUserById"` unchanged |
| `normalize_vn_combining` | `"người"` | `"nguoi"` |
| `normalize_vn_d_stroke` | `"đăng"` | `"dang"` |
| `normalize_vn_combined` | `"đường"` | `"duong"` |
| `normalize_polish` | `"łódź"` | `"lodz"` |
| `normalize_idempotent` | `normalize_for_fts(normalize_for_fts("người"))` | `"nguoi"` (no double-strip artifacts) |

### T-FTS: FTS search integration tests (search/mod.rs)

| Test | Setup | Query | Assert |
|---|---|---|---|
| `fts_english_ascii_unaffected` | Insert node name `"TransportService"` | `"TransportService"` | Non-empty; top result is that node |
| `fts_vn_unaccented_matches_accented` | Insert node name `"người_dùng"` | `"nguoi_dung"` | Non-empty; top result matches |
| `fts_vn_d_stroke_matches` | Insert node name `"đăng_nhập"` | `"dang_nhap"` | Non-empty; top result matches |
| `fts_vn_docstring_searchable` | Insert node with docstring `"Trả về danh sách đơn hàng"` | `"don hang"` | Non-empty; top result matches |

### T-MIG: Migration v5→v6 (migrations.rs or search/mod.rs test)

| Test | Steps | Assert |
|---|---|---|
| `fts_migration_v6_rebuild` | Fresh in-memory DB → apply schema → run migrations v2–v5 → insert Vietnamese nodes → run v6 migration → FTS query for unaccented form | Nodes found; `nodes_fts` exists; schema_versions shows version 6 |
| `fts_migration_idempotent_after_v6` | Run `run_migrations` twice on v6 DB | No error; version still 6 |

### T-DS: Docstring extraction tests (codewiki-extraction/tests/ or per-language module)

The seven tests from DOCSTRING-SPEC §7 are the contract:

| Test ID | Language | Scenario | Key assertion |
|---|---|---|---|
| T-DS-1 | Python | Triple-quoted function docstring | Markers stripped, whitespace dedented |
| T-DS-2 | TypeScript | JSDoc `/** */` block | `/**`/`*/`/` * ` stripped; `@param` lines kept |
| T-DS-3 | Rust | `///` line run | `/// ` prefix stripped per line; blank line preserved |
| T-DS-4 | Go | Godoc `//` run | `// ` prefix stripped; two-line run joined |
| T-DS-5 | C# | `///` XML doc | Raw XML tags kept (rendering concern, not storage) |
| T-DS-6 | Rust | Comment with blank line before decl | `node.docstring == None` (gap check fires) |
| T-DS-7 | TypeScript | `// TODO:` comment before function | `node.docstring == None` (non-`/**` excluded by `PrecedingBlockComment`) |

### T-REG: Regression — node counts unchanged

Run `cargo test` on all extraction golden tests. Node counts must be identical before and after Steps 4–6. If any test uses a hardcoded node count, it must not change (docstring is a field, not a new node).

---

## 5. File Touch Summary

```
crates/codewiki-storage/
  Cargo.toml                                  ADD: unicode-normalization
  src/schema.sql                              MODIFY: nodes_fts tokenize clause (new-DB path)
  src/migrations.rs                           ADD: v6 migration + bump CURRENT_SCHEMA_VERSION
  src/search/query_parser.rs                  ADD: normalize_for_fts fn
                                              MODIFY: parse_query — normalize out.text
  src/search/mod.rs                           MODIFY: re-export normalize_for_fts
  src/queries/nodes.rs                        MODIFY: insert_node — normalize name/qname/docstring
  *** MUST REBASE ON POST-GRAPH VERSION ***

crates/codewiki-extraction/
  src/ast_walker.rs                           ADD: DocCommentStyle enum
                                              MODIFY: LanguageConfig — add doc_comment_style field
                                              ADD: extract_docstring fn
                                              MODIFY: extract_function, extract_method,
                                                      extract_class, extract_struct,
                                                      extract_interface, extract_enum,
                                                      extract_namespace — pass extract_docstring(...)
  src/languages/rust_lang.rs                  MODIFY: CONFIG doc_comment_style
  src/languages/python.rs                     MODIFY: CONFIG doc_comment_style
  src/languages/javascript.rs                 MODIFY: CONFIG doc_comment_style
  src/languages/typescript.rs                 MODIFY: CONFIG doc_comment_style
  src/languages/go.rs                         MODIFY: CONFIG doc_comment_style
  src/languages/java.rs                       MODIFY: CONFIG doc_comment_style
  src/languages/csharp.rs                     MODIFY: CONFIG doc_comment_style
  src/languages/kotlin.rs                     MODIFY: CONFIG doc_comment_style
  src/languages/swift.rs                      MODIFY: CONFIG doc_comment_style
  src/languages/php.rs                        MODIFY: CONFIG doc_comment_style
  src/languages/ruby.rs                       MODIFY: CONFIG doc_comment_style
  src/languages/c.rs                          MODIFY: CONFIG doc_comment_style
  src/languages/cpp.rs                        MODIFY: CONFIG doc_comment_style
  src/languages/scala.rs                      MODIFY: CONFIG doc_comment_style
  src/languages/dart.rs                       MODIFY: CONFIG doc_comment_style
  src/languages/lua.rs                        MODIFY: CONFIG doc_comment_style
  src/languages/luau.rs                       MODIFY: CONFIG doc_comment_style
```

No files in `codewiki-mcp`, `codewiki-cli`, `codewiki-resolution`, `codewiki-graph`, or `codewiki-core` are touched by this work.

---

## 6. What Does NOT Change

- Node counts — docstring is a field population, not a new node
- Edge counts / resolution logic — zero impact
- `NodeKind`, `Language`, or any `codewiki-core` types
- MCP tool handlers — `codewiki_node` already renders `node.docstring` when non-None; no change needed
- BM25 weights `(0,20,5,1,2)` — tokenizer change is symmetric; relative scores are preserved
- Existing user data in the `nodes` table — v6 migration does not touch `nodes`
- ASCII identifier search — `remove_diacritics=2` has zero effect on U+0000–U+007F
- `codewiki-graph` — no intersection; graph's `queries/nodes.rs` additions are in functions this work does not modify

---

## 7. Verdict

**GREEN** — the two specs are fully compatible. All five consolidation points resolve cleanly:

1. **Ordering:** docstring extraction → normalize utility → FTS v6 migration. Single combined release is safe; the migration rebuild picks up populated docstrings if users run a full reindex.
2. **`normalize_for_fts` home:** `query_parser.rs` in `codewiki-storage`. One definition, two call-sites (`insert_node` and `parse_query`), re-exported via `search/mod.rs`. No duplicate.
3. **Migration version:** v6 confirmed. No conflicts.
4. **Graph sequencing:** `queries/nodes.rs` already has graph additions. Rebase required before touching `insert_node` — no functional conflict, purely a merge discipline item. **YELLOW flag.**
5. **Regression safety:** Node counts unchanged. English/ASCII search unaffected. v5→v6 migration preserves all `nodes` data. The FTS rebuild is the only materialized change to stored state.

**YELLOW flag 1 — `queries/nodes.rs` rebase discipline.** This is a merge-order constraint, not a correctness risk. The implementing engineer must start from the current `HEAD` of this file (which includes `get_top_nodes_by_degree` and `DegreeMetric`) and apply the `insert_node` normalization on top. Any branch that was cut before the graph work must rebase before opening a PR.

**YELLOW flag 2 — `codewiki-storage` missing `unicode-normalization` dependency.** The crate is in the workspace and in `codewiki-extraction`, but has not yet been added to `codewiki-storage/Cargo.toml`. This is a required change in Step 1; forgetting it produces a clean compile error (cannot miss it), but it must be the first line in the PR diff to unblock the rest.

No must-fix blocking issues. Both features can ship in a single PR in the order defined by Steps 1–7.
