# CodeWiki Optimization Plan

**Reviewer:** consolidation pass over B1-resolution, B2-sync, B4-memory, SCALE, B5-context analyses.  
**Note:** Analysis files B1-resolution and B5-context were not present on disk; their findings were reconstructed from cross-references in the available analyses and verified structurally via the codewiki index and direct file reads. All code locations below were confirmed against the live index.

---

## 1. Consolidated Work Items

| ID | Title | Analyses | Mechanism | Expected Impact | Risk | Files Touched | Crate(s) |
|----|-------|----------|-----------|----------------|------|---------------|----------|
| OPT-1 | Cache capacity 1000 → 50000 + name HashMap | B1/SCALE | `ResolverCaches::new(1_000)` at `util.rs:159,279` is the LRU fed by every ref lookup. Raise to 50k and route the main name-match path through an in-memory `HashMap<String,Vec<NodeId>>` built once per `run_resolution` call rather than per-ref DB queries. | 30–50% fewer DB round-trips during resolution. For django (114k refs × 6 DB queries/ref = ~686k queries) → ~343k queries. | Low | `crates/codewiki-cli/src/commands/util.rs`, `crates/codewiki-resolution/src/caches.rs` | codewiki-resolution, codewiki-cli |
| OPT-2 | Write-through name cache in `warm_caches` | B1/SCALE | `ReferenceResolver::warm_caches()` loads all node names but the main `resolve_via_name_matcher` path does not consult the in-memory list — it calls `get_nodes_by_name` per ref. Populate `known_names: Option<HashMap<…>>` on `ReferenceResolver` during `warm_caches` and short-circuit the DB call when the map is present. | Eliminates DB hit for ~90% of name-matcher calls. Combined with OPT-1: reduces 19.4 s tokio → ~8 s; django 36 s → ~15 s (est. 2× speed-up on resolution-bound repos). | Low | `crates/codewiki-resolution/src/resolver.rs` | codewiki-resolution |
| OPT-3 | `known_files` Vec clone elimination | B1 | `util.rs:175–183` clones `known_files` (Vec of 3019 strings) into `framework_files` even when no config files exist. Change to `let mut framework_files = known_files;` (move) then extend in-place. Eliminates one full Vec allocation per `run_resolution`. | ~0.5 MB peak RSS; negligible latency but correctness improvement. | Low | `crates/codewiki-cli/src/commands/util.rs` | codewiki-cli |
| OPT-4 | Rayon `stack_size(2 MB)` | B4 | `orchestrator.rs:40-43` builds the rayon pool with no `stack_size`. Linux default is 8 MB × 14 threads = 112 MB committed. Tree-sitter parsers use ≤512 KB actual stack. Add `.stack_size(2 * 1024 * 1024)`. | −84 MB peak RSS across all runs (fixed floor reduction). | Low | `crates/codewiki-extraction/src/orchestrator.rs` | codewiki-extraction |
| OPT-5 | Stream `ExtractionBatch`: return counters not Vec | B4/SCALE | `parse_files_parallel` (orchestrator.rs:74-117) calls `store.store_batch(batch.clone())` inside rayon, then returns `Some(batch)` so all 3019 batches accumulate in a `Vec`. The CLI (`index.rs:63-68`) uses the Vec only for counting. Change return type to `(file_count, node_count, edge_count)` using atomics. The watcher path (`process_changes`) needs a small callback/counter struct instead. | −69 MB django, −2.3 GB at 100k files. The single gate for 100k viability. | Low-Med | `crates/codewiki-extraction/src/orchestrator.rs`, `crates/codewiki-cli/src/commands/index.rs`, `crates/codewiki-sync/` (watcher call site) | codewiki-extraction, codewiki-cli, codewiki-sync |
| OPT-6 | `prepare_cached` for insert_node / insert_edge / insert_unresolved_ref | B4 | All three insert functions call `conn.execute()` which calls `prepare()` each time. Confirmed at `queries/nodes.rs:65`, `queries/edges.rs:44`, `queries/unresolved.rs:27`. Replace with `conn.prepare_cached(SQL)?.execute(params)`. | Eliminates 303k statement compilations for django (53k nodes + 164k edges + 86k urefs). +10–30% write throughput. | Very Low | `crates/codewiki-storage/src/queries/nodes.rs`, `edges.rs`, `unresolved.rs` | codewiki-storage |
| OPT-7 | Single outer transaction + activate `store_extraction_batch_bulk` | B4/SCALE | `store_extraction_batch` (`storage_impl.rs:79-105`) wraps each file in its own `BEGIN IMMEDIATE`; 3019 separate exclusive-lock commits. `store_extraction_batch_bulk` (`storage_impl.rs:131-184`) already implements the single-transaction path but is never called by the CLI `index` command. Route `index` through a dedicated writer thread fed by a bounded channel from rayon; the writer calls the bulk path in 200-file sub-batches. Also: drop FTS triggers (`nodes_ai/ad/au`) during bulk insert, bulk-insert, then rebuild FTS with `INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild')`, then run `run_maintenance` (which exists at `queries/meta.rs:117` but is confirmed never called from CLI). Wire `wal_checkpoint(PASSIVE)` at init/index completion. | 5–20× write throughput reduction in extraction phase. Django extraction ~33 s → 5–10 s; 100k throughput 83 files/s → 300–500 files/s. | Med | `crates/codewiki-storage/src/storage_impl.rs`, `queries/nodes.rs`, `crates/codewiki-cli/src/commands/index.rs`, `crates/codewiki-extraction/src/orchestrator.rs` | codewiki-storage, codewiki-extraction, codewiki-cli |
| OPT-8 | Fix virtual-file churn in `sync_loop.rs` | B2 | `sync_loop.rs:134-138`: the removed-files loop classifies all DB paths not present on disk as removed. Virtual `.codewiki/routes/<resolver>/` paths do not exist on disk so they are classified as removed on every sync → `db.delete_file()` × 120, cascading deletes, then `run_resolution` re-creates them. Add one guard: `&& !path.starts_with(&codewiki_dir.join("routes"))`. | Eliminates 120-file delete/re-create cycle on every sync. Estimated −300–500 ms per sync on django. Prerequisite for OPT-9. | Low | `crates/codewiki-sync/src/sync_loop.rs` | codewiki-sync |
| OPT-9 | Scope `run_resolution` to changed-file refs (incremental sync) | B2 | After `run_sync_cycle`, the changed path set is known. Build three queries: (a) `get_unresolved_by_files(changed_paths)` — function exists at `queries/unresolved.rs:150` but not exposed through `ResolutionStore` trait; (b) `get_dependent_files(changed_paths)` — new SQL JOIN via `idx_unresolved_file_path`; (c) `get_unresolved_by_names(new_symbols)` — new fn using `idx_unresolved_name`. Add `ResolutionBatchRunner::run_for_refs(refs: Vec<UnresolvedRef>)` entry point. Add `run_resolution_incremental()` in `util.rs`. Fallback to full `run_until_empty` when >10% of files changed. | django 1-file sync: 3128 ms → ~146 ms (21×). Tokio: 1240 ms → ~50 ms (25×). Scaling property changes from O(total_refs) to O(changed_file_refs + dependents). | Med | `crates/codewiki-resolution/src/batch.rs`, `crates/codewiki-storage/src/traits/resolution.rs`, `storage_impl.rs`, `queries/unresolved.rs`, `queries/edges.rs`, `crates/codewiki-cli/src/commands/util.rs`, `sync.rs` | codewiki-resolution, codewiki-storage, codewiki-cli |
| OPT-10 | Skip framework `extract()` for non-config/route changes | B2 | Add `fn is_route_or_config_file(&self, path: &Path) -> bool` to `FrameworkResolver` trait (`framework/mod.rs`), default `false`. Gate the extract loop in `util.rs:200-270` on whether any changed path matches. Already-enumerated `FRAMEWORK_CONFIG_FILENAMES` constant can handle config detection without the trait method for a quick first pass. | Eliminates ~1500 ms / 3023 file reads + regex on a typical non-config 1-file sync. | Low | `crates/codewiki-resolution/src/framework/mod.rs`, `crates/codewiki-cli/src/commands/util.rs` | codewiki-resolution, codewiki-cli |
| OPT-11 | Parallel resolution (shard by file_path hash) | SCALE | `ResolutionBatchRunner::run_until_empty` is single-threaded. Shard `unresolved_refs` by `file_path` hash into N buckets; N read-only WAL readers resolve concurrently and send `(from_node_id, target_node_id, edge_kind)` to a single writer channel. Share the write-through name map (OPT-2) via `Arc<HashMap>`. WAL mode allows concurrent reads. | 8–12× throughput on resolution phase. Primary O(n^1.75) fix. 100k: 4.5 h → ~25–35 min combined with OPT-7. INTERACTS with OPT-2 (name map must be `Arc<HashMap>` not `&mut HashMap`). Must come after OPT-2 is `Arc`-safe. | Med | `crates/codewiki-resolution/src/batch.rs`, `resolver.rs`, `crates/codewiki-storage/src/traits/resolution.rs` | codewiki-resolution, codewiki-storage |
| OPT-12 | Context: FTS fallback for NL queries + summary line | B5 | `handle_context` (`tools/context.rs`) calls `find_relevant_context` which calls `extract_symbols_from_query`. For purely NL queries ("how does authentication work") the symbol extractor yields few/no symbols and FTS gets no terms. Add an FTS fallback: when `exact_matches` is empty and the query has ≥3 words, call `search_nodes_fts` directly on the raw query string and seed the subgraph from those results. Also: prepend a one-line summary (node count, file count) above the Entry Points section. | Context NL relevance from 3/8 → ≥6/8 on B5 check queries. | Low | `crates/codewiki-mcp/src/tools/context.rs`, `crates/codewiki-storage/src/storage_impl.rs` | codewiki-mcp, codewiki-storage |
| OPT-13 | Context: code blocks for non-entry non-container functions | B5 | `handle_context` line 144: `CONTAINER_KINDS` filter correctly excludes class/struct/etc, but only `entry_nodes` get code blocks — `other_nodes` (direct neighbors returned by BFS) get only name+location. When `include_code=true` and `other_nodes` has non-container functions, include code blocks for up to 3 of them (capped at 800 chars each). | Improves substantive code coverage in context output. | Low | `crates/codewiki-mcp/src/tools/context.rs` | codewiki-mcp |
| OPT-14 | Fix `projectPath` parameter (MCP tools) | B5 | The MCP `codewiki_context` and `codewiki_search` tools accept a `projectPath` parameter but the storage handle is opened at server startup from `current_dir`. When `projectPath` differs from the server's root the parameter is silently ignored. Add `projectPath`-aware handle selection in the server dispatch layer. | Unblocks multi-project workspaces. Currently returns wrong-project results or empty. | Low-Med | `crates/codewiki-mcp/src/server.rs`, `crates/codewiki-mcp/src/tools/context.rs`, `tools/search.rs` | codewiki-mcp |
| OPT-15 | Context: related-files section | B5 | After the Related Symbols section, add a "### Related Files" block listing the unique file paths (deduped from all nodes in the subgraph, excluding the entry-point files). Cap at 5 files. This surfaces which modules are involved without requiring follow-up `files` calls. | Reduced follow-up tool calls; qualitative UX improvement. | Low | `crates/codewiki-mcp/src/tools/context.rs` | codewiki-mcp |

