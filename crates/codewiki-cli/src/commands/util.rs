// T-430 — Shared utility: open StorageImpl from a project root path.

use anyhow::{Context, Result};
use codewiki_core::{Node, UnresolvedRef};
use codewiki_resolution::resolver::ResolverQueryHandle;
use codewiki_resolution::{
    FrameworkResolverRegistry, ReferenceResolver, ResolutionBatchRunner, ResolverCaches,
};
use codewiki_storage::queries::nodes::NodeRef;
use codewiki_storage::traits::ResolutionStore;
use codewiki_storage::{open, ExtractionStore, QueryHandle, StorageImpl};
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Default node-cache capacity for CLI commands (OPT-1).
/// Raised from 1_000 to 50_000 so the LRU is not evicting constantly at
/// django scale (~53k nodes).  The in-memory name index (OPT-2) covers the
/// hot path; this LRU still helps for query-handle calls outside resolution.
const NODE_CACHE_CAPACITY: u64 = 50_000;

/// Bridge: opens a dedicated read-only SQLite connection to implement
/// `ResolverQueryHandle` (defined in `codewiki-resolution`.).
///
/// We open a second connection rather than routing through `StorageImpl`
/// because `StorageImpl::with_conn` is private and the public `QueryHandle`
/// trait does not expose the per-name lookup methods the resolver needs.
/// SQLite WAL mode allows multiple simultaneous readers without blocking writes.
pub struct StorageQueryAdapter {
    conn: Mutex<Connection>,
}

impl StorageQueryAdapter {
    pub fn open(db_path: &Path) -> Result<Self> {
        // Open a plain read connection — no schema migration, just query access.
        // Use SQLITE_OPEN_READONLY flag via rusqlite's OpenFlags.
        use rusqlite::OpenFlags;
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| {
            format!(
                "resolver query adapter: failed to open {}",
                db_path.display()
            )
        })?;
        // Configure WAL reader pragmas.
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
            .ok();
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl ResolverQueryHandle for StorageQueryAdapter {
    fn get_node_by_id(&self, id: &str) -> Option<Node> {
        let conn = self.conn.lock().ok()?;
        codewiki_storage::queries::nodes::get_node_by_id(&conn, id)
            .ok()
            .flatten()
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        codewiki_storage::queries::nodes::get_nodes_by_name(&conn, name).unwrap_or_default()
    }

    fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        codewiki_storage::queries::nodes::get_nodes_by_lower_name(&conn, lower).unwrap_or_default()
    }

    fn get_nodes_by_qualified_name(&self, qname: &str) -> Vec<Node> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        codewiki_storage::queries::nodes::get_nodes_by_qualified_name_exact(&conn, qname)
            .unwrap_or_default()
    }

    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        codewiki_storage::queries::nodes::get_nodes_by_file(&conn, file_path).unwrap_or_default()
    }

    fn get_all_node_names(&self) -> Vec<String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        codewiki_storage::queries::nodes::get_all_node_names(&conn).unwrap_or_default()
    }

    fn get_all_file_paths(&self) -> Vec<String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        codewiki_storage::queries::files::get_all_file_paths(&conn).unwrap_or_default()
    }

    fn get_all_nodes(&self) -> Vec<Node> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        codewiki_storage::queries::nodes::get_all_nodes(&conn).unwrap_or_default()
    }

    /// OPT-12 (Wave-3): return lean NodeRef rows, skipping heavy optional
    /// string fields.  This is the fast path used by warm_caches.
    fn get_all_node_refs(&self) -> Vec<NodeRef> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        codewiki_storage::queries::nodes::get_all_node_refs(&conn).unwrap_or_default()
    }
}

/// Open (or create) the `.codewiki/codewiki.db` for the given project root
/// and return a ready `StorageImpl`.
///
/// Returns an error if the `.codewiki/codewiki.db` does not yet exist.
pub fn open_storage(root: &Path) -> Result<StorageImpl> {
    let db_path = root.join(".codewiki").join("codewiki.db");
    if !db_path.exists() {
        anyhow::bail!(
            "no index found at {}; run `codewiki init` first",
            db_path.display()
        );
    }
    let conn = open(&db_path)
        .with_context(|| format!("failed to open database at {}", db_path.display()))?;
    Ok(StorageImpl::new(conn, NODE_CACHE_CAPACITY))
}

/// Resolve cwd when an optional `--path` is not given.
pub fn resolve_root(path: Option<std::path::PathBuf>) -> std::path::PathBuf {
    path.unwrap_or_else(|| std::env::current_dir().expect("cannot determine working directory"))
}

// Config-file extensions that framework resolvers may need but that are
// NOT recognised by the AST extractor (so they never appear in known_files).
const FRAMEWORK_CONFIG_FILENAMES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "requirements.txt",
];

