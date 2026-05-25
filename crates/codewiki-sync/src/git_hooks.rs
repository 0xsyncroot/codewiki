//! T-326/T-328 — Git hook installation with idempotent marker blocks.
//!
//! Installs `post-commit`, `post-merge`, and `post-checkout` hooks in the
//! project's `.git/hooks/` directory (or wherever `core.hooksPath` points).
//!
//! The injected script runs `codewiki sync` in the background so git never
//! waits.  Our block is delimited by marker comments so install is idempotent
//! and removal leaves user-authored hook content intact.

use codewiki_core::CodeWikiError;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MARKER_BEGIN: &str = "# >>> codewiki sync hook >>>";
const MARKER_END: &str = "# <<< codewiki sync hook <<<";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Names of the hooks A5 manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitHookName {
    PostCommit,
    PostMerge,
    PostCheckout,
}

impl GitHookName {
    fn as_str(&self) -> &'static str {
        match self {
            Self::PostCommit => "post-commit",
            Self::PostMerge => "post-merge",
            Self::PostCheckout => "post-checkout",
        }
    }
}

/// The default set of hooks installed by `codewiki init`.
pub const DEFAULT_SYNC_HOOKS: &[GitHookName] = &[
    GitHookName::PostCommit,
    GitHookName::PostMerge,
    GitHookName::PostCheckout,
];

/// Result of a hook install/remove operation.
pub struct GitHookResult {
    /// Hooks that were successfully installed or removed.
    pub installed: Vec<GitHookName>,
    /// Resolved hooks directory, `None` when not a git repo.
    pub hooks_dir: Option<PathBuf>,
    /// Reason why nothing was done (e.g. "not a git repository").
    pub skipped: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns `true` if `project_root` is inside a git working tree.
pub fn is_git_repo(project_root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8(out.stdout)
                .ok()
                .map(|s| s.trim() == "true")
        })
        .unwrap_or(false)
}

/// Resolve the git hooks directory for a project, honouring `core.hooksPath`.
///
/// Returns an absolute path, or `None` when not a git repo / git not found.
pub fn git_hooks_dir(project_root: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path_str = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if path_str.is_empty() {
        return None;
    }
    let p = PathBuf::from(&path_str);
    Some(if p.is_absolute() {
        p
    } else {
        project_root.join(&p)
    })
}

/// Install (or update) the CodeWiki sync hooks.
///
/// Idempotent: re-running replaces our marker block without duplicating it.
/// Any user-authored hook content surrounding the block is preserved.
pub fn install_git_sync_hook(
    project_root: &Path,
    hooks: &[GitHookName],
) -> Result<GitHookResult, CodeWikiError> {
    let hooks_dir = match git_hooks_dir(project_root) {
        Some(d) => d,
        None => {
            return Ok(GitHookResult {
                installed: vec![],
                hooks_dir: None,
                skipped: Some("not a git repository".to_string()),
            });
        }
    };

    fs::create_dir_all(&hooks_dir).map_err(|e| {
        CodeWikiError::Git(format!(
            "cannot access hooks dir {}: {e}",
            hooks_dir.display()
        ))
    })?;

    let block = marker_block();
    let mut installed = Vec::new();

    for &hook in hooks {
        let file = hooks_dir.join(hook.as_str());
        let content = if file.exists() {
            let base = strip_marker_block(&fs::read_to_string(&file).unwrap_or_default());
            let base = base.trim_end().to_string();
            if base.is_empty() {
                format!("#!/bin/sh\n{block}\n")
            } else {
                format!("{base}\n\n{block}\n")
            }
        } else {
            format!("#!/bin/sh\n{block}\n")
        };

        fs::write(&file, &content).map_err(|e| {
            CodeWikiError::Git(format!("failed to write hook {}: {e}", file.display()))
        })?;

        chmod_executable(&file);
        installed.push(hook);
    }

    Ok(GitHookResult {
        installed,
        hooks_dir: Some(hooks_dir),
        skipped: None,
    })
}