---

## 2. Conflict / Interaction Map

### Same-file collisions

| File | Items | Resolution |
|------|-------|-----------|
| `crates/codewiki-cli/src/commands/util.rs` | OPT-1, OPT-2, OPT-3, OPT-9, OPT-10 | Single agent owns this file across waves 1-2. OPT-3 (one-liner) can land first; OPT-1/OPT-2 are additive to the same `run_resolution` function; OPT-9/OPT-10 add new functions alongside. No overwrite conflicts if done sequentially by the same agent. |
| `crates/codewiki-extraction/src/orchestrator.rs` | OPT-4, OPT-5, OPT-7 | OPT-4 and OPT-5 are independent (different sections). OPT-7 restructures the writer path that OPT-5 also touches — these MUST be done together or in strict order: OPT-5 first (change return type), then OPT-7 (add writer thread using the new counter-return shape). |
| `crates/codewiki-storage/src/storage_impl.rs` | OPT-7, OPT-9, OPT-11 | OPT-7 adds bulk write path; OPT-9 adds resolution trait methods; OPT-11 adds parallel resolution entry. Non-overlapping sections — safe to parallelize across Wave 2 and Wave 3 by having agents take separate trait/impl sections. |
| `crates/codewiki-resolution/src/batch.rs` | OPT-9, OPT-11 | OPT-9 adds `run_for_refs`; OPT-11 adds parallel sharding logic. OPT-9 must land first because OPT-11's parallel variant replaces `run_until_empty` and calls `run_for_refs` on each shard. |
| `crates/codewiki-mcp/src/tools/context.rs` | OPT-12, OPT-13, OPT-15 | All additive to the same function body. Assign to one agent; no conflict. |

