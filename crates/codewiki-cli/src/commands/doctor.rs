// D5 — `codewiki doctor`: run diagnostics and print actionable fix hints.
//
// 6 checks (in order):
//   1. Binary on PATH
//   2. .codewiki/ exists
//   3. Index non-empty (file_count > 0)
//   4. Index freshness (last-modified < 24 h)
//   5. At least one agent wired (MCP config present)
//   6. Git hook installed (.git/hooks/post-commit contains "codewiki")
//
// --strict → exit non-zero if any check is in Warning or Error state.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::ui::glyphs;

// ── Check result ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub status: CheckStatus,
    pub label: &'static str,
    pub detail: String,
    pub hint: Option<String>,
}

impl CheckResult {
    fn ok(label: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Ok,
            label,
            detail: detail.into(),
            hint: None,
        }
    }
    fn warn(label: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Warn,
            label,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
    fn fail(label: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Fail,
            label,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

// ── Entry-point ───────────────────────────────────────────────────────────────

pub fn run(path: Option<PathBuf>, strict: bool) -> Result<()> {
    let root = path
        .unwrap_or_else(|| std::env::current_dir().expect("cannot determine working directory"));

    let g = glyphs::glyphs();

    println!("{}  CodeWiki diagnostics", g.bullet);
    println!();

    let checks = run_all_checks(&root);

    let col_width = 22usize;
    for c in &checks {
        let glyph = match c.status {
            CheckStatus::Ok => g.success,
            CheckStatus::Warn => g.warn,
            CheckStatus::Fail => g.error,
        };
        println!(
            "  {}  {:width$} {}",
            glyph,
            c.label,
            c.detail,
            width = col_width
        );
        if let Some(ref hint) = c.hint {
            println!("       -> Run: {}", hint);
        }
    }

    // Tally.
    let warns = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    let fails = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .count();

    println!();
    if warns == 0 && fails == 0 {
        println!("  All checks passed.");
    } else {
        let mut parts = Vec::new();
        if warns > 0 {
            parts.push(format!(
                "{} warning{}",
                warns,
                if warns == 1 { "" } else { "s" }
            ));
        }
        if fails > 0 {
            parts.push(format!(
                "{} error{}",
                fails,
                if fails == 1 { "" } else { "s" }
            ));
        }
        println!("  {}", parts.join(" · "));
        println!("  Run `codewiki setup` to fix automatically.");
    }

    // --strict: exit non-zero on any failure.
    if strict && (fails > 0 || warns > 0) {
        anyhow::bail!("doctor: {} failure(s), {} warning(s)", fails, warns);
    }

    Ok(())
}

// ── Individual checks ─────────────────────────────────────────────────────────

fn run_all_checks(root: &Path) -> Vec<CheckResult> {
    vec![
        check_binary_on_path(),
        check_index_exists(root),
        check_index_non_empty(root),
        check_index_freshness(root),
        check_agent_wired(root),
        check_git_hook(root),
    ]
}

/// Check 1: binary on PATH.
fn check_binary_on_path() -> CheckResult {
    let self_path = match std::env::current_exe().ok() {
        Some(p) => p,
        None => {
            return CheckResult::fail(
                "binary on PATH",
                "cannot determine running binary path",
                "export PATH=\"$HOME/.cargo/bin:$PATH\"",
            )
        }
    };

    // Walk PATH to find `codewiki`.
    if let Some(found) = find_on_path("codewiki") {
        let ra = std::fs::canonicalize(&found).unwrap_or(found.clone());
        let rb = std::fs::canonicalize(&self_path).unwrap_or(self_path.clone());
        if ra == rb {
            return CheckResult::ok("binary on PATH", self_path.display().to_string());
        }
        // Found but points to a different binary.
        return CheckResult::warn(
            "binary on PATH",
            format!(
                "PATH has {} but running binary is {}",
                found.display(),
                self_path.display()
            ),
            format!(
                "export PATH=\"{}:$PATH\"",
                self_path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
        );
    }

    // Not found at all.
    let parent = self_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "$HOME/.cargo/bin".to_string());
    CheckResult::fail(
        "binary on PATH",
        "codewiki not found on PATH",
        format!("export PATH=\"{}:$PATH\"", parent),
    )
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let candidate_exe = dir.join(format!("{}.exe", name));
            if candidate_exe.is_file() {
                return Some(candidate_exe);
            }
        }
    }
    None
}

