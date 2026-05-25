// T-430 — `init` subcommand: create .codewiki/, init DB, optionally auto-index.

use anyhow::Result;
use codewiki_storage::{open, StorageImpl};
use codewiki_sync::directory_utils::{create_codewiki_dir, is_initialized};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::commands::util::run_resolution;

// OPT-1: raised from 1_000 to 50_000.
const NODE_CACHE_CAPACITY: u64 = 50_000;

pub fn run(path: Option<PathBuf>, no_index: bool) -> Result<()> {
    let root = path.unwrap_or_else(|| std::env::current_dir().expect("cannot determine cwd"));

    // If already initialised, print info and exit.
    if is_initialized(&root) {
        println!("Already initialized at {}", root.display());
        println!("Run `codewiki index` to re-index, or `codewiki status` to see stats.");
        return Ok(());
    }

    // Create .codewiki/ dir + .gitignore.
    create_codewiki_dir(&root).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Create and initialise the database.
    let db_path = root.join(".codewiki").join("codewiki.db");
    let conn = open(&db_path)?;
    tracing::debug!(db = %db_path.display(), "database initialised");

    // Persist the project root so query tools render workspace-relative paths
    // (stored paths are forward-slash normalized on every platform).
    let root_norm = root.to_string_lossy().replace('\\', "/");
    codewiki_storage::queries::meta::set_metadata(&conn, "root_path", &root_norm)?;

    if no_index {
        // DB is created but empty — no indexing.
        println!("Initialized (--no-index: skipped indexing).");
        println!("Run `codewiki index` to build the index.");
        return Ok(());
    }

    // Auto-index.
    let storage = Arc::new(StorageImpl::new(conn, NODE_CACHE_CAPACITY));
    let store_adapter = Arc::new(crate::commands::index::StorageAdapter(Arc::clone(&storage)));
    let orchestrator = codewiki_extraction::ExtractionOrchestratorImpl::new(store_adapter);

    let t0 = Instant::now();
    let counts = orchestrator.index_all(&root);

    // Guard against silent total data loss: a repo with source files that
    // indexed nothing (or a store error) is a real failure → non-zero exit.
    // A repo with no source files is a clean, informational success.
    if !crate::commands::index::check_index_outcome(&counts)? {
        println!(
            "Initialized. No source files to index under {}.",
            root.display()
        );
        println!("Run `codewiki index` after adding source files.");
        return Ok(());
    }

    // Run WAL checkpoint + PRAGMA optimize after bulk insert (OPT-7).
    storage.run_maintenance_pub();

    // Run framework extraction + reference resolution to promote unresolved_refs
    // into real import/call/route edges (AUDIT-2/4/7 blocker fix).
    // Pass None → full framework-extract run (init always re-extracts).
    let resolved_count = run_resolution(&storage, &root, None)?;
    let elapsed = t0.elapsed();

    println!(
        "Indexed {} files, {} nodes, {} edges in {:.1}s",
        counts.files,
        counts.nodes,
        counts.edges,
        elapsed.as_secs_f64()
    );
    println!("Resolved {} references", resolved_count);

    // Auto-install git hooks so `sync` runs automatically on commit (AUDIT-5 Bug 3).
    let git_hooks_dir = root.join(".git").join("hooks");
    if git_hooks_dir.is_dir() {
        match codewiki_sync::git_hooks::install_git_sync_hook(
            &root,
            codewiki_sync::DEFAULT_SYNC_HOOKS,
        ) {
            Ok(_) => tracing::debug!("installed git sync hook"),
            Err(e) => tracing::warn!("could not install git hooks: {}", e),
        }
    }

    println!(
        "Wire your AI agent once (machine-wide) with `codewiki install --target <agent>` \
         (or `codewiki setup` to do both). Every project you `codewiki init` then works \
         automatically."
    );
    Ok(())
}
