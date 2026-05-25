//! T-320 — Initial directory walk using `ignore::WalkBuilder`.
//!
//! Respects `.gitignore` patterns, enforces a 1 MB file size cap,
//! and returns only source files (filtered via `is_source_file`).

use codewiki_extraction::{is_source_file, MAX_FILE_SIZE_BYTES};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Perform a parallel gitignore-aware walk of `root` and return paths to all
/// source files that:
///
/// 1. Pass `is_source_file` (language known to the extractor).
/// 2. Are ≤ `MAX_FILE_SIZE_BYTES` (1 MiB).
/// 3. Are not inside `.codewiki/`.
///
/// The walk uses all available CPU threads for maximum throughput.
pub fn walk_source_files(root: &Path) -> Vec<PathBuf> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let num_threads = num_threads();

    WalkBuilder::new(root)
        .standard_filters(true) // respects .gitignore, .git/info/exclude, global gitignore
        .require_git(false) // apply .gitignore rules even in non-git directories (T-320 fix)
        .threads(num_threads)
        .build_parallel()
        .run(|| {
            let tx = tx.clone();
            Box::new(move |entry| {
                use ignore::WalkState;
                match entry {
                    Ok(e) => {
                        let path = e.path().to_owned();

                        // Skip directories named `.codewiki`..
                        if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                            if e.file_name() == ".codewiki" {
                                return WalkState::Skip;
                            }
                            return WalkState::Continue;
                        }

                        // Only process regular files.
                        if !e.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                            return WalkState::Continue;
                        }

                        // Skip non-source extensions.
                        if !is_source_file(&path) {
                            return WalkState::Continue;
                        }

                        // Enforce 1 MB cap.
                        if let Ok(meta) = std::fs::metadata(&path) {
                            if meta.len() > MAX_FILE_SIZE_BYTES {
                                tracing::debug!(
                                    path = %path.display(),
                                    size = meta.len(),
                                    "skipping: file exceeds 1 MB limit"
                                );
                                return WalkState::Continue;
                            }
                        }

                        let _ = tx.send(path);
                        WalkState::Continue
                    }
                    Err(e) => {
                        tracing::warn!(err = %e, "directory walk error");
                        WalkState::Continue
                    }
                }
            })
        });

    // Drop the last sender so the receiver knows we're done.
    drop(tx);
    rx.into_iter().collect()
}

fn num_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16) // cap at 16 to avoid inotify watch-descriptor exhaustion
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_temp_project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.ts"), b"export const x = 1;").unwrap();
        fs::write(dir.path().join("helper.py"), b"def foo(): pass").unwrap();
        // Large file — should be skipped.
        let large = vec![b'x'; (MAX_FILE_SIZE_BYTES + 1) as usize];
        fs::write(dir.path().join("big.ts"), large).unwrap();
        // Non-source file — should be skipped.
        fs::write(dir.path().join("README.md"), b"# readme").unwrap();
        // .codewiki dir — should be skipped.
        fs::create_dir(dir.path().join(".codewiki")).unwrap();
        fs::write(dir.path().join(".codewiki").join("codewiki.db"), b"db").unwrap();
        dir
    }

    fn make_gitignored_project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.ts"), b"const a = 1;").unwrap();
        fs::write(dir.path().join("ignored.ts"), b"const b = 2;").unwrap();
        fs::write(dir.path().join(".gitignore"), b"ignored.ts\n").unwrap();
        dir
    }

    #[test]
    fn walk_returns_source_files() {
        let dir = make_temp_project();
        let files = walk_source_files(dir.path());
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"main.ts".to_string()), "expected main.ts");
        assert!(
            names.contains(&"helper.py".to_string()),
            "expected helper.py"
        );
    }

    #[test]
    fn walk_skips_large_files() {
        let dir = make_temp_project();
        let files = walk_source_files(dir.path());
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(
            !names.contains(&"big.ts".to_string()),
            "big.ts should be skipped"
        );
    }

    #[test]
    fn walk_skips_codewiki_dir() {
        let dir = make_temp_project();
        let files = walk_source_files(dir.path());
        for f in &files {
            let s = f.to_string_lossy();
            assert!(
                !s.contains(".codewiki"),
                "should not contain .codewiki path: {s}"
            );
        }
    }

    #[test]
    fn walk_respects_gitignore() {
        let dir = make_gitignored_project();
        let files = walk_source_files(dir.path());
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(
            names.contains(&"index.ts".to_string()),
            "index.ts should be present"
        );
        assert!(
            !names.contains(&"ignored.ts".to_string()),
            "ignored.ts should be absent"
        );
    }

    /// T-320: `.gitignore` must be honoured even when the walk root is NOT inside a git
    /// repository (i.e. there is no `.git/` ancestor).  Without `.require_git(false)` the
    /// `ignore` crate silently skips gitignore processing in non-git directories, so
    /// `vendor/dep.ts` would appear in the results even though `.gitignore` lists `vendor/`.
    #[test]
    fn walk_respects_gitignore_outside_git_repo() {
        // TempDir has no .git/ — this is the non-git-repo scenario.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("included.ts"), b"export const x = 1;").unwrap();
        fs::create_dir(dir.path().join("vendor")).unwrap();
        fs::write(
            dir.path().join("vendor").join("dep.ts"),
            b"export const y = 2;",
        )
        .unwrap();
        fs::write(dir.path().join(".gitignore"), b"vendor/\n").unwrap();

        // Confirm there is no .git/ directory — this is the bug-trigger condition.
        assert!(
            !dir.path().join(".git").exists(),
            "test must not be inside a git repo (no .git/ must exist in the tempdir)"
        );

        let files = walk_source_files(dir.path());
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(
            names.contains(&"included.ts".to_string()),
            "included.ts must be indexed"
        );
        assert!(
            !names.contains(&"dep.ts".to_string()),
            "dep.ts inside vendor/ must be excluded by .gitignore even without .git/"
        );
    }
}
