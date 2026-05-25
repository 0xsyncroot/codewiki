# B2 Analysis: Incremental Sync Is Not Incremental

**Machine:** 28 cores, 31 GB RAM, WSL2  
**Binary:** `target/release/codewiki` (post-fix build from REPORT.md baseline)  
**Corpus:** django (3,019 source files, 53,197 nodes, 86,407 unresolved refs after clean `init`)

---

## 1. Root Cause: Two Overlapping Global Re-Scans

A 1-file `sync` on django takes **3,128 ms** wall time. The `sync_cycle` itself
(`run_sync_cycle` in `crates/codewiki-sync/src/sync_loop.rs`) reports **33 ms** — it
correctly limits extraction to changed files only. The remaining **3,095 ms (99% of
total)** belongs to `run_resolution`, called unconditionally on every non-noop sync in
`crates/codewiki-cli/src/commands/sync.rs:50`.

`run_resolution` (`crates/codewiki-cli/src/commands/util.rs:155`) performs two
globally-scoped passes regardless of how many files changed:

### Pass A — Framework `extract()` over every source file (util.rs lines 197–271)

```
for resolver in &active {
    for file_path_str in &framework_files {   // 3,023 files: 3,019 source + 4 config
        let content = std::fs::read_to_string(file_path)?;
        let result = resolver.extract(file_path, &content, &ctx)?;
        // store batch → re-inserts unresolved_refs for each file
    }
}
```

`framework_files` is built from `storage.get_files(None)` (all 3,019 known source
files) plus `discover_config_files` (4 pyproject.toml/requirements.txt hits for
django). For django the Django framework resolver's `extract()` is active and runs
regex passes over every `.py` file on every sync. That is 3,023 `fs::read_to_string`
+ regex calls for a 1-file change.

The stored batches use a virtual path under `.codewiki/routes/<resolver>/`. These
paths do not exist on disk, so `walk_source_files` never sees them — causing
`run_sync_cycle` to report them as **120 removed files** on every subsequent sync.
This triggers `db.delete_file()` for each, cascading to delete their edges, then
`run_resolution` re-creates them all again. The virtual-file churn is a compounding
bug that forces a full re-extraction every single sync.

### Pass B — `ResolutionBatchRunner::run_until_empty()` over ALL unresolved refs (util.rs lines 282–286)

```rust
let runner = ResolutionBatchRunner::new(storage_arc, ref_resolver, true);
// full_index_mode = true → batch_size = 2,000
let resolved_count = runner.run_until_empty()?;
```

`get_unresolved_batch(limit, offset)` in `crates/codewiki-resolution/src/batch.rs:68`
paginates over the entire `unresolved_refs` table with no file-path filter. For django
after a 1-file touch this processes all **86,407** unresolved refs. The file being
synced (`docs/lint.py`) contributed **52** of those refs — a **1,661× overcount**
relative to the actual work needed.

---

## 2. Measured Evidence

**Test:** clean `init` of django (36 s), then `touch docs/lint.py`, then `sync`.

| Metric | Value |
|---|---|
| `run_sync_cycle` wall time (self-reported) | 33 ms |
| Total `sync` wall time | 3,128 ms |
| `run_resolution` estimated cost | 3,095 ms (99% of total) |
| Total unresolved refs in DB | 86,407 |
| Unresolved refs in changed file (`docs/lint.py`) | 52 |
| Over-resolution ratio | 1,661× |
| Framework extract iterations | 3,023 files |
| Files actually changed | 1 |
| Framework extract over-iteration ratio | 3,023× |
| Resolved count reported | 686 |
| Edges from touched file's nodes specifically | 28 |
| Collateral resolutions from unmodified files | 658 |

The 686 resolved edges (vs 52 refs for the changed file) occur because
`ResolutionBatchRunner` processes the entire table — 658 of the resolved edges came
from refs in other, unmodified files that happened to be resolvable at this pass. This
is direct evidence the runner is not scoped to the changed file.

**Tokio cross-check:** 778-file repo, 14,610 unresolved refs, 1-file sync → 1,260 ms.
Unresolved-ref ratio django/tokio = 86,407 / 14,610 = 5.9×. Sync-time ratio =
3,128 / 1,260 = 2.5×. Both are clearly O(repo size), not O(changed files).

**Key queries measured on django (SQLite, cold):**
- `get_unresolved_by_files(['docs/lint.py'])` via `idx_unresolved_file_path`: ~1 ms
- Reverse-dep file lookup (JOIN nodes→edges→nodes): 78 ms for 1 file
- Name-match sweep on largest file (1,031 nodes): 2 ms

---

## 3. Design: Truly Incremental Sync

After `run_sync_cycle` returns a non-noop `SyncResult`, the changed file set is known.
The resolution pass should operate on that set only.

### Step 1 — Fix virtual-file churn (prerequisite)