/// Check 2: .codewiki/ exists.
fn check_index_exists(root: &Path) -> CheckResult {
    let db = root.join(".codewiki").join("codewiki.db");
    if db.exists() {
        CheckResult::ok("index initialized", db.display().to_string())
    } else {
        CheckResult::fail(
            "index initialized",
            ".codewiki/codewiki.db not found",
            "codewiki setup",
        )
    }
}

/// Check 3: index non-empty.
fn check_index_non_empty(root: &Path) -> CheckResult {
    use crate::commands::util::open_storage;
    use codewiki_storage::QueryHandle;

    match open_storage(root) {
        Err(_) => CheckResult::fail("files indexed", "index not readable", "codewiki setup"),
        Ok(storage) => match storage.get_stats() {
            Err(e) => CheckResult::fail(
                "files indexed",
                format!("stats error: {}", e),
                "codewiki index",
            ),
            Ok(stats) if stats.file_count == 0 => {
                CheckResult::fail("files indexed", "0 files in index", "codewiki index")
            }
            Ok(stats) => CheckResult::ok(
                "files indexed",
                format!("{} files · {} nodes", stats.file_count, stats.node_count),
            ),
        },
    }
}

/// Check 4: index freshness (< 24 h since last write to codewiki.db).
fn check_index_freshness(root: &Path) -> CheckResult {
    let db = root.join(".codewiki").join("codewiki.db");
    if !db.exists() {
        return CheckResult::fail("index fresh", "no index", "codewiki setup");
    }

    let metadata = match std::fs::metadata(&db) {
        Ok(m) => m,
        Err(e) => {
            return CheckResult::warn(
                "index fresh",
                format!("cannot stat DB: {}", e),
                "codewiki sync",
            );
        }
    };

    let modified = match metadata.modified() {
        Ok(t) => t,
        Err(_) => return CheckResult::ok("index fresh", "modification time unavailable"),
    };

    let age = match modified.elapsed() {
        Ok(d) => d,
        Err(_) => return CheckResult::ok("index fresh", "clock skew detected"),
    };

    const STALE_SECS: u64 = 24 * 60 * 60;
    if age.as_secs() > STALE_SECS {
        let hours = age.as_secs() / 3600;
        CheckResult::warn(
            "index fresh",
            format!("last indexed {} hours ago", hours),
            "codewiki sync",
        )
    } else {
        let mins = age.as_secs() / 60;
        CheckResult::ok("index fresh", format!("indexed {} min ago", mins))
    }
}

/// Check 5: at least one agent wired (MCP config present) at EITHER Global or
/// Local scope.
///
/// §4-C — a global registration (`codewiki install`) serves every project, but
/// a user may instead pin CodeWiki to this project with `--location local`.
/// Probing only Global would wrongly report a working local install as "no AI
/// agent configured", so we accept an agent wired at either location.
fn check_agent_wired(root: &Path) -> CheckResult {
    use crate::installer::targets::{all_targets, Location};

    // Local install detection resolves the per-project config from the current
    // working directory (the targets read cwd, not an arbitrary root), so only
    // probe Local when `root` IS the cwd. For `doctor --path <other>` we report
    // Global-only (cwd-independent) rather than inspect the wrong project's local
    // config. A global registration serves every project regardless of location.
    let check_local = std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd.canonicalize().ok())
        .zip(root.canonicalize().ok())
        .map(|(cwd, r)| cwd == r)
        .unwrap_or(false);

    // is_installed checks whether codewiki is already wired into the agent
    // config at the given location.
    let mut wired: Vec<String> = Vec::new();
    for t in all_targets() {
        let global = t.supports_location(Location::Global) && t.is_installed(Location::Global);
        let local =
            check_local && t.supports_location(Location::Local) && t.is_installed(Location::Local);
        if global && local {
            wired.push(format!("{} (global + local)", t.display_name()));
        } else if global {
            wired.push(format!("{} (global)", t.display_name()));
        } else if local {
            wired.push(format!("{} (local)", t.display_name()));
        }
    }

    if wired.is_empty() {
        CheckResult::fail(
            "agent wired",
            "no AI agent has CodeWiki configured (global or local)",
            "codewiki setup",
        )
    } else {
        CheckResult::ok("agent wired", wired.join(", "))
    }
}