### Semantic interactions

- **OPT-2 ↔ OPT-11 (critical):** OPT-2 must expose the name map as `Arc<HashMap<String, Vec<NodeId>>>` (not `&mut HashMap`) so OPT-11 can share it across parallel resolution workers. Implementing OPT-2 with an `Arc` from the start avoids rework.
- **OPT-5 → OPT-7 (sequencing):** OPT-5 changes `parse_files_parallel` return type. OPT-7 adds the writer-thread channel. Both touch `orchestrator.rs`; do together in Wave 2 as a single commit.
- **OPT-8 → OPT-9 (prerequisite):** Without OPT-8, virtual-file churn keeps repopulating unresolved refs and the incremental scope is defeated. OPT-8 must ship before OPT-9 is tested.
- **OPT-10 → OPT-9 (additive, independent):** OPT-10 can ship without OPT-9 and independently cuts ~1.5 s from every non-config sync. Both are gated by OPT-8.

---

## 3. Phased Rollout

### Wave 1 — Quick wins, low risk, independently shippable

Target: no structural changes; each item is a 1–50 line change; all tests stay green without any refactor.

| Item | Agent Assignment | Est. Effort |
|------|-----------------|-------------|
| OPT-4: rayon stack_size | Agent A (codewiki-extraction) | 1 line |
| OPT-3: known_files move not clone | Agent A (codewiki-extraction/cli) | 1 line |
| OPT-6: prepare_cached inserts | Agent B (codewiki-storage) | 3 files, ~10 lines each |
| OPT-8: virtual-file churn guard | Agent C (codewiki-sync) | 3 lines |
| OPT-10: skip framework extract on non-config | Agent C (codewiki-sync/resolution) | ~30 lines |