Virtual `.codewiki/routes/` files are detected as removed every sync because
`walk_source_files` excludes `.codewiki/`. Fix in `sync_loop.rs:134`: before
classifying DB records as removed, skip paths prefixed by `codewiki_dir.join("routes")`:

```rust
for path in db_map.keys() {
    if !fs_map.contains_key(path)
        && !path.starts_with(&codewiki_dir.join("routes"))  // guard virtual paths
    {
        removed_records.push(path.clone());
    }
}
```

This eliminates the 120-file delete/re-create cycle that forces a global re-extraction
on every sync.

### Step 2 — Skip framework `extract()` unless a config/route file changed

Add `fn is_route_or_config_file(&self, path: &Path) -> bool` to the `FrameworkResolver`
trait in `crates/codewiki-resolution/src/framework/mod.rs` (default: `false`). Gate
the extract loop in `util.rs`:

```rust
if changed_paths.iter().any(|p| resolver.is_route_or_config_file(p)) {
    for file_path_str in &framework_files {
        // existing extract() loop — unchanged
    }
}
// else: skip entirely for this resolver
```

For the common case (editing model/view/utility code), no extract runs. Config files
(Cargo.toml, pyproject.toml) and route-declaration files (e.g. `urls.py` for Django)
that are listed in `FRAMEWORK_CONFIG_FILENAMES` can be detected without adding any new
resolver method — the existing `FRAMEWORK_CONFIG_FILENAMES` constant in `util.rs:105`
already enumerates them.

### Step 3 — Resolve only the changed file's refs + their dependents

After re-extracting changed files, collect the incremental resolution work set:

**(a) New unresolved refs from changed files** — already in DB after
`store_extraction_batch` deletes old and inserts new refs. Query:
`get_unresolved_by_files(changed_paths)`. This function already exists at
`crates/codewiki-storage/src/queries/unresolved.rs:150` (indexed by
`idx_unresolved_file_path`) but is not exposed through `ResolutionStore`.

**(b) Unresolved refs in files that imported/referenced nodes from changed files** —
reverse-edge walk:

```sql
SELECT DISTINCT n2.file_path
FROM nodes n1
JOIN edges e ON e.target = n1.id
JOIN nodes n2 ON n2.id = e.source
WHERE n1.file_path IN (<changed_paths>)
  AND e.kind IN ('references', 'imports', 'calls')
```

Measured on django: 78 ms. Returns dependent file paths; fetch their refs with
another `get_unresolved_by_files` call.

**(c) New-symbol re-resolution sweep** — if a changed file introduces a new symbol,
other files' unresolved refs with a matching `reference_name` may now be resolvable:

```sql
SELECT * FROM unresolved_refs
WHERE reference_name IN (
  SELECT name FROM nodes WHERE file_path IN (<changed_paths>)
)
```

Uses existing `idx_unresolved_name`. Measured on django's largest file (1,031 nodes):
2 ms.

### Required Storage Changes

| Query | Exists? | Location | Action |
|---|---|---|---|
| `get_unresolved_by_files(paths)` | Yes (query fn only) | `queries/unresolved.rs:150` | Add to `ResolutionStore` trait |
| `get_dependent_files(changed_paths)` | No | — | New fn in `queries/edges.rs` using SQL above |
| `get_unresolved_by_names(names)` | No | — | New fn in `queries/unresolved.rs` using `idx_unresolved_name` |
| Virtual-file guard in SyncStore | No | `sync_loop.rs:134` | Path-prefix filter (1 line) |

`ResolutionBatchRunner` needs a new entry point:

```rust
pub fn run_for_refs(&self, refs: Vec<UnresolvedRef>) -> Result<usize, CodeWikiError>
```

Takes a pre-filtered slice instead of paginating the full table. The existing
`run_until_empty` is kept for `init`, `index`, and the large-changeset fallback (>200
changed files or >10% of total files → fall back to global).

A new `run_resolution_incremental(storage, root, changed_paths)` function in `util.rs`
wraps steps (a)–(c) and calls `run_for_refs`. The existing `run_resolution` becomes
the full-index path. `sync.rs` passes `result.added + result.modified` paths to the
incremental path.

---

## 4. Expected Impact

**Assumptions:** 1-file change, not a config/route file. ~50 unresolved refs in
changed file (measured: 52). ~10 dependent files × ~45 avg refs = ~450 additional
refs. Framework extract skipped. `warm_caches` unchanged (~15 ms).
Resolution throughput: ~28 refs/ms (measured from 86,407 refs / 3,095 ms).

| Phase | Current | Incremental Target |
|---|---|---|
| `run_sync_cycle` | 33 ms | 33 ms (unchanged) |
| Framework `extract()` loop | ~1,500 ms (3,023 reads + regex) | 0 ms (skipped) |
| `warm_caches()` | ~15 ms | ~15 ms (unchanged) |
| Reverse-dep lookup SQL | 0 ms | ~80 ms |
| `ResolutionBatchRunner` | ~1,580 ms (86,407 refs) | ~18 ms (~500 refs) |
| **Total** | **~3,128 ms** | **~146 ms** |