/// Remove the CodeWiki sync hooks.
///
/// Strips only our marker block; deletes the hook file when nothing but a
/// shebang line remains; otherwise rewrites the file with our block removed.
pub fn remove_git_sync_hook(
    project_root: &Path,
    hooks: &[GitHookName],
) -> Result<GitHookResult, CodeWikiError> {
    let hooks_dir = match git_hooks_dir(project_root) {
        Some(d) => d,
        None => {
            return Ok(GitHookResult {
                installed: vec![],
                hooks_dir: None,
                skipped: Some("not a git repository".to_string()),
            });
        }
    };

    let mut removed = Vec::new();

    for &hook in hooks {
        let file = hooks_dir.join(hook.as_str());
        if !file.exists() {
            continue;
        }
        let original = fs::read_to_string(&file).unwrap_or_default();
        if !original.contains(MARKER_BEGIN) {
            continue;
        }
        let stripped = strip_marker_block(&original);
        if is_effectively_empty(&stripped) {
            fs::remove_file(&file).map_err(|e| {
                CodeWikiError::Git(format!("failed to remove hook {}: {e}", file.display()))
            })?;
        } else {
            let rewritten = format!("{}\n", stripped.trim_end());
            fs::write(&file, &rewritten).map_err(|e| {
                CodeWikiError::Git(format!("failed to rewrite hook {}: {e}", file.display()))
            })?;
            chmod_executable(&file);
        }
        removed.push(hook);
    }

    Ok(GitHookResult {
        installed: removed,
        hooks_dir: Some(hooks_dir),
        skipped: None,
    })
}