/// Check 6: git hook installed (.git/hooks/post-commit contains "codewiki").
fn check_git_hook(root: &Path) -> CheckResult {
    let hook_path = root.join(".git").join("hooks").join("post-commit");

    if !root.join(".git").is_dir() {
        // Not a git repo — skip with OK.
        return CheckResult::ok("git hook", "not a git repo — hook not required");
    }

    if !hook_path.exists() {
        return CheckResult::fail(
            "git hook",
            ".git/hooks/post-commit not found",
            "codewiki setup   (re-running setup is safe)",
        );
    }

    let content = match std::fs::read_to_string(&hook_path) {
        Ok(c) => c,
        Err(e) => {
            return CheckResult::warn(
                "git hook",
                format!("cannot read hook: {}", e),
                "codewiki setup",
            );
        }
    };

    if content.contains("codewiki") {
        CheckResult::ok("git hook", hook_path.display().to_string())
    } else {
        CheckResult::fail(
            "git hook",
            "post-commit hook exists but does not call codewiki",
            "codewiki setup   (re-running setup is safe)",
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn empty_project() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    /// F8 (c): doctor reports missing index when .codewiki/ is absent.
    #[test]
    fn doctor_reports_missing_index() {
        let project = empty_project();
        let checks = run_all_checks(project.path());

        // Check 2: index initialized → must be Fail.
        let index_check = checks
            .iter()
            .find(|c| c.label == "index initialized")
            .unwrap();
        assert_eq!(
            index_check.status,
            CheckStatus::Fail,
            "expected Fail for missing index, got {:?}",
            index_check.status
        );
        // Hint must mention setup.
        let hint = index_check.hint.as_deref().unwrap_or("");
        assert!(
            hint.contains("setup") || hint.contains("init"),
            "hint should mention setup, got: {:?}",
            hint
        );
    }

    /// F8 (d): doctor --strict returns Err (non-zero) on failure.
    #[test]
    fn doctor_strict_exits_nonzero_on_failure() {
        let project = empty_project();
        // No .codewiki/ → several checks fail.
        let result = run(Some(project.path().to_path_buf()), /*strict=*/ true);
        assert!(
            result.is_err(),
            "expected Err from doctor --strict when checks fail, got Ok"
        );
    }

    /// doctor --strict returns Ok when all checks pass (smoke, relaxed conditions).
    #[test]
    fn doctor_non_strict_always_returns_ok() {
        let project = empty_project();
        // Non-strict should always return Ok regardless of check outcomes.
        let result = run(Some(project.path().to_path_buf()), /*strict=*/ false);
        assert!(
            result.is_ok(),
            "non-strict doctor should return Ok, got: {:?}",
            result
        );
    }

    /// Individual check: check_index_exists fails on a project without .codewiki/.
    #[test]
    fn check_index_exists_fails_when_missing() {
        let project = empty_project();
        let result = check_index_exists(project.path());
        assert_eq!(result.status, CheckStatus::Fail);
    }

    /// Individual check: check_git_hook passes on a non-git directory.
    #[test]
    fn check_git_hook_ok_on_non_git_repo() {
        let project = empty_project();
        let result = check_git_hook(project.path());
        // Non-git repos get a pass (not required).
        assert_eq!(result.status, CheckStatus::Ok);
    }

    /// check_git_hook fails when .git exists but hook is missing.
    #[test]
    fn check_git_hook_fails_when_hook_missing() {
        let project = empty_project();
        fs::create_dir_all(project.path().join(".git").join("hooks")).unwrap();
        let result = check_git_hook(project.path());
        assert_eq!(result.status, CheckStatus::Fail);
    }

    /// check_git_hook passes when post-commit contains "codewiki".
    #[test]
    fn check_git_hook_passes_when_hook_present() {
        let project = empty_project();
        let hooks_dir = project.path().join(".git").join("hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("post-commit"), "#!/bin/sh\ncodewiki sync\n").unwrap();
        let result = check_git_hook(project.path());
        assert_eq!(result.status, CheckStatus::Ok);
    }
}
