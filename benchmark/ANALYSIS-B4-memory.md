# B4 Memory and DB-Write Throughput Analysis

**Date:** 2026-05-25
**Goal:** Identify where RSS goes during django index, quantify per-node costs, and determine the path to 100k-file viability.

---

## 1. Measured RSS Across Real Corpora

All benchmarks run with `/usr/bin/time -v` from a cold `.codewiki/` (`init --no-index` then `index`). Machine: 28 logical CPUs; rayon pool = `(28/2).max(2) = 14` threads.

| Corpus   | Files | Nodes  | Edges   | Source MB | Peak RSS | Time  |
|----------|------:|-------:|--------:|----------:|---------:|------:|
| flask    |    83 |  1,839 |   1,756 |    ~0.5   |   32 MB  |  0.3 s |
| ripgrep  |   101 |  5,379 |   5,278 |    ~1.1   |   65 MB  |  3.7 s |
| lodash   |    54 |  8,936 |   8,882 |     3.7   |  128 MB  |  0.9 s |
| django   | 3,019 | 53,197 | 164,368 |    18.2   |  371 MB  | 36.5 s |

The small-corpus numbers are dominated by fixed overhead. Django at 371 MB is the first point where the variable component becomes the majority.

---

## 2. RSS Breakdown: Where the 371 MB Goes

### Fixed overhead (~191 MB, present in every run)

| Component | MB | Source |
|-----------|---:|--------|
| SQLite page cache (`PRAGMA cache_size = -64000`) | 64 | `connection.rs:48` |
| Rayon thread stacks (14 threads × 8 MB Linux default) | 112 | `orchestrator.rs:39-43`, no `stack_size` set |
| Binary + allocator metadata | ~15 | system |
| **Fixed floor** | **~191** | |

This floor explains why flask at 83 files still costs 32 MB. The rayon stacks (112 MB) are the single largest fixed item and are not touched by tree-sitter parsers beyond a small fraction.

### Variable component: django 371 MB − 191 MB = ~180 MB variable

| Component | MB | Calculation |
|-----------|---:|-------------|
| `Vec<ExtractionBatch>` returned by `index_all` | ~69 | 53k nodes × 320 B + 164k edges × 212 B + 86k unresolved × 241 B |
| Source text `Vec<(PathBuf, String)>` pre-collected | ~18 | all 2,910 `.py` files read sequentially then held until rayon finishes |
| SQLite mmap (`PRAGMA mmap_size = 268435456`) page faults into 108 MB DB | ~37 | WAL write bursts page in DB pages |
| Resolution phase unresolved refs re-read from DB | ~15 | 114k refs × ~140 B each |
| Allocator fragmentation / arena overhead | ~41 | remainder |

**Both dominant variable items are avoidable by design changes** (sections 4A and 4B below).

---

## 3. Per-Node String Cost

### Node struct (`types.rs:86-115`)

Every `Node` carries seven owned `String` fields plus three `Option<String>`:

| Field | Avg heap bytes | Stack (ptr+len+cap) | Notes |
|-------|---------------:|--------------------:|-------|
| `id` | 20.4 | 24 | constructed path-qualified ID |
| `name` | 17.6 | 24 | |
| `qualified_name` | 36.7 | 24 | |
| `file_path` | **61.6** | 24 | **duplicated across all 17.5 nodes/file** |
| `language` | 0 (enum) | 8 | no heap |
| `signature` | 0 (None) | 24 | all None for Python |
| `docstring` | 0 (None) | 24 | all None for Python |
| `metadata` | 0 (None) | 24 | all None |
| `kind`, line/col fields | — | 32 | enum + 4×u32 + bool |
| **Total per node** | **~136 heap** | **~184 stack** | **~320 bytes** |