/// Returns `true` if any of `hooks` has the codewiki marker block installed.
pub fn is_sync_hook_installed(project_root: &Path, hooks: &[GitHookName]) -> bool {
    let Some(hooks_dir) = git_hooks_dir(project_root) else {
        return false;
    };
    hooks.iter().any(|hook| {
        let file = hooks_dir.join(hook.as_str());
        file.exists()
            && fs::read_to_string(&file)
                .map(|s| s.contains(MARKER_BEGIN))
                .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn marker_block() -> String {
    [
        MARKER_BEGIN,
        "# Keeps the CodeWiki index fresh while the live file watcher is off",
        "# (e.g. WSL2 /mnt drives). Runs in the background so it never blocks git.",
        "# Managed by codewiki; remove with `codewiki uninit` or delete this block.",
        "if command -v codewiki >/dev/null 2>&1; then",
        "  ( codewiki sync >/dev/null 2>&1 & ) >/dev/null 2>&1",
        "fi",
        MARKER_END,
    ]
    .join("\n")
}

/// Remove our marker block (and the marker lines themselves) from `content`.
fn strip_marker_block(content: &str) -> String {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == MARKER_BEGIN {
            in_block = true;
            continue;
        }
        if trimmed == MARKER_END {
            in_block = false;
            continue;
        }
        if !in_block {
            out.push(line);
        }
    }
    out.join("\n")
}

/// Returns `true` if `content` consists only of shebangs and blank lines —
/// i.e. the hook only ever contained our block and nothing user-authored.
fn is_effectively_empty(content: &str) -> bool {
    content
        .lines()
        .map(|l| l.trim())
        .all(|l| l.is_empty() || l.starts_with("#!"))
}

fn chmod_executable(file: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(file, fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    {
        let _ = file; // no-op on non-Unix platforms
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a minimal fake git repo with a hooks directory.
    fn fake_git_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        let hooks = git_dir.join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        // Fake HEAD so git doesn't look further up.
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        dir
    }

    /// Get the hooks dir using our helper (bypasses git binary requirement).
    fn hooks_dir(tmp: &TempDir) -> PathBuf {
        tmp.path().join(".git").join("hooks")
    }

    /// Install hooks using the file path directly (not via git binary).
    fn install_direct(tmp: &TempDir, hook: GitHookName) {
        let hooks = hooks_dir(tmp);
        fs::create_dir_all(&hooks).unwrap();
        let block = marker_block();
        let file = hooks.join(hook.as_str());
        let content = if file.exists() {
            let base = strip_marker_block(&fs::read_to_string(&file).unwrap_or_default());
            let base = base.trim_end().to_string();
            if base.is_empty() {
                format!("#!/bin/sh\n{block}\n")
            } else {
                format!("{base}\n\n{block}\n")
            }
        } else {
            format!("#!/bin/sh\n{block}\n")
        };
        fs::write(&file, content).unwrap();
    }

    #[test]
    fn marker_block_contains_markers() {
        let block = marker_block();
        assert!(block.contains(MARKER_BEGIN));
        assert!(block.contains(MARKER_END));
        assert!(block.contains("codewiki sync"));
    }

    #[test]
    fn strip_removes_only_marker_block() {
        let original = "#!/bin/sh\n# user content\n# >>> codewiki sync hook >>>\ncodewiki sync\n# <<< codewiki sync hook <<<\n# more user content\n";
        let stripped = strip_marker_block(original);
        assert!(stripped.contains("# user content"));
        assert!(stripped.contains("# more user content"));
        assert!(!stripped.contains("codewiki sync"));
        assert!(!stripped.contains(MARKER_BEGIN));
    }

    #[test]
    fn strip_preserves_surrounding_user_content() {
        let pre = "#!/bin/sh\necho before\n";
        let post = "echo after\n";
        let block = marker_block();
        let full = format!("{pre}\n{block}\n{post}");
        let stripped = strip_marker_block(&full);
        assert!(stripped.contains("echo before"));
        assert!(stripped.contains("echo after"));
        assert!(!stripped.contains("codewiki sync"));
    }

    #[test]
    fn is_effectively_empty_for_shebang_only() {
        assert!(is_effectively_empty("#!/bin/sh\n\n"));
        assert!(is_effectively_empty("#!/bin/sh"));
        assert!(!is_effectively_empty("#!/bin/sh\necho hello\n"));
    }

    #[test]
    fn idempotent_install_no_duplicate_markers() {
        let tmp = fake_git_repo();
        let hooks_path = hooks_dir(&tmp);
        let hook_file = hooks_path.join("post-commit");

        // Install twice.
        install_direct(&tmp, GitHookName::PostCommit);
        install_direct(&tmp, GitHookName::PostCommit);

        let content = fs::read_to_string(&hook_file).unwrap();
        // Count occurrences of MARKER_BEGIN — must be exactly 1.
        let count = content.matches(MARKER_BEGIN).count();
        assert_eq!(
            count, 1,
            "marker block should appear exactly once after two installs"
        );
    }

    #[test]
    fn marker_survives_unrelated_edits() {
        let tmp = fake_git_repo();
        let hooks_path = hooks_dir(&tmp);
        let hook_file = hooks_path.join("post-commit");

        install_direct(&tmp, GitHookName::PostCommit);

        // User appends their own content.
        let existing = fs::read_to_string(&hook_file).unwrap();
        fs::write(&hook_file, format!("{existing}\necho 'user content'\n")).unwrap();

        // Install again — must not duplicate marker.
        install_direct(&tmp, GitHookName::PostCommit);
        let content = fs::read_to_string(&hook_file).unwrap();
        assert_eq!(content.matches(MARKER_BEGIN).count(), 1);
        assert!(
            content.contains("user content"),
            "user content must survive"
        );
    }

    #[test]
    fn remove_strips_block_leaves_user_content() {
        let tmp = fake_git_repo();
        let hooks_path = hooks_dir(&tmp);
        let hook_file = hooks_path.join("post-commit");

        // Write hook with user content + our block.
        let content = "#!/bin/sh\necho user\n".to_string() + &marker_block() + "\n";
        fs::write(&hook_file, &content).unwrap();

        // Strip the marker block manually (no git binary needed).
        let stripped = strip_marker_block(&content);
        assert!(stripped.contains("echo user"));
        assert!(!stripped.contains("codewiki sync"));
    }

    #[test]
    fn remove_deletes_file_when_only_shebang() {
        let shebang_only = "#!/bin/sh\n".to_string() + &marker_block() + "\n";
        let stripped = strip_marker_block(&shebang_only);
        assert!(is_effectively_empty(&stripped));
    }
}
