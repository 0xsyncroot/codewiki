//! T-323 — FileWatcher: notify-based watcher with debounce.
//!
//! Wraps `notify_debouncer_full::Debouncer` to provide:
//! - Recursive watch on the project root.
//! - Event filtering: skips `.codewiki/` and non-source files.
//! - Path normalisation.
//! - Configurable debounce window (default 2000 ms).
//! - An MPSC channel that receives `ChangedFiles` batches.
//! - A `tokio::sync::broadcast` channel for `FrameworkConfigChanged` events.
//! - An `AtomicBool` SYNCING flag to prevent concurrent syncs.

use crate::events::{is_framework_config, FsChangeKind, FrameworkConfigChanged};
use crate::watch_policy::watch_disabled_reason;
use codewiki_extraction::is_source_file;
use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

pub const DEFAULT_DEBOUNCE_MS: u64 = 2000;

/// A batch of file system changes, ready for the sync loop.
#[derive(Default)]
pub struct WatcherChanges {
    pub added: HashSet<PathBuf>,
    pub modified: HashSet<PathBuf>,
    pub removed: HashSet<PathBuf>,
}

/// Configuration for the file watcher.
pub struct WatcherConfig {
    pub debounce_ms: u64,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_ms: DEFAULT_DEBOUNCE_MS,
        }
    }
}

/// The active file watcher handle.
///
/// Drop to stop watching.
pub struct FileWatcher {
    _debouncer: notify_debouncer_full::Debouncer<notify_debouncer_full::notify::RecommendedWatcher, notify_debouncer_full::RecommendedCache>,
    pub changes_rx: mpsc::UnboundedReceiver<WatcherChanges>,
    pub framework_tx: broadcast::Sender<FrameworkConfigChanged>,
    pub syncing: Arc<AtomicBool>,
}

impl FileWatcher {
    /// Start watching `root` with the given configuration.
    ///
    /// Returns `Err` if the watch policy disables watching for this path,
    /// or if the underlying notify watcher fails to initialise.
    pub fn start(
        root: PathBuf,
        config: WatcherConfig,
        framework_tx: broadcast::Sender<FrameworkConfigChanged>,
    ) -> Result<Self, String> {
        // Check watch policy.
        if let Some(reason) = watch_disabled_reason(&root) {
            return Err(format!("watching disabled: {reason}"));
        }

        let (changes_tx, changes_rx) = mpsc::unbounded_channel::<WatcherChanges>();
        let syncing = Arc::new(AtomicBool::new(false));
        let fw_tx = framework_tx.clone();
        let debounce = Duration::from_millis(config.debounce_ms);

        let mut debouncer = new_debouncer(debounce, None, move |result: DebounceEventResult| {
            match result {
                Ok(events) => {
                    let mut changes = WatcherChanges::default();
                    let mut fw_events: Vec<(PathBuf, FsChangeKind)> = Vec::new();

                    for event in events {
                        let kind = classify_event(&event.kind);
                        for path in &event.paths {
                            // Normalise.
                            let path = normalize_path(path);

                            // Skip .codewiki/ changes.
                            if is_in_codewiki_dir(&path) {
                                continue;
                            }

                            // Check for framework config changes.
                            if is_framework_config(&path) {
                                if let Some(fk) = event_class_to_fs_change(&kind) {
                                    fw_events.push((path.clone(), fk));
                                }
                            }

                            // Only track source files.
                            if !is_source_file(&path) {
                                continue;
                            }

                            match kind {
                                Some(EventClass::Created) => { changes.added.insert(path); }
                                Some(EventClass::Modified) => { changes.modified.insert(path); }
                                Some(EventClass::Removed) => { changes.removed.insert(path); }
                                None => {}
                            }
                        }
                    }

                    // Emit framework events.
                    for (path, change_kind) in fw_events {
                        let _ = fw_tx.send(FrameworkConfigChanged { path, change_kind });
                    }

                    // Emit changes (even if empty, callers can filter).
                    let has_changes = !changes.added.is_empty()
                        || !changes.modified.is_empty()
                        || !changes.removed.is_empty();
                    if has_changes {
                        let _ = changes_tx.send(changes);
                    }
                }
                Err(errors) => {
                    for e in errors {
                        tracing::warn!(err = ?e, "file watcher error");
                    }
                }
            }
        })
        .map_err(|e| format!("failed to create debouncer: {e}"))?;

        debouncer
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| format!("failed to watch {}: {e}", root.display()))?;

        tracing::debug!(
            root = %root.display(),
            debounce_ms = config.debounce_ms,
            "file watcher started"
        );

        Ok(Self {
            _debouncer: debouncer,
            changes_rx,
            framework_tx,
            syncing,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum EventClass {
    Created,
    Modified,
    Removed,
}

fn event_class_to_fs_change(kind: &Option<EventClass>) -> Option<FsChangeKind> {
    match kind {
        Some(EventClass::Created) => Some(FsChangeKind::Created),
        Some(EventClass::Modified) => Some(FsChangeKind::Modified),
        Some(EventClass::Removed) => Some(FsChangeKind::Deleted),
        None => None,
    }
}

fn classify_event(kind: &notify_debouncer_full::notify::EventKind) -> Option<EventClass> {
    use notify_debouncer_full::notify::EventKind::*;
    match kind {
        Create(_) => Some(EventClass::Created),
        Modify(_) => Some(EventClass::Modified),
        Remove(_) => Some(EventClass::Removed),
        _ => None,
    }
}

fn is_in_codewiki_dir(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == ".codewiki")
}

fn normalize_path(path: &Path) -> PathBuf {
    // On Windows, convert backslashes; on Unix, path is already canonical.
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(path.to_string_lossy().replace('\\', "/"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codewiki_dir_detection() {
        assert!(is_in_codewiki_dir(&PathBuf::from("/project/.codewiki/db")));
        assert!(!is_in_codewiki_dir(&PathBuf::from("/project/src/main.ts")));
    }

    #[test]
    fn path_normalization_unix() {
        let p = PathBuf::from("/project/src/main.ts");
        assert_eq!(normalize_path(&p), p);
    }
}
