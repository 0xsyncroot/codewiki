//! `codewiki-sync` — filesystem watcher, git hooks, and sync orchestrator.
//!
//! # Overview
//!
//! This crate implements all P3/A5 tasks (T-320…T-329):
//!
//! - `directory` — initial gitignore-aware walk and `.codewiki/` management.
//! - `watch_policy` — WSL2 detection and env-var override logic.
//! - `events` — `FrameworkConfigChanged` broadcast and `is_framework_config`.
//! - `watcher` — `notify`-backed file watcher with debounce.
//! - `coalesce` — event deduplication rules.
//! - `edit_builder` — construct `FileEdit` from cached vs. new source.
//! - `tree_cache` — moka-backed parsed-tree cache.
//! - `git_hooks` — idempotent git-hook installation.
//! - `sync_loop` — `run_sync_cycle` orchestration loop.
//! - `directory_utils` — `.codewiki/` directory creation and inspection.

pub mod coalesce;
pub mod directory;
pub mod directory_utils;
pub mod edit_builder;
pub mod events;
pub mod git_hooks;
pub mod sync_loop;
pub mod tree_cache;
pub mod watch_policy;
pub mod watcher;

// Public re-exports
pub use coalesce::coalesce_events;
pub use directory::walk_source_files;
pub use directory_utils::{create_codewiki_dir, get_codewiki_dir, is_initialized};
pub use edit_builder::try_build_file_edit;
pub use events::{framework_config_channel, is_framework_config, FsChangeKind, FrameworkConfigChanged};
pub use git_hooks::{
    install_git_sync_hook, is_git_repo, is_sync_hook_installed, remove_git_sync_hook,
    GitHookName, GitHookResult, DEFAULT_SYNC_HOOKS,
};
pub use sync_loop::{run_sync_cycle, SyncResult};
pub use tree_cache::TreeCache;
pub use watch_policy::{detect_wsl, watch_disabled_reason, WatchPolicy};
pub use watcher::FileWatcher;