/// Discover extra config files under `root` that framework resolvers need but
/// that the AST extractor ignores (e.g. Cargo.toml, package.json).
/// Returns absolute path strings.
fn discover_config_files(root: &Path) -> Vec<String> {
    use ignore::WalkBuilder;
    let mut result = Vec::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(true)
        .require_git(false)
        .parents(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            name != ".codewiki" && name != "node_modules" && name != "target"
        });
    for dent in builder.build().flatten() {
        if dent.file_type().is_some_and(|ft| ft.is_file()) {
            let p = dent.into_path();
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if FRAMEWORK_CONFIG_FILENAMES.contains(&name) {
                    result.push(p.to_string_lossy().into_owned());
                }
            }
        }
    }
    result
}

/// Run the full resolution pipeline: framework `extract()` + `ResolutionBatchRunner`.
///
/// Called after every extraction pass (init / index / sync) to promote
/// unresolved_refs into real import/call/route edges.
///
/// Framework extraction is done via **Option B**: after AST extraction is
/// complete, iterate active framework resolvers for the project, call
/// `extract()` on each known source file AND each config file (Cargo.toml,
/// package.json, etc.), and persist route nodes + handler unresolved_refs.
///
/// To avoid clobbering AST-extracted nodes, each batch uses a synthetic
/// `file.path` derived from the file's path relative to `project_root` under
/// `.codewiki/routes/<resolver>/`.  This ensures two files with the same
/// filename but different directory paths get distinct virtual manifest paths.
///
/// # `changed_paths` parameter (OPT-10)
///
/// - `None` → full run; framework `extract()` always executes (used by
///   `init` / `index` / full re-index paths).
/// - `Some(paths)` → incremental run; framework `extract()` is **skipped**
///   when none of the changed paths is a config file (matched via
///   `FRAMEWORK_CONFIG_FILENAMES`) or a route file (under `.codewiki/routes/`).
///   The reference-resolution pass (ResolutionBatchRunner) still runs as
///   before — OPT-9 (Wave 2) will scope that too.
pub fn run_resolution(
    storage: &Arc<StorageImpl>,
    project_root: &Path,
    changed_paths: Option<&[PathBuf]>,
) -> Result<usize> {
    let db_path = project_root.join(".codewiki").join("codewiki.db");

    // Build the resolver caches and a minimal context for framework detect().
    // OPT-1: raised capacity from 1_000 to NODE_CACHE_CAPACITY (50_000).
    let caches = ResolverCaches::new(NODE_CACHE_CAPACITY);
    let path_aliases = codewiki_resolution::PathAliasMap::load(project_root).unwrap_or_default();
    let known_files: Vec<String> = storage
        .get_files(None)
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.path.to_string_lossy().to_string())
        // Exclude synthetic framework-manifest entries from previous runs.
        .filter(|p| !p.contains("/.codewiki/routes/"))
        .collect();

    // OPT-10: Determine whether the framework-extract pass is needed.
    //
    // On an incremental sync (`changed_paths = Some(paths)`) we skip the
    // extract pass entirely when no changed file is a config file or a route
    // manifest.  The ResolutionBatchRunner still runs so that reference
    // resolution for any new/modified source files proceeds normally.
    //
    // On a full run (`changed_paths = None`, used by init/index) we always
    // run extract.
    let routes_prefix = project_root.join(".codewiki").join("routes");
    let run_framework_extract = match changed_paths {
        None => true,
        Some(paths) => paths.iter().any(|p| {
            // Config file check: matches any FRAMEWORK_CONFIG_FILENAMES entry.
            let is_config = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| FRAMEWORK_CONFIG_FILENAMES.contains(&n))
                .unwrap_or(false);
            // Route manifest check: path under `.codewiki/routes/`.
            let is_route = p.starts_with(&routes_prefix);
            is_config || is_route
        }),
    };

    if !run_framework_extract {
        tracing::debug!("OPT-10: skipping framework extract() — no config/route files changed");
    }

    // Extend the iteration set for framework extract() with config files that
    // are not AST-indexed (e.g. Cargo.toml, package.json).  These are only
    // used for the extract() loop below; they are NOT added to known_files so
    // that resolution-context lookups remain source-file only.
    //
    // OPT-3: Move `known_files` into `framework_files` (no clone); extend in
    // place with config entries.  `known_files` must not be used after this
    // point — `ctx.known_files` now borrows `framework_files` instead.
    let config_files = discover_config_files(project_root);
    let mut framework_files = known_files; // OPT-3: move, not clone
                                           // Record how many source files are in the vec before appending config-only
                                           // entries; `ctx.known_files` borrows only this prefix so resolution context
                                           // stays source-file only.
    let source_file_count = framework_files.len();
    for cf in &config_files {
        if !framework_files.contains(cf) {
            framework_files.push(cf.clone());
        }
    }

    let ctx = codewiki_resolution::ResolutionContext {
        project_root,
        project_languages: &[],
        caches: &caches,
        path_aliases: &path_aliases,
        known_files: &framework_files[..source_file_count],
    };

    // Option B: call framework extract() on each source + config file the
    // resolver understands.  Route nodes and unresolved_refs are stored in a
    // synthetic manifest file so the delete step does NOT touch AST-extracted
    // nodes for the real source file.
    let registry = FrameworkResolverRegistry::default_registry();
    let active = registry.active_resolvers(&ctx);
    if run_framework_extract && !active.is_empty() {
        for resolver in &active {
            tracing::debug!(resolver = resolver.name(), "running framework extract()");
            for file_path_str in &framework_files {
                let file_path = std::path::Path::new(file_path_str);
                let content = match std::fs::read_to_string(file_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let result = match resolver.extract(file_path, &content, &ctx) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::debug!(
                            resolver = resolver.name(),
                            file = %file_path.display(),
                            error = %e,
                            "framework extract() error (skipped)"
                        );
                        continue;
                    }
                };
                if result.nodes.is_empty() && result.unresolved_refs.is_empty() {
                    continue;
                }
                // Synthetic manifest path: `.codewiki/routes/<resolver>/<rel_path>`
                // Use the path relative to project_root so that two different files
                // with the same filename (e.g. multiple Cargo.toml) get distinct
                // virtual paths, preventing the hash-check skip from clobbering one
                // over the other.
                let rel_path = file_path
                    .strip_prefix(project_root)
                    .unwrap_or(file_path)
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                let virtual_path = project_root
                    .join(".codewiki")
                    .join("routes")
                    .join(resolver.name())
                    .join(&rel_path);
                tracing::debug!(
                    resolver = resolver.name(),
                    file = %file_path.display(),
                    nodes = result.nodes.len(),
                    edges = result.edges.len(),
                    virtual_path = %virtual_path.display(),
                    "framework extract produced results — storing batch"
                );
                let batch = codewiki_core::ExtractionBatch {
                    file: codewiki_core::FileRecord {
                        path: virtual_path,
                        content_hash: String::new(),
                        language: String::new(),
                        size: 0,
                        modified_at: 0,
                        indexed_at: 0,
                        node_count: 0,
                        errors: Vec::new(),
                    },
                    nodes: result.nodes,
                    edges: result.edges,
                    unresolved_refs: result.unresolved_refs,
                };
                if let Err(e) = ExtractionStore::store_extraction_batch(storage.as_ref(), batch) {
                    tracing::warn!(
                        resolver = resolver.name(),
                        file = %file_path.display(),
                        error = %e,
                        "framework extract store failed"
                    );
                }
            }
        }
    }

    // Build the reference resolver and drain unresolved_refs → real edges.
    let storage_arc: Arc<dyn ResolutionStore> = Arc::clone(storage) as Arc<dyn ResolutionStore>;
    let query_adapter: Arc<dyn ResolverQueryHandle> =
        Arc::new(StorageQueryAdapter::open(&db_path)?);
    // OPT-1: raised capacity from 1_000 to NODE_CACHE_CAPACITY (50_000).
    let mut ref_resolver = ReferenceResolver::new(
        project_root.to_path_buf(),
        query_adapter,
        NODE_CACHE_CAPACITY,
    );
    ref_resolver.warm_caches();

    let runner = ResolutionBatchRunner::new(storage_arc, ref_resolver, true);

    // W5 OPT-11: full-index path uses parallel resolve + large commit batches.
    // Cap at 16 threads — beyond that, Arc<HashMap> read contention and
    // SQLite writer backpressure offer diminishing returns.
    let thread_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);

    tracing::debug!(thread_count, "W5 OPT-11: starting parallel resolution");
    let resolved_count = runner
        .run_until_empty_parallel(thread_count)
        .map_err(|e| anyhow::anyhow!("resolution failed: {}", e))?;

    tracing::info!(resolved_count, "resolution pipeline complete");
    Ok(resolved_count)
}