**Wave 1 gate:** All 562 tests green; `cargo clippy -- -D warnings` clean; django incremental sync measurably below 2 s (from 3.13 s). Peak RSS of any corpus measurably down by ≥80 MB.

### Wave 2 — Structural, moderate risk

Dependencies: Wave 1 merged. Items OPT-5+OPT-7 must land together.

| Item | Agent Assignment | Est. Effort |
|------|-----------------|-------------|
| OPT-1 + OPT-2: cache capacity + write-through name map (expose as Arc) | Agent A (codewiki-resolution, codewiki-cli) | ~80 lines |
| OPT-5 + OPT-7: stream batches + single transaction + FTS drop-rebuild + wire run_maintenance | Agent B (codewiki-extraction, codewiki-storage, codewiki-cli) | ~150 lines across 4 files |
| OPT-9: incremental sync scoping (requires OPT-8 from Wave 1) | Agent C (codewiki-resolution, codewiki-storage, codewiki-cli) | ~200 lines across 7 files |

**Wave 2 gate:** django cold index ≤18 s (from 36.2 s); django incremental sync ≤200 ms (from 3130 ms); tokio cold index ≤10 s (from 19.4 s); peak RSS django ≤280 MB (from 367 MB); 100k-file extrapolation ≤40 min (from 4.5 h).

### Wave 3 — Larger / additive

Dependencies: Wave 2 merged. OPT-11 requires OPT-2's Arc name map.

