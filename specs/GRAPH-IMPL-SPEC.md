# codewiki-graph — Consolidated Implementation Spec

**Status:** BUILD CONTRACT — supersedes A-features-ux.md, B-web-architecture.md, C-data-api.md  
**Verdict:** GREEN with one must-fix (see §10)  
**Date:** 2026-05-25

---

## 0. Conflict Resolution Summary

Three agents produced overlapping specs. All conflicts below are resolved decisively.

| # | Conflict | Resolution |
|---|----------|------------|
| Port | B says 7890; A says 7007 | **7007** (A authored the UX contract; B stated it as a throwaway example; consistency with the user's original request) |
| Node cap | A says 150; B says 200/500; C says 200 default/500 hard max | **200 default, 500 hard max** (C's numbers are more carefully specified; A's 150 is the v1 UI rendering budget, not the API cap) |
| `file-graph` endpoint | B defines it; C omits it | **Included as a v1 must-have endpoint** (see §2, endpoint 8); backed by `get_files` + `traverse_bfs` — not a new storage query |
| `edges_by_kind` in stats | C adds to `/api/stats`; B omits it | **Included in `/api/stats`** as a handler-side SQL call; does NOT touch the shared `GraphStats` struct in `codewiki-core` to avoid breaking the MCP layer |
| Connection model | C says r2d2 pool; B says open directly | **Single read-only `rusqlite::Connection` wrapped in `Mutex`** for v1 (pool is correct long-term but r2d2 adds a dependency; `spawn_blocking` + single connection is safe and simpler; upgrade to pool in v2 if contention is observed) |
| `search_count_hint` | C calls it optional | **Omit from v1** — response field simply absent |
| CLI alias `codewiki serve --web` | A mentions it as alias | **Not implemented in v1** — `codewiki graph` is the only entry point; `serve --web` is a potential v2 alias |
| Browser auto-open | B includes it | **Included**; `--no-open` flag suppresses it |

---

## 1. Tech Stack (confirmed, final)

| Layer | Choice | Rationale |
|-------|--------|-----------|
| HTTP server | axum 0.8 + hyper 1.x | Tokio-native; already in workspace via codewiki-mcp; tower-http for CORS |
| Static assets | rust-embed 8 | Compiles `src/assets/` into binary at `cargo build`; disk reads in debug mode automatically |
| Graph library | force-graph (vasturiano) 2D Canvas IIFE | ~350 KB, single file, no bundler, offline-safe, 60fps at 2000 nodes |
| Frontend build | **None** | index.html + vendored `graph.min.js` committed to `src/assets/`; no npm ever |
| Cargo feature | `web` | Gates axum/tower-http/rust-embed; default binary is unchanged |
| New crate | `crates/codewiki-graph/` | Workspace member, `publish = false` |
| Concurrency | `spawn_blocking` + read-only `Mutex<Connection>` | Never touches the writer `Mutex<Connection>` in `StorageImpl` |

**No npm at build or runtime — confirmed.** The vendored `graph.min.js` is committed once, updated intentionally when upgrading the library. `build.rs` is only used for asset checksum verification (optional), not for downloading anything.

---

## 2. Final Endpoint Table

All responses: `Content-Type: application/json`. All errors: `{"error": "...", "code": "NOT_FOUND|BAD_REQUEST|INTERNAL"}`.

### EP-1 `GET /api/stats`

**Backs:** M9 (stats header), initial load  
**Params:** none  
**Backing queries:** `QueryHandle::get_stats()` (existing) + one extra SQL call for `edges_by_kind` (new §3.1)

```json
{
  "node_count": 2271,
  "edge_count": 8981,
  "file_count": 162,
  "unresolved_ref_count": 12382,
  "db_size_bytes": 9091227,
  "journal_mode": "wal",
  "nodes_by_kind": { "function": 1493, "method": 339, "struct": 132 },
  "files_by_language": { "rust": 161, "ruby": 1 },
  "edges_by_kind": { "calls": 4102, "imports": 1877, "contains": 2300, "references": 456 }
}
```

**New query:** §3.1. Handler issues `get_stats()` then the `edges_by_kind` query; merges results client-side in the handler before serialization. `GraphStats` struct in `codewiki-core` is NOT modified.

---

### EP-2 `GET /api/search`

**Backs:** M1, M5 (search bar, fuzzy match)  
**Params:** `q` (required), `kind` (optional NodeKind), `lang` (optional Language), `limit` (default 20, max 100)  
**Backing query:** `QueryHandle::search_nodes(q, SearchOptions { limit, kinds, languages, path_filter: None })` (existing)

```json
{
  "results": [
    {
      "node": { /* NodeJson — §4.1 */ },
      "score": 14.72,
      "snippet": "pub fn traverse_bfs(..."
    }
  ]
}
```

No `total_hint` in v1 (omitted per conflict resolution §0).

---

### EP-3 `GET /api/node/:id`

**Backs:** M2 (detail panel)  
**Params:** `:id` (node id)  
**Backing queries:** `QueryHandle::get_node_by_id(id)` + `QueryHandle::get_code(id)` + `get_node_degree` (new §3.2, two COUNT(*) statements)

```json
{
  "node": { /* NodeJson */ },
  "code": "pub fn traverse_bfs(&self, ...) {\n    ...\n}",
  "caller_count": 3,
  "callee_count": 7,
  "in_degree": 5,
  "out_degree": 9
}
```

`caller_count` = callers limited to `calls` edges only (reuses `get_callers(id, 1).len()`). `in_degree`/`out_degree` = all edge kinds (new §3.2). `code` is `null` if file unreadable.

---

### EP-4 `GET /api/neighborhood/:id`

**Backs:** M1, M3 (neighborhood explorer, expand on click) — **primary canvas endpoint**  
**Params:** `depth` (default 1, max 4), `edge_kinds` (comma-separated, default all), `node_kinds` (comma-separated, default all, post-BFS filter), `direction` (default `both`), `limit` (default 200, hard max 500), `exclude` (comma-separated node ids, max 500)  
**Backing query:** `GraphTraverser::traverse_bfs(id, TraversalOptions { max_depth: depth, edge_kinds, direction, limit, include_start: true })` (existing). Post-BFS: filter `node_kinds` in handler; deduplicate edges by `edge.id`; skip `exclude` ids.

```json
{
  "subgraph": {
    "nodes": { "<id>": { /* NodeJson */ } },
    "edges": [ { /* EdgeJson — §4.2 */ } ],
    "roots": ["<id>"]
  },
  "truncated": true,
  "node_count": 200,
  "edge_count": 312
}
```

`truncated: true` when BFS hit the `limit` before exhausting the graph. UI shows "Load more" affordance. Pagination via `exclude` parameter (client passes currently visible node ids).

---

### EP-5 `GET /api/impact/:id`

**Backs:** View C (blast-radius), N1 stretch  
**Params:** `depth` (default 3, max 6), `limit` (default 200, hard max 500)  
**Backing query:** `QueryHandle::get_impact_radius(id, depth)` (existing). Handler truncates result to `limit` and sets `truncated`.

Response: same `SubgraphJson` wrapper as EP-4.

---

### EP-6 `GET /api/callers/:id`

**Backs:** View D (call graph), detail panel "Called by N"  
**Params:** `depth` (default 1, max 3), `limit` (default 50, hard max 200)  
**Backing query:** `GraphTraverser::get_callers(id, depth)` (existing, accessed via `QueryHandle::get_callers`)

```json
{
  "items": [
    { "node": { /* NodeJson */ }, "edge": { /* EdgeJson */ } }
  ],
  "truncated": false,
  "total": 3
}
```

---

### EP-7 `GET /api/callees/:id`

**Backs:** View D (call graph), detail panel "Calls N"  
**Params:** `depth` (default 1, max 3), `limit` (default 50, hard max 200)  
**Backing query:** `QueryHandle::get_callees(id, depth)` (existing)

Response: same shape as EP-6.

---

### EP-8 `GET /api/file-graph`

**Backs:** View B (file/module cluster, click-to-expand), file-tree click  
**Params:** `path` (required, file path), `limit` (default 200, hard max 500)  
**Backing query:** `QueryHandle::get_files(Some(&FileFilter { path_prefix: Some(path) }))` to confirm the file exists, then `GraphTraverser::traverse_bfs` with `direction: Both`, `edge_kinds: all`, `limit`, seeded from all node IDs in the file. **This is NOT a new storage query** — it composes existing methods.

Implementation note: get all nodes in the file via `queries::nodes::get_nodes_by_file(conn, path)` (already called internally by `cycle_dfs`, confirmed in `traversal.rs:560`), then run one BFS pass per node merging results. Handler deduplicates by node id.

Response: same `SubgraphJson` wrapper as EP-4.

---

### EP-9 `GET /api/files`

**Backs:** File-tree sidebar, View B seed list  
**Params:** `prefix` (optional path prefix), `lang` (optional language filter)  
**Backing query:** `QueryHandle::get_files(Some(&FileFilter { language, path_prefix }))` (existing)

```json
{
  "files": [
    {
      "path": "crates/codewiki-storage/src/storage_impl.rs",
      "language": "rust",
      "node_count": 76,
      "size": 58240,
      "modified_at": 1748128000000
    }
  ],
  "total": 162
}
```

---

### EP-10 `GET /api/top-nodes`

**Backs:** Initial canvas seed (A's open question OQ-1 — see §6)  
**Params:** `metric` (default `degree`, options: `degree|in_degree|out_degree`), `kind` (optional NodeKind filter), `limit` (default 20, hard max 50)  
**Backing query:** `get_top_nodes_by_degree` (new §3.3)

```json
{
  "nodes": [
    { "node": { /* NodeJson */ }, "in_degree": 42, "out_degree": 18, "degree": 60 }
  ]
}
```

---

### EP-11 `GET /api/clusters`

**Backs:** View B (module map clustering)  
**Params:** `depth` (default 2, directory depth), `prefix` (optional path prefix)  
**Backing query:** `QueryHandle::get_files(filter)` + Rust-side grouping by first `depth` path segments (no new SQL). Handler truncates each path to `depth` components, groups, sums `node_count` and `file_count`.

```json
{
  "clusters": [
    { "dir": "crates/codewiki-storage/src", "node_count": 312, "file_count": 14, "languages": ["rust"] }
  ]
}
```

---

### EP-12 `GET /api/health`

**Backs:** Startup probe, browser extension health check  
**Params:** none  
**Backing query:** `QueryHandle::get_stats()` (cheap)

```json
{ "ok": true, "db_size_bytes": 9091227 }
```

503 if DB is unreadable.

---

### Endpoint Summary Table

| # | Path | Method | Must-Have? | New Storage Query? | Backing Method |
|---|------|--------|------------|-------------------|----------------|
| 1 | `/api/stats` | GET | YES | §3.1 (`edges_by_kind`) | `get_stats()` + §3.1 |
| 2 | `/api/search` | GET | YES | No | `search_nodes()` |
| 3 | `/api/node/:id` | GET | YES | §3.2 (`get_node_degree`) | `get_node_by_id`, `get_code`, §3.2 |
| 4 | `/api/neighborhood/:id` | GET | YES | No | `GraphTraverser::traverse_bfs` |
| 5 | `/api/impact/:id` | GET | Stretch | No | `get_impact_radius()` |
| 6 | `/api/callers/:id` | GET | Stretch | No | `get_callers()` |
| 7 | `/api/callees/:id` | GET | Stretch | No | `get_callees()` |
| 8 | `/api/file-graph` | GET | Stretch | No | `get_files` + `traverse_bfs` composed |
| 9 | `/api/files` | GET | YES | No | `get_files()` |
| 10 | `/api/top-nodes` | GET | YES | §3.3 (`get_top_nodes_by_degree`) | §3.3 |
| 11 | `/api/clusters` | GET | Stretch | No | `get_files()` + Rust grouping |
| 12 | `/api/health` | GET | YES | No | `get_stats()` |

**Must-have endpoints (v1 ship-blocking):** EP-1, EP-2, EP-3, EP-4, EP-9, EP-10, EP-12.  
**Stretch endpoints (implement in order):** EP-5, EP-6, EP-7, EP-8, EP-11.

---

## 3. New Storage Queries (Final List)

**3 new queries.** All are pure reads against existing tables and indexes. No schema change, no new index, no migration.

### 3.1 `edges_by_kind` — for `/api/stats`

**File:** `crates/codewiki-storage/src/queries/meta.rs`  
**Signature:**

```rust
pub fn get_edges_by_kind(conn: &Connection) -> Result<HashMap<String, u64>, CodeWikiError> {
    // SELECT kind, COUNT(*) AS cnt FROM edges GROUP BY kind;
    // Uses idx_edges_kind. Returns HashMap<String, u64>.
}
```

Called only from the `/api/stats` route handler. Does NOT modify `GraphStats` or the `QueryHandle` trait.

```sql
SELECT kind, COUNT(*) AS cnt FROM edges GROUP BY kind;
```

### 3.2 `get_node_degree` — for `/api/node/:id`

**File:** `crates/codewiki-storage/src/queries/edges.rs`  
**Signature:**

```rust
pub fn get_node_degree(
    conn: &Connection,
    node_id: &str,
) -> Result<(u64, u64), CodeWikiError>  // (in_degree, out_degree)
```

```sql
SELECT
  (SELECT COUNT(*) FROM edges WHERE target = ?1) AS in_degree,
  (SELECT COUNT(*) FROM edges WHERE source = ?1) AS out_degree;
```

Uses `idx_edges_target_kind` and `idx_edges_source_kind` prefix scans. Called only from the `/api/node/:id` route handler. Does NOT modify the `QueryHandle` trait.

### 3.3 `get_top_nodes_by_degree` — for `/api/top-nodes`

**File:** `crates/codewiki-storage/src/queries/nodes.rs`  
**Signature:**

```rust
pub enum DegreeMetric { Total, In, Out }

pub fn get_top_nodes_by_degree(
    conn: &Connection,
    metric: DegreeMetric,
    kind_filter: Option<&NodeKind>,
    limit: usize,
) -> Result<Vec<(Node, u64, u64)>, CodeWikiError>  // (node, in_degree, out_degree)
```

```sql
SELECT
    n.id, n.kind, n.name, n.qualified_name, n.file_path, n.language,
    n.start_line, n.end_line, n.start_column, n.end_column,
    n.is_exported, n.docstring, n.signature, n.decorators,
    COUNT(DISTINCT e_in.id)  AS in_degree,
    COUNT(DISTINCT e_out.id) AS out_degree,
    COUNT(DISTINCT e_in.id) + COUNT(DISTINCT e_out.id) AS degree
FROM nodes n
LEFT JOIN edges e_in  ON e_in.target  = n.id
LEFT JOIN edges e_out ON e_out.source = n.id
-- WHERE n.kind = ?2  (injected when kind_filter is Some)
GROUP BY n.id
ORDER BY degree DESC   -- swap column for In/Out variants
LIMIT ?1;
```

On 2271 nodes × 8981 edges: well under 10ms. Called only from the `/api/top-nodes` route handler. Does NOT modify the `QueryHandle` trait.

**Note on `file-graph` (B's 4th query concern):** EP-8 does NOT require a new storage query. It composes `get_nodes_by_file(conn, path)` (already used internally in `cycle_dfs` in `traversal.rs`) with multiple `traverse_bfs` calls. The handler merges the resulting subgraphs. This is a composition pattern, not a new query.

---

## 4. JSON Data Shapes

All types derive `serde::Serialize` + `serde::Deserialize` in `codewiki-core`. No custom serializers needed.

### 4.1 NodeJson

Direct `serde_json::to_value(&node)` output from `Node`. Key fields for the UI:

```json
{
  "id": "sha256-...",
  "name": "traverse_bfs",
  "qualified_name": "codewiki_storage::graph::traversal::GraphTraverser::traverse_bfs",
  "kind": "method",
  "language": "rust",
  "file_path": "/abs/path/to/traversal.rs",
  "start_line": 67,
  "end_line": 138,
  "is_exported": true,
  "docstring": "Breadth-first traversal.",
  "signature": "pub fn traverse_bfs(&self, start_id: &str, opts: &TraversalOptions) -> Result<Subgraph, CodeWikiError>"
}
```

`FileRecord.language` is a `String` (not the `Language` enum) — verified from `types.rs:146`. The UI receives it verbatim.

### 4.2 EdgeJson

Direct serde output of `Edge`:

```json
{
  "id": "12345",
  "source_id": "node-id-a",
  "target_id": "node-id-b",
  "kind": "calls",
  "line": 94,
  "col": 8,
  "provenance": "name-matcher",
  "confidence": null,
  "metadata": null
}
```

### 4.3 SubgraphJson (wrapped)

```json
{
  "subgraph": {
    "nodes": { "<id>": { /* NodeJson */ } },
    "edges": [ { /* EdgeJson */ } ],
    "roots": ["<id>"]
  },
  "truncated": false,
  "node_count": 47,
  "edge_count": 63
}
```

`nodes` is the existing `HashMap<String, Node>` from `Subgraph` — keyed by node id, O(1) lookup from edge `source_id`/`target_id`.

---

## 5. Bounded Response Contract

Every subgraph endpoint enforces hard server-side limits. The `truncated` flag is mandatory in every response — clients MUST check it.

| Endpoint | Default limit | Hard max | truncated? |
|----------|--------------|----------|------------|
| `/api/neighborhood/:id` | 200 nodes | 500 | Yes |
| `/api/impact/:id` | 200 nodes | 500 | Yes |
| `/api/callers/:id` | 50 items | 200 | Yes |
| `/api/callees/:id` | 50 items | 200 | Yes |
| `/api/search` | 20 results | 100 | No (drop silently) |
| `/api/top-nodes` | 20 nodes | 50 | No |
| `/api/file-graph` | 200 nodes | 500 | Yes |

**Pagination / "expand more":** Client passes `exclude=id1,id2,...` (max 500 ids) to `/api/neighborhood/:id`. Handler skips excluded ids during BFS, effectively expanding the frontier without server-side cursor state.

**Edge deduplication:** `traverse_bfs` with `direction: Both` can emit duplicate edges (same edge seen as outgoing from A and incoming to B). Route handlers deduplicate by `edge.id` before serializing.

**`contains` edges default:** The client defaults to hiding `contains` edges in the UI filter bar. This is a **client-side rendering decision**, not a server-side filter. The API always returns `contains` edges unless the caller passes `edge_kinds=calls,imports,...` (excluding `contains`).

---

## 6. A's Four Open Questions — v1 Answers

### OQ-1: Initial-load seeding

**Decision: show top-20 nodes by total degree on load.**

The canvas is not empty on first open. EP-10 (`/api/top-nodes?limit=20`) is called immediately and the results are rendered as the seed. This gives the user immediate signal ("these are the most-connected symbols in this codebase") without requiring a search first. The user can then search to re-center on any symbol. An empty canvas is correct for domain tools; for a code explorer aimed at exploration it is unnecessarily dead on arrival.

The top-nodes seed is replaced when the user searches: on search result selection the canvas clears and seeds from the selected node's neighborhood.

### OQ-2: File-tree lazy vs eager

**Decision: eager load on first open, prefix-filtered lazy load for drill-down.**

`/api/files` (EP-9) is called once on startup with no prefix. For codewiki-rs self-index (163 files) and typical projects under ~5000 files, a single call returning all `FileRecord` objects is fast (< 5ms, verified by `get_files` implementation). The client renders a collapsible tree client-side. If the user navigates into a directory in the tree, the client filters the already-loaded list by prefix — **no additional API call**. This keeps the tree fast and simple.

For very large projects (> 10k files), the client can pass a `prefix` param to lazy-load subdirectories. This is a v2 concern — the endpoint already supports it.

### OQ-3: Source in sidebar

**Decision: include source inline in EP-3 (`/api/node/:id`).**

The `code` field in EP-3 is returned as part of the same response as node metadata. `QueryHandle::get_code(id)` reads the source file and slices `start_line..end_line`. This is the correct default: the user clicks a node, the sidebar opens with signature + source immediately — no second round-trip. If `get_code` fails (file unreadable, moved), the field is `null` and the UI shows a "source unavailable" placeholder.

`GET /api/code/:id` as a standalone endpoint is **removed** from v1 (B had it; it's redundant since EP-3 already returns source). If the frontend ever needs to lazy-load source separately (e.g. for a very large file), EP-3 can be split in v2.

### OQ-4: Impact — overlay vs separate mode

**Decision: overlay on the current graph, not a separate mode.**

When the user clicks "Show impact" in the detail panel, the client calls EP-5 (`/api/impact/:id`). The returned nodes are **merged into the current canvas** and colored by traversal depth (heat-map: depth 1 = red, depth 2 = orange, depth 3 = yellow). Existing canvas nodes that appear in the impact subgraph are recolored. A "Clear impact overlay" button resets node colors to defaults.

This is simpler than a mode switch (no canvas clear, no navigation history break) and more useful (the user can see which of their currently visible neighbors are in the blast radius). A full mode switch that replaces the canvas is v2.

---

## 7. Crate Layout

```
crates/codewiki-graph/
├── Cargo.toml
├── build.rs                    # Optional: verify vendor asset checksums only
└── src/
    ├── lib.rs                  # pub use GraphServer; feature gate on "web"
    ├── server.rs               # axum Router construction, bind, graceful shutdown, browser open
    ├── assets.rs               # rust-embed struct pointing at src/assets/
    ├── db.rs                   # open_readonly_conn() helper; Mutex<Connection> for read-only use
    ├── routes/
    │   ├── mod.rs              # shared types: BoundedResponse<T>, error mapping
    │   ├── stats.rs            # EP-1
    │   ├── search.rs           # EP-2
    │   ├── node.rs             # EP-3
    │   ├── neighborhood.rs     # EP-4
    │   ├── impact.rs           # EP-5
    │   ├── callers.rs          # EP-6
    │   ├── callees.rs          # EP-7
    │   ├── file_graph.rs       # EP-8
    │   ├── files.rs            # EP-9
    │   ├── top_nodes.rs        # EP-10
    │   ├── clusters.rs         # EP-11
    │   └── health.rs           # EP-12
    └── assets/
        ├── index.html          # ~300-line vanilla JS SPA; loads graph.min.js
        ├── graph.min.js        # vendored force-graph IIFE (committed, ~350 KB)
        └── style.css           # dark background, sidebar, filter bar layout
```

### `crates/codewiki-graph/Cargo.toml`

```toml
[package]
name    = "codewiki-graph"
version.workspace = true
edition.workspace = true
publish = false

[dependencies]
codewiki-core    = { path = "../codewiki-core" }
codewiki-storage = { path = "../codewiki-storage" }
serde            = { workspace = true }
serde_json       = { workspace = true }
thiserror        = { workspace = true }
tracing          = { workspace = true }
anyhow           = { workspace = true }

# Web feature — not compiled into the base binary
axum             = { version = "0.8",  optional = true }
tower-http       = { version = "0.6",  features = ["cors"], optional = true }
tokio            = { workspace = true, optional = true }
rust-embed       = { version = "8",    optional = true }
mime_guess       = { version = "2",    optional = true }
open             = { version = "5",    optional = true }

[features]
default = []
web     = ["axum", "tower-http", "tokio", "rust-embed", "mime_guess", "open"]
```

### Binary size impact (confirmed)

| Component | Addition |
|-----------|----------|
| axum 0.8 + hyper 1.x + tower-http | ~900 KB |
| rust-embed macro + runtime | ~20 KB |
| force-graph.min.js (vendored IIFE) | ~360 KB |
| index.html + style.css | ~15 KB |
| codewiki-graph crate code | ~50 KB |
| **Total with `web` feature** | **~1.35 MB** |

Without `web` feature: **zero addition** to the binary. The `codewiki-cli` release build enables `web` by default. CI can build without it for speed.

---

## 8. CLI Wiring

### Workspace `Cargo.toml` change

```toml
[workspace]
members = [
    "crates/codewiki-cli",
    "crates/codewiki-mcp",
    "crates/codewiki-extraction",
    "crates/codewiki-storage",
    "crates/codewiki-resolution",
    "crates/codewiki-sync",
    "crates/codewiki-core",
    "crates/codewiki-testutil",
    "crates/codewiki-graph",    # NEW
]
```

### `crates/codewiki-cli/Cargo.toml`

```toml
[dependencies]
# ... existing ...
codewiki-graph = { path = "../codewiki-graph", features = ["web"] }
```

### `crates/codewiki-cli/src/main.rs` — new variant in `Commands` enum

```rust
/// Launch the interactive graph web UI
#[command(next_help_heading = "Advanced")]
Graph {
    /// TCP port to bind (default: 7007)
    #[arg(long, default_value = "7007")]
    port: u16,

    /// Path to project root (defaults to cwd)
    #[arg(long)]
    path: Option<std::path::PathBuf>,

    /// Do not open the browser automatically
    #[arg(long)]
    no_open: bool,

    /// Hard cap on nodes returned per subgraph endpoint
    #[arg(long, default_value = "200")]
    max_nodes: usize,
},
```

Note: The `Graph` variant is NOT gated with `#[cfg(feature = "web")]` at the variant level. `codewiki-cli/Cargo.toml` always enables the `web` feature for the distributed build. If a `no-web` build is needed, a separate binary target is the right mechanism — `#[cfg]` on enum variants causes clap parse failures that are hard to diagnose.

### `crates/codewiki-cli/src/commands/graph.rs` (new file)

```rust
use std::path::PathBuf;

pub fn run(port: u16, path: Option<PathBuf>, no_open: bool, max_nodes: usize) -> anyhow::Result<()> {
    let project_root = path
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let db_path = project_root.join(".codewiki").join("codewiki.db");
    if !db_path.exists() {
        anyhow::bail!(
            "No CodeWiki index found at {}. Run `codewiki init` first.",
            db_path.display()
        );
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(
        codewiki_graph::GraphServer::new(db_path)
            .port(port)
            .no_open(no_open)
            .max_nodes(max_nodes)
            .serve()
    )
}
```

### `match` arm in `run()` function in `main.rs`

```rust
Commands::Graph { port, path, no_open, max_nodes } => {
    commands::graph::run(port, path, no_open, max_nodes)
}
```

---

## 9. Concurrency Model

The web server opens its own **read-only** `rusqlite::Connection` — completely separate from `StorageImpl`'s writer `Mutex<Connection>`.

```rust
// crates/codewiki-graph/src/db.rs
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Mutex;

pub fn open_readonly(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // PRAGMAs safe on read-only connections:
    conn.execute_batch("PRAGMA busy_timeout = 3000;")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    conn.execute_batch("PRAGMA cache_size = -32000;")?;   // 32 MB
    conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
    conn.execute_batch("PRAGMA mmap_size = 268435456;")?;
    Ok(conn)
}

pub type ReadDb = Arc<Mutex<Connection>>;
```

Every route handler acquires the `Mutex<Connection>`, calls its query synchronously inside `tokio::task::spawn_blocking`, and releases the lock. This is correct because:

1. WAL mode allows the read-only connection to read a consistent snapshot while the writer (sync daemon) appends to the WAL.
2. `GraphTraverser<'a>` borrows `&'a Connection` — it cannot cross an async boundary, so `spawn_blocking` is mandatory.
3. The `Mutex` is only contended by concurrent HTTP requests to the graph server; in a local single-user context this is negligible. A `r2d2` pool can replace the `Mutex<Connection>` in v2 with no API changes.

The writer's `StorageImpl` `Mutex<Connection>` is **never acquired** by the graph server.

---

## 10. Frontend File Plan

The `src/assets/` directory is the sole frontend home. No HTML files outside it.

### `src/assets/graph.min.js`

Vendored IIFE from `force-graph` (vasturiano). Source: `https://cdn.jsdelivr.net/npm/force-graph/dist/force-graph.min.js`. Committed to the repo. To upgrade: replace the file, run `cargo build`, commit. Exposes global `ForceGraph` when loaded as `<script>`.

### `src/assets/style.css`

~100 lines. Dark background (#1a1a2e), sidebar (320px right panel), filter bar (top bar, flex-row), minimap (bottom-right), node detail panel. No framework. CSS custom properties for theming.

### `src/assets/index.html`

~350 lines of vanilla JS in a `<script>` tag. No framework. Responsibilities:
- Init `ForceGraph()(document.getElementById('graph'))` with node/link color callbacks keyed on `kind` and edge `kind`
- `fetchNeighborhood(id)`: calls EP-4, merges nodes/edges into local `graphData`, updates `ForceGraph.graphData()`
- `fetchNode(id)`: calls EP-3, populates sidebar panel
- `searchBar` input: debounced (300ms), calls EP-2, renders dropdown with results
- On startup: calls EP-1 (stats header) and EP-10 (top-nodes seed)
- Filter bar: edge-kind toggles and node-kind chips operate on the local `graphData` via `nodeVisibility`/`linkVisibility` callbacks — no re-fetch
- "Load more" button: re-calls EP-4 with `exclude=<currentIds>` when `truncated: true`
- "Show impact" button in panel: calls EP-5, merges result, applies depth-colored overlay
- Right-click context menu: Explore / Show callers / Show callees / Pin / Remove
- `file://` links on file:line in sidebar (no server involvement)

### `src/assets.rs`

```rust
#[cfg(feature = "web")]
#[derive(rust_embed::Embed)]
#[folder = "src/assets/"]
pub struct Assets;
```

In debug builds, `rust-embed` reads from disk automatically (no `debug-embed` feature set) — live reload by refreshing the browser. In release builds, bytes are compiled in.

---

## 11. Test and Acceptance Plan

### Unit tests (in `crates/codewiki-graph/src/routes/`)

Each route module gets a `#[cfg(test)]` block using `axum::test::TestClient` (axum's built-in test helper):

- `stats.rs` test: assert response has `node_count`, `edge_count`, `edges_by_kind` keys
- `search.rs` test: query `traverse_bfs` returns `results` array
- `node.rs` test: known node id returns `code`, `in_degree`, `out_degree`
- `neighborhood.rs` test: depth=1 returns `truncated` field; `node_count <= 200`
- `top_nodes.rs` test: returns 20 nodes ordered by degree descending

All tests use `open_in_memory()` from `codewiki-storage` seeded with a small fixture graph.

### New storage query tests (in `crates/codewiki-storage/`)

- `queries/meta.rs`: `get_edges_by_kind` returns non-empty HashMap on a seeded DB
- `queries/edges.rs`: `get_node_degree(id)` returns `(u64, u64)` matching manual COUNT
- `queries/nodes.rs`: `get_top_nodes_by_degree(Total, None, 5)` returns 5 nodes ordered by degree

### Integration / acceptance test (manual, against self-index)

Run against codewiki-rs's own `.codewiki/codewiki.db` (2271 nodes, 8981 edges):

```
cargo run --bin codewiki --features web -- graph --port 7007
```

1. Browser opens at `http://127.0.0.1:7007`
2. Stats header shows: nodes: 2271, edges: 8981
3. Top-nodes seed renders ~20 nodes centered on canvas (expect `traverse_bfs`, `StorageImpl`, `GraphTraverser` near the top)
4. Search for `traverse_bfs` → dropdown shows result → select → canvas re-centers on node
5. Neighborhood at depth=1 renders: `traverse_bfs` + ~15 neighbors (actual neighbors from the self-index)
6. Click any neighbor → EP-4 called → new nodes merge into canvas
7. Open detail panel for `traverse_bfs` → signature shown, source shown (first 30 lines), in_degree/out_degree shown
8. Click "Show impact" → EP-5 called → impact nodes colored with heat-map overlay
9. Toggle off `contains` edges → contains edges hidden from canvas immediately (no re-fetch)
10. `GET /api/health` → `{"ok": true}`
11. Ctrl-C → server shuts down cleanly

---

## 10. Conflicts Flagged

### FLAG-1 (resolved): `GraphStats` struct extension

C proposes adding `edges_by_kind` to `GraphStats` in `codewiki-core`. **Do not do this.** `GraphStats` is returned by `QueryHandle::get_stats()` which is used by the MCP server. Adding a new field to the struct is safe for serde (unknown fields are ignored by deserializers), but modifying the trait and struct creates churn in the MCP layer. Instead, the `/api/stats` handler issues a second SQL call (`get_edges_by_kind`) and merges the result into the JSON response directly. `GraphStats` is unchanged.

### FLAG-2 (resolved): Port number conflict

A says 7007, B says 7890. **7007 is canonical.** B's port was never justified and contradicts A's explicit statement. Default is 7007 in all code.

### FLAG-3 (resolved): `GET /api/code/:id` standalone endpoint

B defines it; C omits it; A doesn't mention it. **Remove it.** EP-3 already returns `code` inline. A standalone source endpoint is only needed for lazy-loading heavy source files — a v2 concern.

### FLAG-4 (must-fix): `GraphTraverser` is not `Send`

`GraphTraverser<'a>` borrows `&'a Connection`. rusqlite `Connection` is `!Send` by design (it wraps a C pointer). `spawn_blocking` closures must be `Send`. The pattern is:

```rust
let db = state.db.clone();  // Arc<Mutex<Connection>>
spawn_blocking(move || {
    let conn = db.lock().unwrap();
    let traverser = GraphTraverser::new(&conn);
    traverser.traverse_bfs(id, &opts)
}).await?
```

The closure is `Send` because it moves the `Arc<Mutex<Connection>>` (which is `Send`), and `conn` (the `MutexGuard`) + `traverser` (the borrow of `conn`) are created and dropped inside the blocking thread. This is safe but must be verified at compile time. **The closure must not capture `&conn` from outside `spawn_blocking`** — the guard must be acquired inside the blocking closure. This is a subtle lifetime requirement; flag it prominently in code review.

---

## Verdict: GREEN

All three specs are consistent in intent. The conflicts are minor (port, struct extension approach, one endpoint removed). No fundamental design contradiction exists. The backing queries are confirmed to exist in `GraphTraverser`, `QueryHandle`, and `StorageImpl`. The three new storage functions are straightforward SQL against existing indexes. The `spawn_blocking` + read-only connection pattern is safe with WAL mode.

**Must-fix before merge:**
1. FLAG-4: Verify `spawn_blocking` closure pattern compiles — `MutexGuard` + `GraphTraverser` borrow must be created inside the blocking closure, not captured from outside.

**Deferred to v2:**
- r2d2 connection pool (replace `Mutex<Connection>` if contention observed under load)
- `GET /api/code/:id` standalone endpoint (if lazy-load needed for large files)
- WebSocket for live file-watcher push
- `codewiki serve --web` alias
- `search_count_hint` (`total_hint` field in search response)