/// Incremental resolution for sync: scope `run_for_refs` to only the refs
/// that belong to (or depend on) the changed files, instead of draining the
/// entire `unresolved_refs` table.
///
/// ## Algorithm (OPT-9)
///
/// 1. If changed files > 10 % of total indexed files, fall back to the full
///    `run_until_empty` pass (tracking a giant dependent set would cost more
///    than just draining the queue).
/// 2. Gather refs from three sources and deduplicate by `id`:
///    - **direct refs**: `get_unresolved_by_files(changed_paths)` — refs in
///      the changed files themselves.
///    - **dependent refs**: `get_dependent_files(changed_paths)` → files with
///      edges targeting the changed files → their unresolved refs.
///    - **newly-nameable refs**: collect the names of all nodes in the changed
///      files, call `get_unresolved_by_names(names)` — previously-unresolvable
///      refs that may now resolve because the changed files added new symbols.
/// 3. Warm the in-memory name index (Wave 2a, already DONE in `warm_caches`).
/// 4. Call `run_for_refs(deduplicated_refs)`.
///
/// `init` / `index` always use the global `run_resolution` path.
pub fn run_resolution_incremental(
    storage: &Arc<StorageImpl>,
    project_root: &Path,
    changed_paths: &[PathBuf],
) -> Result<usize> {
    if changed_paths.is_empty() {
        return Ok(0);
    }

    let db_path = project_root.join(".codewiki").join("codewiki.db");

    // -----------------------------------------------------------------------
    // 10 % fallback guard: if too many files changed, full pass is cheaper.
    // -----------------------------------------------------------------------
    let total_files = storage.get_total_file_count().unwrap_or(0);

    let fallback_threshold = (total_files / 10).max(1);
    if changed_paths.len() >= fallback_threshold {
        tracing::debug!(
            changed = changed_paths.len(),
            total = total_files,
            "OPT-9: changed files ≥ 10% of total — falling back to full run_until_empty"
        );
        return run_resolution(storage, project_root, Some(changed_paths));
    }

    // -----------------------------------------------------------------------
    // Convert changed PathBufs to Strings for SQL IN-lists.
    // -----------------------------------------------------------------------
    let changed_strs: Vec<String> = changed_paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    // -----------------------------------------------------------------------
    // Query 1: direct unresolved refs in the changed files.
    // -----------------------------------------------------------------------
    let direct_refs = storage
        .get_unresolved_by_files(&changed_strs)
        .unwrap_or_default();

    // -----------------------------------------------------------------------
    // Query 2: dependent files (reverse-edge lookup) → their unresolved refs.
    // -----------------------------------------------------------------------
    let dep_files = storage
        .get_dependent_files(&changed_strs)
        .unwrap_or_default();

    let dep_refs = if dep_files.is_empty() {
        vec![]
    } else {
        storage
            .get_unresolved_by_files(&dep_files)
            .unwrap_or_default()
    };

    // -----------------------------------------------------------------------
    // Query 3: newly-nameable refs — symbol names added by the changed files.
    // -----------------------------------------------------------------------
    // Use the StorageQueryAdapter (read-only connection) to get node names.
    let new_symbol_names: Vec<String> = match StorageQueryAdapter::open(&db_path) {
        Ok(qa) => {
            let mut names: HashSet<String> = HashSet::new();
            for path_str in &changed_strs {
                for node in qa.get_nodes_in_file(path_str) {
                    names.insert(node.name.clone());
                }
            }
            names.into_iter().collect()
        }
        Err(e) => {
            tracing::warn!(error = %e, "OPT-9: could not open query adapter — skipping name query");
            vec![]
        }
    };

    let name_refs = if new_symbol_names.is_empty() {
        vec![]
    } else {
        storage
            .get_unresolved_by_names(&new_symbol_names)
            .unwrap_or_default()
    };

    // -----------------------------------------------------------------------
    // Deduplicate by id (string primary key from the DB).
    // -----------------------------------------------------------------------
    let mut seen_ids: HashSet<String> = HashSet::new();
    let all_refs: Vec<UnresolvedRef> = direct_refs
        .into_iter()
        .chain(dep_refs)
        .chain(name_refs)
        .filter(|r| seen_ids.insert(r.id.clone()))
        .collect();

    let ref_count = all_refs.len();
    tracing::info!(
        ref_count,
        changed_files = changed_strs.len(),
        dep_files = dep_files.len(),
        new_symbol_names = new_symbol_names.len(),
        "OPT-9: incremental resolution scoped ref set"
    );

    if all_refs.is_empty() {
        return Ok(0);
    }

    // -----------------------------------------------------------------------
    // Build resolver with warm caches and run for the scoped ref set.
    // -----------------------------------------------------------------------
    let storage_arc: Arc<dyn ResolutionStore> = Arc::clone(storage) as Arc<dyn ResolutionStore>;
    let qa_arc: Arc<dyn ResolverQueryHandle> = Arc::new(
        StorageQueryAdapter::open(&db_path)
            .with_context(|| "OPT-9: resolver query adapter open failed")?,
    );
    let mut ref_resolver =
        ReferenceResolver::new(project_root.to_path_buf(), qa_arc, NODE_CACHE_CAPACITY);
    ref_resolver.warm_caches();

    let runner = ResolutionBatchRunner::new(storage_arc, ref_resolver, false);
    let resolved_count = runner
        .run_for_refs(all_refs)
        .map_err(|e| anyhow::anyhow!("incremental resolution failed: {}", e))?;

    tracing::info!(
        resolved_count,
        "OPT-9: incremental resolution pipeline complete"
    );
    Ok(resolved_count)
}
