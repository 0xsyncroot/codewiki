//! T-328/T-329 — `.codewiki/` directory creation and inspection utilities.
//!
//! Used by both `codewiki init` (P4) and the sync loop.  All operations are
//! idempotent: calling them on an already-initialised project is always safe.

use codewiki_core::CodeWikiError;
use std::fs;
use std::path::{Path, PathBuf};

/// Name of the CodeWiki data directory.
pub const CODEWIKI_DIR: &str = ".codewiki";

/// The `.gitignore` content written inside `.codewiki/`.
const GITIGNORE_CONTENT: &str = "# CodeWiki data files
# These are local to each machine and should not be committed

# Database
*.db
*.db-wal
*.db-shm

# Cache
cache/

# Logs
*.log

# Hook markers
.dirty

# Sync lock
.sync.lock
";

// ---------------------------------------------------------------------------
// Core helpers
// ---------------------------------------------------------------------------

/// Return the absolute path of the `.codewiki/` directory for `project_root`.
pub fn get_codewiki_dir(project_root: &Path) -> PathBuf {
    project_root.join(CODEWIKI_DIR)
}

/// Return `true` if the project has been initialised (`.codewiki/codewiki.db`
/// exists).
pub fn is_initialized(project_root: &Path) -> bool {
    let dir = get_codewiki_dir(project_root);
    if !dir.is_dir() {
        return false;
    }
    dir.join("codewiki.db").exists()
}

/// Create the `.codewiki/` directory and its `.gitignore`.
///
/// Idempotent: calling this on a project that already has `.codewiki/` is
/// safe and will only create the `.gitignore` if it is absent.
///
/// Returns an error only if a `codewiki.db` already exists (project already
/// fully initialised).
pub fn create_codewiki_dir(project_root: &Path) -> Result<PathBuf, CodeWikiError> {
    let dir = get_codewiki_dir(project_root);
    let db = dir.join("codewiki.db");

    if db.exists() {
        return Err(CodeWikiError::Other(format!(
            "CodeWiki is already initialised in {}",
            project_root.display()
        )));
    }

    fs::create_dir_all(&dir)?;

    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, GITIGNORE_CONTENT)?;
    }

    tracing::debug!(dir = %dir.display(), "created .codewiki/ directory");
    Ok(dir)
}

/// List the contents of `.codewiki/` as paths relative to the dir itself.
///
/// Skips symlinks to prevent following links outside `.codewiki`.
pub fn list_directory_contents(project_root: &Path) -> Vec<PathBuf> {
    let dir = get_codewiki_dir(project_root);
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    walk_no_symlinks(&dir, PathBuf::new(), &mut files);
    files
}

fn walk_no_symlinks(base: &Path, prefix: PathBuf, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        // Skip symlinks.
        if entry
            .file_type()
            .map(|ft| ft.is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        let rel = if prefix.as_os_str().is_empty() {
            PathBuf::from(entry.file_name())
        } else {
            prefix.join(entry.file_name())
        };
        if entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(false)
        {
            walk_no_symlinks(&entry.path(), rel, out);
        } else {
            out.push(rel);
        }
    }
}

/// Return the total size (in bytes) of everything inside `.codewiki/`.
///
/// Skips symlinks.
pub fn get_directory_size(project_root: &Path) -> u64 {
    let dir = get_codewiki_dir(project_root);
    if !dir.is_dir() {
        return 0;
    }
    dir_size_no_symlinks(&dir)
}

fn dir_size_no_symlinks(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    let mut total = 0u64;
    for entry in entries.flatten() {
        if entry
            .file_type()
            .map(|ft| ft.is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        let p = entry.path();
        if entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(false)
        {
            total += dir_size_no_symlinks(&p);
        } else if let Ok(meta) = fs::metadata(&p) {
            total += meta.len();
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn create_idempotent() {
        let dir = tmp();
        let root = dir.path();

        // First call — creates dir + .gitignore.
        let created = create_codewiki_dir(root).unwrap();
        assert!(created.exists(), ".codewiki dir should exist");
        assert!(created.join(".gitignore").exists());

        // Second call (no db) — still ok.
        let created2 = create_codewiki_dir(root).unwrap();
        assert_eq!(created, created2);
    }

    #[test]
    fn create_fails_if_db_exists() {
        let dir = tmp();
        let root = dir.path();
        let codewiki_dir = root.join(".codewiki");
        fs::create_dir_all(&codewiki_dir).unwrap();
        fs::write(codewiki_dir.join("codewiki.db"), b"db").unwrap();

        let result = create_codewiki_dir(root);
        assert!(result.is_err(), "should fail when db already exists");
    }

    #[test]
    fn is_initialized_false_without_db() {
        let dir = tmp();
        assert!(!is_initialized(dir.path()));
        create_codewiki_dir(dir.path()).unwrap();
        assert!(!is_initialized(dir.path()), "no db yet → not initialized");
    }

    #[test]
    fn is_initialized_true_with_db() {
        let dir = tmp();
        let cg = create_codewiki_dir(dir.path()).unwrap();
        fs::write(cg.join("codewiki.db"), b"fake").unwrap();
        assert!(is_initialized(dir.path()));
    }

    #[test]
    fn list_contents_empty_before_creation() {
        let dir = tmp();
        let contents = list_directory_contents(dir.path());
        assert!(contents.is_empty());
    }

    #[test]
    fn list_contents_after_creation() {
        let dir = tmp();
        create_codewiki_dir(dir.path()).unwrap();
        let contents = list_directory_contents(dir.path());
        let names: Vec<String> = contents
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&".gitignore".to_string()));
    }

    #[test]
    fn get_directory_size_zero_before_creation() {
        let dir = tmp();
        assert_eq!(get_directory_size(dir.path()), 0);
    }

    #[test]
    fn get_directory_size_positive_after_creation() {
        let dir = tmp();
        create_codewiki_dir(dir.path()).unwrap();
        assert!(
            get_directory_size(dir.path()) > 0,
            "directory should have non-zero size after creation"
        );
    }
}