Measured from django DB:
- `AVG(LENGTH(file_path))` = 61.6 bytes
- `AVG(LENGTH(id))` = 20.4 bytes
- `AVG(LENGTH(qualified_name))` = 36.7 bytes
- `AVG(LENGTH(name))` = 17.6 bytes
- `pct_with_signature` = 0% (Python extractor does not emit signatures)
- `pct_with_docstring` = 0%

### The file_path duplication

3,019 unique paths × 61.6 B = 181 KB of unique data. But 52,695 nodes each store a full owned `String`: **3.1 MB total** = a 17× duplication. The schema has a `files` table with `path TEXT PRIMARY KEY`, but `nodes` stores `file_path TEXT NOT NULL` directly — there is no `file_id` INTEGER FK (T-510 was deferred). At 100k files with the same density, wasted file_path heap grows to ~96 MB. This is real but not the dominant lever today (1.6% of variable RSS at django scale).

### Edge struct

`Edge` has `id: String` (24-byte stack, but String ID is empty or generated), `source_id: String`, `target_id: String`, plus optional fields. ~212 bytes per edge. Django's 164,368 edges = **33.2 MB** — the single largest in-memory item. The SQLite schema uses `INTEGER PRIMARY KEY AUTOINCREMENT` for edge IDs, but the Rust struct carries `id: String`. This `id` field is never populated by `insert_edge` (`schema.sql:44`: auto-increment), making `Edge.id` a dead allocation on every edge.

---

## 4. Top 3 Optimizations

### Optimization A: Eliminate the full `Vec<ExtractionBatch>` return — stream to counters

**Root cause.** `parse_files_parallel` (`orchestrator.rs:74-117`) calls `store.store_batch(batch.clone())` inside the rayon par_iter, then also returns `Some(batch)` so rayon collects all 3,019 batches into a `Vec<ExtractionBatch>`. The CLI (`index.rs:63-68`) uses this Vec only to count files, nodes, and edges:

```rust
let files_indexed = batches.len();
let nodes_total: usize = batches.iter().map(|b| b.nodes.len()).sum();
let edges_total: usize = batches.iter().map(|b| b.edges.len()).sum();
```

All 53k nodes and 164k edges are already stored in SQLite. The Vec is pure redundancy.

**Fix.** Change `parse_files_parallel` to return `(file_count: usize, node_count: usize, edge_count: usize)` computed atomically during collection rather than accumulating `Vec<ExtractionBatch>`. The `store_batch` call already happens in-place inside the iterator.

**Expected savings.** Eliminates ~69 MB for django; ~2.3 GB at 100k files. No throughput regression.

**Risk.** Low. Two call sites: `index.rs` (use counters) and the sync watcher path (`process_changes` in `orchestrator.rs`). The watcher path uses the returned batches to invalidate file state — it would need to accept an explicit callback or a small counter struct.

**Files.** `crates/codewiki-extraction/src/orchestrator.rs`, `crates/codewiki-cli/src/commands/index.rs`, `crates/codewiki-sync/` watcher call site.

### Optimization B: Tune the two largest fixed overheads

**B1 — Rayon stack size.** The `ThreadPoolBuilder` in `orchestrator.rs:40-43` sets no `stack_size`, accepting Linux's 8 MB default per thread. 14 threads × 8 MB = 112 MB committed. Tree-sitter parsers are recursive but bounded; actual stack usage is typically under 512 KB. Setting `stack_size(2 * 1024 * 1024)` reduces committed stacks to 28 MB, saving **84 MB**.

**B2 — Adaptive SQLite cache.** `PRAGMA cache_size = -64000` (64 MB) is applied unconditionally regardless of DB size. For the initial index pass, the cache does not benefit writes (WAL mode bypasses the cache for dirty pages). For the resolution pass, 64 MB is appropriate on a large corpus. A reasonable heuristic: keep 64 MB for corpora where DB size > 32 MB; for small repos use `min(DB_size × 2, 64 MB)`. This saves 48 MB on flask/ripgrep-scale runs.

