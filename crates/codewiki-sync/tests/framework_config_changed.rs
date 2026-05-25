//! P3 gate integration test — T-322 / REVIEW.md gate.
//!
//! Creates a tempdir, starts the file watcher, modifies a `tsconfig.json`
//! inside it, and asserts that a `FrameworkConfigChanged` broadcast is
//! received within 500 ms.

use codewiki_sync::{
    events::{framework_config_channel, FsChangeKind},
    watcher::{FileWatcher, WatcherConfig, DEFAULT_DEBOUNCE_MS},
};
use std::time::Duration;
use tempfile::TempDir;

/// Set up a temporary project root.
fn setup_project() -> TempDir {
    tempfile::tempdir().expect("failed to create tempdir")
}

/// The integration test uses a short debounce so it completes well within
/// the 500 ms deadline.
const SHORT_DEBOUNCE_MS: u64 = 100;

#[tokio::test]
async fn framework_config_changed_on_tsconfig_modify() {
    let project = setup_project();
    let root = project.path().to_path_buf();

    // Pre-create the file so the watcher is already running when we modify it.
    let tsconfig_path = root.join("tsconfig.json");
    std::fs::write(&tsconfig_path, r#"{"compilerOptions": {}}"#)
        .expect("failed to create tsconfig.json");

    // Create broadcast channel.
    let (fw_tx, mut fw_rx) = framework_config_channel(16);

    // Start the watcher with a short debounce.
    let config = WatcherConfig {
        debounce_ms: SHORT_DEBOUNCE_MS,
    };

    let _watcher = match FileWatcher::start(root.clone(), config, fw_tx) {
        Ok(w) => w,
        Err(e) => {
            // If the watch policy disabled watching (e.g. WSL2 /mnt path),
            // skip the test gracefully.
            eprintln!("skipping watcher test: {e}");
            return;
        }
    };

    // Give the watcher a moment to set up inotify watches.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Modify tsconfig.json — this should trigger a FrameworkConfigChanged.
    std::fs::write(
        &tsconfig_path,
        r#"{"compilerOptions": {"strict": true}}"#,
    )
    .expect("failed to modify tsconfig.json");

    // Wait for the broadcast with a 500 ms timeout (debounce 100ms + margin).
    let deadline = Duration::from_millis(500);
    let result = tokio::time::timeout(deadline, async {
        loop {
            match fw_rx.recv().await {
                Ok(event) => {
                    if event.path.file_name().and_then(|n| n.to_str()) == Some("tsconfig.json") {
                        return Some(event.change_kind);
                    }
                    // Ignore unrelated events (e.g. for other files in the dir).
                }
                Err(_) => return None,
            }
        }
    })
    .await;

    match result {
        Ok(Some(kind)) => {
            assert!(
                matches!(kind, FsChangeKind::Modified | FsChangeKind::Created),
                "expected Modified or Created, got {kind:?}"
            );
        }
        Ok(None) => {
            panic!("broadcast channel closed before receiving tsconfig.json event");
        }
        Err(_timeout) => {
            panic!(
                "did not receive FrameworkConfigChanged for tsconfig.json within {} ms",
                deadline.as_millis()
            );
        }
    }
}

/// Sanity test: modifying a non-framework file does NOT emit a
/// FrameworkConfigChanged event, but the function handles it gracefully.
#[tokio::test]
async fn no_framework_event_for_source_file() {
    let project = setup_project();
    let root = project.path().to_path_buf();

    // Create a TypeScript source file.
    let ts_path = root.join("app.ts");
    std::fs::write(&ts_path, b"export const x = 1;").expect("failed to create app.ts");

    let (fw_tx, mut fw_rx) = framework_config_channel(16);

    let config = WatcherConfig {
        debounce_ms: SHORT_DEBOUNCE_MS,
    };

    let _watcher = match FileWatcher::start(root.clone(), config, fw_tx) {
        Ok(w) => w,
        Err(_) => return, // watch policy disabled — skip
    };

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Modify the source file.
    std::fs::write(&ts_path, b"export const x = 2;").expect("failed to modify app.ts");

    // Wait briefly — no FrameworkConfigChanged should arrive.
    let result = tokio::time::timeout(Duration::from_millis(300), fw_rx.recv()).await;

    // Either timeout (expected) or an event for a *different* file (acceptable).
    match result {
        Err(_) => { /* timeout — correct: no framework event */ }
        Ok(Ok(event)) => {
            // Any framework event received must NOT be for app.ts.
            let name = event.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            assert_ne!(
                name, "app.ts",
                "should not emit FrameworkConfigChanged for app.ts"
            );
        }
        Ok(Err(_)) => { /* channel closed */ }
    }
}