Conservative target: **<200 ms** for a 1-file non-config change on django-scale repos.
With `warm_caches` also scoped to changed symbols only (deferred optimization), target
is <100 ms. **47× speedup** from 3.13 s. Tokio 1.24 s → ~50 ms (25× speedup).

The scaling property changes from O(total_unresolved_refs × total_files) to
O(changed_files + their dependents) — the fix is algorithmic, not a constant-factor
optimization.

---

## 5. Edge Cases

**Changed file introduces a new symbol others were waiting for.** The new-symbol sweep
(Step 3c) covers this. Scope is bounded by `idx_unresolved_name`; typically tens to
hundreds of refs, not tens of thousands.

**Changed file removes a symbol others had resolved to.** When `store_extraction_batch`
deletes nodes for the changed file, `ON DELETE CASCADE` on `edges` removes the
resolved edges. Dependents will have broken references until their own file is next
synced or a full `sync --reindex` is run. Acceptable for incremental watch mode; full
consistency is preserved by a periodic or explicit full sync.

**Multiple files changed simultaneously** (e.g. a `git pull` touching 200 files). The
incremental path degrades gracefully: the union of refs for all changed files is
fetched. At a threshold (e.g. >10% of total source files) the code falls back to
`run_until_empty`. The existing code becomes the fallback, not dead code.

**Config/route file changed** (e.g. `urls.py`, `pyproject.toml`). Framework
`extract()` must run, but only on the changed config/route files' resolver. Not all
3,023 files need to be re-extracted — only the resolver that owns that file type needs
to re-run, and only over the files it actually cares about. This is a second-order
optimization; the conservative path runs `extract()` on all files for the affected
resolver only (not all resolvers).

---

## 6. Top 3 Changes Ordered by Leverage / Risk

### #1 — Fix virtual-file churn in `sync_loop.rs` (HIGH leverage, LOW risk)

**File:** `crates/codewiki-sync/src/sync_loop.rs:134`

One predicate added to the removed-files classification loop. Eliminates the 120-file
delete/re-create cycle on every sync, which forces `store_extraction_batch` 120 times
and repopulates unresolved refs unnecessarily. No new abstractions; no trait changes.
Estimated impact: ~300–500 ms reduction (virtual-file reconstruction cost). Safe to
ship independently.

---

### #2 — Scope `ResolutionBatchRunner` to changed-file refs (HIGH leverage, MEDIUM risk)

**Files touched:**
- `crates/codewiki-resolution/src/batch.rs` — add `run_for_refs`
- `crates/codewiki-storage/src/traits/resolution.rs` — add `get_unresolved_by_files`, `get_dependent_files` to trait
- `crates/codewiki-storage/src/storage_impl.rs` — implement new trait methods
- `crates/codewiki-storage/src/queries/unresolved.rs` — expose `get_unresolved_by_files`; add `get_unresolved_by_names`
- `crates/codewiki-storage/src/queries/edges.rs` — add `get_dependent_files`
- `crates/codewiki-cli/src/commands/util.rs` — add `run_resolution_incremental`
- `crates/codewiki-cli/src/commands/sync.rs` — call incremental path with changed paths

Estimated impact: 1,550 ms reduction (86,407 → ~500 refs). Medium risk because it
changes the primary resolution entry point and requires the `SyncResult` to carry
changed path lists back to the caller (currently it only carries counts).

---

### #3 — Skip framework `extract()` for non-config/route changes (MEDIUM leverage, LOW risk)

**Files touched:**
- `crates/codewiki-resolution/src/framework/mod.rs` — add `is_route_or_config_file` to trait (default: `false`)
- `crates/codewiki-cli/src/commands/util.rs` — gate extract loop on changed-path check

Estimated impact: ~1,500 ms reduction (3,023 file reads + regex eliminated). Low risk
because the default returns `false` (always run), preserving current behaviour until
individual resolvers opt in. Can be partially achieved immediately using the existing
`FRAMEWORK_CONFIG_FILENAMES` list as a guard — no trait change needed for the
config-file detection half.

---

## Summary

The 3.13 s django 1-file sync is caused entirely by `run_resolution` running globally
after every non-noop sync cycle. The correct incremental extraction (`run_sync_cycle`)
completes in 33 ms. The global resolution work is 1,661× the amount needed for the
changed file. Two compounding issues: (1) virtual framework files are re-detected as
removed and re-created every sync, keeping the unresolved ref queue inflated; (2)
`ResolutionBatchRunner` paginates the full `unresolved_refs` table with no file-path
filter. All storage queries needed for the incremental path are either already
implemented (`get_unresolved_by_files` at `queries/unresolved.rs:150`) or
straightforward index-backed additions. No new indexing infrastructure is required;
the fix is algorithmic scoping of existing operations.