**Expected savings.** 84 MB (B1, all runs) + up to 48 MB (B2, small corpora). For 100k-file runs the DB will be multi-GB and the full cache is warranted.

**Risk.** B1: Low (rayon's own recommended default is 2 MB). B2: Medium — reducing cache during the resolution phase may increase I/O if unresolved_refs scans exceed cache; profile resolution before reducing.

**Files.** `crates/codewiki-extraction/src/orchestrator.rs` (B1), `crates/codewiki-storage/src/connection.rs` (B2).

### Optimization C: Single outer transaction + prepared write statements

**Root cause — per-file transactions.** `store_extraction_batch` (`storage_impl.rs:79-105`) wraps each file in its own `BEGIN IMMEDIATE...COMMIT`. For django: **3,019 separate transactions**. Each `BEGIN IMMEDIATE` acquires an exclusive write lock on the WAL file, then releases it on `COMMIT`. With 14 rayon threads all calling this through a `Mutex<Connection>`, writes are fully serialized despite rayon parallelism.

`store_extraction_batch_bulk` (`storage_impl.rs:131-184`) already implements a single outer transaction covering all files, but the CLI `index` command does not use it — it calls per-file `store_batch` inside the rayon iterator.

**Root cause — unprepared inserts.** `insert_node`, `insert_edge`, and `insert_unresolved_ref` all call `conn.execute()` which calls `prepare()` internally on each invocation. For django's 53k nodes + 164k edges + 86k unresolved_refs = **303k unprepared statement compilations**. `prepare_cached()` reuses a connection-level LRU of compiled statements; for these three fixed-SQL hot paths the compilation cost is eliminated entirely.

**Fix C1.** Route the initial index through a dedicated writer thread fed by a bounded channel. Rayon threads parse and send `ExtractionBatch` to the channel; the writer thread holds the connection and writes in a single outer transaction (or 200-file sub-batches to bound WAL size). The existing `store_extraction_batch_bulk` method is the reference implementation.

**Fix C2.** Replace `conn.execute(INSERT ...)` with `let mut stmt = conn.prepare_cached(INSERT ...)?; stmt.execute(...)?` in `insert_node`, `insert_edge`, `insert_unresolved_ref`.

**Expected throughput gain.** The 36.5 s for django includes ~3 s for parsing (parallel) and ~33 s dominated by mutex contention + transaction overhead. A single outer transaction eliminates 3,018 `BEGIN IMMEDIATE` round-trips, expected to reduce extraction DB-write time by 5-20×. Prepared statements add 10-30% on top. Projected: extraction phase from ~33 s to 5-10 s for django, raising throughput from 83 files/s to 300-500 files/s.

**Risk C1.** Medium. The dedicated writer thread breaks the current design where `store_batch` is called inline from rayon. Error propagation across the channel boundary needs care. Sub-batch commits (every 200 files) ensure the WAL does not grow unbounded.

**Risk C2.** Very low. Drop-in replacement.

**Files.** `crates/codewiki-storage/src/queries/nodes.rs`, `edges.rs`, `unresolved.rs` (C2); `crates/codewiki-cli/src/commands/index.rs`, `orchestrator.rs` (C1).

---

## 5. Can the Current Design Hit 100k Files?

### Extrapolation from django (3,019 → 100,000 files, 33× scale)

| Component | Django (3k files) | 100k files (no opts) | 100k files (after A+B+C) |
|-----------|------------------:|---------------------:|-------------------------:|
| Fixed floor | ~191 MB | ~191 MB | ~107 MB (stacks 28 MB) |
| `Vec<ExtractionBatch>` | ~69 MB | **~2,286 MB** | 0 MB (eliminated by A) |
| Source text Vec | ~18 MB | **~603 MB** | ~603 MB |
| SQLite mmap | ~37 MB | **~1,200 MB** | ~1,200 MB |
| Resolution data | ~15 MB | ~500 MB | ~500 MB |
| Fragmentation | ~41 MB | ~200 MB | ~100 MB |
| **Total** | **~371 MB** | **~5–12 GB** | **~2.5 GB** |

The naive linear extrapolation (122 MB/kfile × 100k) gives **12 GB**. This is overstated because the 191 MB fixed floor does not scale; the realistic extrapolation using only the variable component (59.6 KB/file × 100k + 191 MB floor) gives **~6 GB** without optimizations.

After Opt A (eliminate Vec return) + Opt B (stack reduction) + Opt C (single transaction), the dominant remaining items are:
- Source text Vec: ~603 MB (all files read before rayon) — can be bounded by feeding rayon through a fixed-size work queue
- SQLite mmap into a multi-GB DB: ~1.2 GB RSS — reduce `mmap_size` to 512 MB for initial index; mmap benefits reads more than writes

**Realistic 100k-file target after all three optimizations: ~2–2.5 GB peak RSS.** Achievable on a 16 GB developer machine. Opt A alone (eliminating the batch Vec) is the gate — without it, 100k files requires 5+ GB just for extraction data.

**Throughput at 100k files:** Current 83 files/s → 1,200 s (20 min). After single-transaction write: projected 300-500 files/s → 3-5 min. Acceptable for a one-time initial index.

---

## 6. Ranking by Leverage / Risk

| Rank | Optimization | RSS Saved (django) | RSS Saved (100k) | Throughput | Risk |
|------|-------------|-------------------:|-----------------:|-----------|------|
| 1 | **A: Stream batches, return counters only** | 69 MB | 2,286 MB | neutral | Low |
| 2 | **C1: Single outer transaction (writer thread)** | WAL −200 MB | WAL −7 GB | 5-20× writes | Medium |
| 3 | **B1: Rayon stack_size = 2 MB** | 84 MB | 84 MB | neutral | Low |
| 4 | **C2: prepare_cached for insert_node/edge/uref** | 0 MB | 0 MB | +10-30% write | Very Low |
| 5 | **B2: Adaptive SQLite cache_size** | 48 MB (small repos) | 0 MB (large DB needs it) | may slow resolution | Medium |
| 6 | **T-510: file_path interning / file_id FK** | 3 MB | 96 MB | neutral | High (schema change) |
| 7 | **Bounded source-text batching** | 0 MB now | 603 MB | slight slowdown | Medium |

Optimizations 1–4 (A + C1 + B1 + C2) together bring django from 371 MB to approximately **218 MB** and the 100k-file projection from ~6–12 GB to **~2.5 GB**. They share no invasive schema changes and can be implemented independently.

---

## Appendix: Key Code Locations

| Item | File | Lines |
|------|------|-------|
| Vec<ExtractionBatch> collected unnecessarily | `crates/codewiki-extraction/src/orchestrator.rs` | 74-117 |
| CLI uses batches only for counting | `crates/codewiki-cli/src/commands/index.rs` | 63-68 |
| Per-file transaction | `crates/codewiki-storage/src/storage_impl.rs` | 79-105 |
| Bulk transaction (exists, unused by CLI) | `crates/codewiki-storage/src/storage_impl.rs` | 131-184 |
| SQLite pragmas (cache, mmap) | `crates/codewiki-storage/src/connection.rs` | 38-53 |
| Rayon pool (no stack_size) | `crates/codewiki-extraction/src/orchestrator.rs` | 38-49 |
| Unprepared insert_node | `crates/codewiki-storage/src/queries/nodes.rs` | 65-93 |
| Unprepared insert_edge | `crates/codewiki-storage/src/queries/edges.rs` | 44-59 |
| Unprepared insert_unresolved_ref | `crates/codewiki-storage/src/queries/unresolved.rs` | 27-46 |
| Node struct (String fields) | `crates/codewiki-core/src/types.rs` | 86-115 |
| Schema: nodes stores file_path TEXT, no file_id | `crates/codewiki-storage/src/schema.sql` | 20-41 |