| Item | Agent Assignment | Est. Effort |
|------|-----------------|-------------|
| OPT-11: parallel resolution | Agent A (codewiki-resolution, codewiki-storage) | ~200 lines |
| OPT-12 + OPT-13 + OPT-15: context NL improvements + code blocks + related files | Agent B (codewiki-mcp) | ~80 lines, all in tools/context.rs |
| OPT-14: projectPath fix | Agent C (codewiki-mcp) | ~60 lines, server.rs + tool handlers |

**Wave 3 gate:** 100k-file extrapolation ≤15 min, ≤2 GB RSS; context NL relevance ≥6/8 on B5 check script; all 10 corpus repos still index cleanly.

---

## 4. Acceptance Targets (re-benchmark gates)

All targets measured on the same machine (28 cores, 31 GB RAM, WSL2) using the same `run-bench.sh` methodology.

### After Wave 1

| Metric | Baseline | Wave 1 Target |
|--------|----------|--------------|
| django incremental sync | 3130 ms | ≤1800 ms |
| tokio incremental sync | 1240 ms | ≤700 ms |
| django peak RSS | 367 MB | ≤280 MB |
| Any corpus RSS | — | ≥80 MB reduction (rayon stacks) |
| All 562 tests | green | green |
| clippy | clean | clean |

### After Wave 2

| Metric | Baseline | Wave 2 Target |
|--------|----------|--------------|
| django cold index | 36.2 s | ≤18 s |
| tokio cold index | 19.4 s | ≤10 s |
| django incremental sync | 3130 ms | ≤200 ms |
| tokio incremental sync | 1240 ms | ≤60 ms |
| django peak RSS | 367 MB | ≤250 MB |
| 100k-file extrapolation (time) | ~4.5 h | ≤40 min |
| 100k-file extrapolation (RSS) | ~4–12 GB | ≤2.5 GB |
| All 10 corpus repos index cleanly | yes | yes |

### After Wave 3

| Metric | Baseline | Wave 3 Target |
|--------|----------|--------------|
| django cold index | 36.2 s | ≤10 s |
| 100k-file extrapolation (time) | ~4.5 h | ≤15 min |
| 100k-file extrapolation (RSS) | ~4–12 GB | ≤2 GB |
| context NL relevance | 3/8 | ≥6/8 (B5 check script) |
| `projectPath` multi-project | broken | returns correct-project results |
| Search latency p50 (all corpora) | ≤7 ms | unchanged or better |
| All 562 tests | green | green |
| clippy | clean | clean |

---

## 5. Verdict

**GREEN** — the plan is internally consistent and non-conflicting.

Key sequencing notes:

1. **OPT-8 before OPT-9.** The incremental sync fix (OPT-9) is functionally correct only after the virtual-file churn is stopped by OPT-8. Both are in Wave 1/2 respectively — this ordering is respected.

2. **OPT-2 Arc-aware before OPT-11.** Implementing the write-through name map as `Arc<HashMap>` in OPT-2 (Wave 2) is required for OPT-11's parallel workers (Wave 3) to share it without a Mutex bottleneck. If OPT-2 uses `&mut HashMap`, OPT-11 will require a rework. Implement OPT-2 Arc-first.

3. **OPT-5 + OPT-7 are one commit.** They both reshape `orchestrator.rs`'s return contract and the CLI's consumption. Shipping OPT-5 alone (counters only) and OPT-7 alone (writer thread with bulk path) are each valid independently, but the same `orchestrator.rs` lines are in the blast radius of both. Assign to one agent and ship as a single PR.

4. **No schema migrations in Waves 1–2.** The `file_id` FK / file_path interning (T-510, B4 rank-6) is intentionally deferred — it is a high-risk schema migration with 3 MB benefit at django scale. It becomes relevant at 100k files (~96 MB) but should not block earlier waves.

5. **`store_extraction_batch_bulk` already exists** (`storage_impl.rs:131-184`). OPT-7 is activating existing code (wiring it into the CLI index path + adding FTS drop-rebuild + checkpoint), not new infrastructure. This substantially reduces OPT-7's risk from the "Medium" label.
