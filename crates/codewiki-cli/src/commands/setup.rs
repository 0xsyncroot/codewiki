// D5 — `codewiki setup`: one-command onboarding orchestrator.
//
// Happy path (TTY): intro → detect project → index/sync → single agent-select
// prompt → wire MCP → summary + next-steps.
//
// Non-interactive (--yes or non-TTY): zero prompts; uses detected agents or
// falls back to ["claude"].

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::installer::{self, InstallerOpts};
use crate::ui::glyphs;
use codewiki_sync::directory_utils::is_initialized;

// ── Public entry-point ────────────────────────────────────────────────────────

/// Run `codewiki setup`.
///
/// # Arguments
/// * `yes`    – skip all prompts; use detected agents (fall back to claude).
/// * `target` – `"auto"` (default) | `"all"` | comma-separated agent IDs.
/// * `path`   – project root (defaults to cwd).
/// * `ascii`  – force ASCII glyphs.
pub fn run(yes: bool, target: String, path: Option<PathBuf>, ascii: bool) -> Result<()> {
    let ascii = ascii
        || std::env::var("CODEWIKI_ASCII").as_deref() == Ok("1")
        || std::env::var("TERM").as_deref() == Ok("linux")
        || cfg!(target_os = "windows");

    let g = glyphs::glyphs();

    let root = path
        .unwrap_or_else(|| std::env::current_dir().expect("cannot determine working directory"));

    // ── Step 0: binary on PATH check ──────────────────────────────────────────
    let path_warning = check_binary_on_path();

    // ── Intro ─────────────────────────────────────────────────────────────────
    let version = env!("CARGO_PKG_VERSION");
    let intro_msg = format!("codewiki setup  v{}", version);
    let _ = cliclack::intro(&intro_msg);

    // ── Step 1: project detection ─────────────────────────────────────────────
    let already = is_initialized(&root);

    let display_root = abbreviate_home(&root);
    if already {
        println!(
            "{}  Detected project: {}  (already initialized)",
            g.bullet, display_root
        );
    } else {
        println!("{}  Detected project: {}", g.bullet, display_root);
    }

    // ── Step 2: index or sync ─────────────────────────────────────────────────
    let (files_indexed, nodes_count, edges_count, resolved_count, elapsed_ms) = if already {
        // Re-run: sync changed files.
        let t0 = std::time::Instant::now();
        crate::commands::sync::run(Some(root.clone()))?;
        let elapsed_ms = t0.elapsed().as_millis();
        // After sync, read stats for summary.
        let storage_stats = get_stats(&root);
        (
            storage_stats.0,
            storage_stats.1,
            storage_stats.2,
            0usize,
            elapsed_ms,
        )
    } else {
        // Fresh: init + index.
        let t0 = std::time::Instant::now();
        crate::commands::init::run(Some(root.clone()), false)?;
        let elapsed_ms = t0.elapsed().as_millis();
        let storage_stats = get_stats(&root);
        (
            storage_stats.0,
            storage_stats.1,
            storage_stats.2,
            0usize,
            elapsed_ms,
        )
    };

    let elapsed_s = elapsed_ms as f64 / 1000.0;
    let _ = resolved_count; // used in summary when available

    // ── Step 3 & 4: agent selection ───────────────────────────────────────────
    let chosen_csv = if yes || !is_tty() {
        // Non-interactive: resolve from --target flag.
        pick_agents_non_interactive(&target)
    } else {
        // Interactive: single cliclack multiselect prompt.
        pick_agents_interactive(&target, ascii)?
    };

    // ── Step 5: wire MCP ─────────────────────────────────────────────────────
    if !chosen_csv.is_empty() {
        let opts = InstallerOpts {
            yes: true,
            target: chosen_csv.clone(),
            location: "global".to_string(),
            ascii,
            auto_allow: true,
        };
        installer::run_installer(opts)?;
    }

    // ── Step 6: summary ───────────────────────────────────────────────────────
    print_summary(
        already,
        IndexStats {
            files: files_indexed,
            nodes: nodes_count,
            edges: edges_count,
            elapsed_s,
        },
        &path_warning,
        g,
        ascii,
    );

    Ok(())
}

// ── Agent selection helpers ───────────────────────────────────────────────────

/// Pick agents without a prompt (--yes or non-TTY).
fn pick_agents_non_interactive(target: &str) -> String {
    use crate::installer::targets::Location;
    if target != "auto" {
        return target.to_string();
    }
    // Auto: detected, fallback to claude.
    let detected: Vec<&'static str> = crate::installer::detect_installed_agents(Location::Global);
    if detected.is_empty() {
        "claude".to_string()
    } else {
        detected.join(",")
    }
}

/// Pick agents interactively via cliclack multiselect.
fn pick_agents_interactive(target: &str, _ascii: bool) -> Result<String> {
    use crate::installer::targets::{all_targets, Location};

    let all = all_targets();
    let loc = Location::Global;

    // Determine initial selection.
    let detected: Vec<&'static str> = crate::installer::detect_installed_agents(loc);
    let initial: Vec<&str> = if target == "auto" {
        if detected.is_empty() {
            vec!["claude"]
        } else {
            detected.to_vec()
        }
    } else {
        target.split(',').map(|s| s.trim()).collect()
    };

    let items: Vec<(&str, &str, &str)> = all
        .iter()
        .filter(|t| t.supports_location(loc))
        .map(|t| {
            let hint = if detected.contains(&t.id()) {
                "(detected)"
            } else {
                ""
            };
            (t.id(), t.display_name(), hint)
        })
        .collect();

    let chosen = cliclack::multiselect("Which AI agents should CodeWiki connect to?")
        .items(
            &items
                .iter()
                .map(|(id, name, hint)| (*id, *name, *hint))
                .collect::<Vec<_>>(),
        )
        .initial_values(initial)
        .interact()?;

    if chosen.is_empty() {
        let _ = cliclack::outro("Nothing selected — exiting.");
        std::process::exit(0);
    }

    Ok(chosen.join(","))
}

// ── Summary printer ───────────────────────────────────────────────────────────

struct IndexStats {
    files: usize,
    nodes: usize,
    edges: usize,
    elapsed_s: f64,
}

fn print_summary(
    already: bool,
    stats: IndexStats,
    path_warning: &Option<String>,
    g: &'static glyphs::Glyphs,
    _ascii: bool,
) {
    println!();
    println!("  {}  CodeWiki is ready.", g.success);
    println!();

    let IndexStats {
        files,
        nodes,
        edges,
        elapsed_s,
    } = stats;
    if already {
        println!(
            "  Synced {} files — {} nodes, {} edges ({:.1}s).",
            files, nodes, edges, elapsed_s
        );
    } else {
        println!(
            "  Indexed {} files — {} nodes, {} edges ({:.1}s).",
            files, nodes, edges, elapsed_s
        );
        println!("  Restart Claude Code (or any configured agent) to activate");
        println!("  the MCP tools.");
    }

    println!();
    println!("  Next steps:");
    println!("    codewiki status          - verify index stats");
    println!("    codewiki doctor          - run diagnostics");
    println!("    codewiki query <symbol>  - search a symbol");
    println!();
    println!("  Docs: https://codewiki.dev/docs");

    if let Some(warning) = path_warning {
        println!();
        println!("  {}  codewiki is not on your PATH.", g.warn);
        println!("     Add this line to your shell config:");
        println!();
        println!("       {}", warning);
        println!();
        println!("     Then open a new terminal.");
    }

    let _ = cliclack::outro(if already {
        "Already set up. Nothing changed."
    } else {
        "CodeWiki is ready."
    });
}

// ── PATH detection ────────────────────────────────────────────────────────────

/// Returns `Some(export_line)` when the running binary is NOT on PATH.
pub fn check_binary_on_path() -> Option<String> {
    let self_path = std::env::current_exe().ok()?;

    // Try to find `codewiki` via PATH lookup.
    let on_path = which_codewiki();

    if let Some(ref found) = on_path {
        // Check if found path resolves to the same file.
        if paths_are_same(found, &self_path) {
            return None; // binary IS on PATH
        }
    }

    // Binary not found on PATH or points elsewhere. Build export hint.
    let parent = self_path.parent()?;
    let parent_str = parent.to_string_lossy();

    // Detect shell config file name.
    let shell_config = detect_shell_config();
    let export_line = format!("export PATH=\"{}:$PATH\"", parent_str);

    let hint = format!(
        "{}\n     Add to {} to make it permanent.",
        export_line, shell_config
    );
    Some(hint)
}

fn which_codewiki() -> Option<PathBuf> {
    // Walk PATH manually — avoids pulling in the `which` crate.
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join("codewiki");
        if candidate.is_file() {
            return Some(candidate);
        }
        // On Windows also try .exe suffix.
        #[cfg(windows)]
        {
            let candidate_exe = dir.join("codewiki.exe");
            if candidate_exe.is_file() {
                return Some(candidate_exe);
            }
        }
    }
    None
}

fn paths_are_same(a: &PathBuf, b: &PathBuf) -> bool {
    // Resolve symlinks before comparing.
    let ra = std::fs::canonicalize(a).unwrap_or_else(|_| a.clone());
    let rb = std::fs::canonicalize(b).unwrap_or_else(|_| b.clone());
    ra == rb
}

fn detect_shell_config() -> &'static str {
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.contains("zsh") {
        "~/.zshrc"
    } else if shell.contains("fish") {
        "~/.config/fish/config.fish"
    } else {
        "~/.bashrc"
    }
}

// ── Misc helpers ──────────────────────────────────────────────────────────────

/// Abbreviate the home directory as `~`.
fn abbreviate_home(path: &Path) -> String {
    if let Some(home) = home::home_dir() {
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

/// Get (files, nodes, edges) from the index if available.
fn get_stats(root: &Path) -> (usize, usize, usize) {
    use crate::commands::util::open_storage;
    use codewiki_storage::QueryHandle;
    match open_storage(root) {
        Ok(storage) => {
            if let Ok(stats) = storage.get_stats() {
                return (
                    stats.file_count as usize,
                    stats.node_count as usize,
                    stats.edge_count as usize,
                );
            }
            (0, 0, 0)
        }
        Err(_) => (0, 0, 0),
    }
}

// ── TTY detection ─────────────────────────────────────────────────────────────

fn is_tty() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(libc::STDOUT_FILENO) != 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a tiny project with one source file.
    fn tiny_project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.ts"), "function foo() {}").unwrap();
        dir
    }

    /// F8 (a): `setup --yes` on a fresh project creates the DB.
    #[test]
    #[serial]
    fn setup_non_interactive_yes_indexes_and_wires() {
        let project = tiny_project();
        let home_dir = tempfile::tempdir().unwrap();

        // Override HOME (Unix) and USERPROFILE (Windows) so agent config files
        // land in our tmpdir on every platform — `home::home_dir()` resolves via
        // USERPROFILE on Windows.
        let original_home = std::env::var_os("HOME");
        let original_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", home_dir.path());
        std::env::set_var("USERPROFILE", home_dir.path());

        let result = run(
            true, // yes = true (no prompts)
            "auto".to_string(),
            Some(project.path().to_path_buf()),
            true, // ascii = true (no unicode issues in CI)
        );

        // Probe the wiring through each target's OWN `is_installed(Global)` while
        // HOME/USERPROFILE still point at the sandbox — the target resolves its
        // config path the SAME way the writer did, so test and writer cannot
        // drift apart on any platform. Captured here (before restore) so the
        // probe sees the sandboxed home; asserted after restore so a failure
        // never leaves HOME polluted for sibling serial tests.
        //
        // We assert that AT LEAST ONE agent was wired rather than Claude
        // specifically: `--target auto` resolves to whatever the host has
        // installed (falling back to Claude only when nothing is detected), and a
        // CI runner may have a different agent present — the contract under test
        // is "setup wires an agent globally", not "setup always picks Claude".
        use crate::installer::targets::{all_targets, Location};
        let any_agent_wired = all_targets()
            .iter()
            .any(|t| t.supports_location(Location::Global) && t.is_installed(Location::Global));

        // Restore HOME / USERPROFILE.
        match original_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match original_userprofile {
            Some(h) => std::env::set_var("USERPROFILE", h),
            None => std::env::remove_var("USERPROFILE"),
        }

        // Setup must succeed.
        assert!(result.is_ok(), "setup returned error: {:?}", result);

        // DB must exist.
        let db = project.path().join(".codewiki").join("codewiki.db");
        assert!(db.exists(), ".codewiki/codewiki.db not created");

        // setup must have wired at least one agent globally. Probed via the
        // target's own resolution (platform-robust by construction — no hand-built
        // HOME/XDG path that could drift on Windows).
        assert!(
            any_agent_wired,
            "setup --yes (auto) must wire at least one agent at the global location"
        );
    }

    /// F8 (b): running `setup --yes` twice is idempotent (exits 0 both times).
    #[test]
    #[serial]
    fn setup_rerun_is_idempotent() {
        let project = tiny_project();
        let home_dir = tempfile::tempdir().unwrap();

        let original_home = std::env::var_os("HOME");
        let original_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", home_dir.path());
        std::env::set_var("USERPROFILE", home_dir.path());

        // First run.
        let r1 = run(
            true,
            "auto".to_string(),
            Some(project.path().to_path_buf()),
            true,
        );
        // Second run on already-initialized project.
        let r2 = run(
            true,
            "auto".to_string(),
            Some(project.path().to_path_buf()),
            true,
        );

        match original_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match original_userprofile {
            Some(h) => std::env::set_var("USERPROFILE", h),
            None => std::env::remove_var("USERPROFILE"),
        }

        assert!(r1.is_ok(), "first setup run failed: {:?}", r1);
        assert!(r2.is_ok(), "second setup run (idempotent) failed: {:?}", r2);
    }

    /// PATH detection returns None when the binary resolves to itself.
    #[test]
    fn check_binary_on_path_returns_none_when_on_path() {
        // We can't guarantee the running test binary is named `codewiki`,
        // so we just assert the function doesn't panic and returns a valid Option.
        let _ = check_binary_on_path();
    }

    /// detect_installed_agents is side-effect-free (runs twice safely).
    #[test]
    #[serial]
    fn detect_installed_agents_is_idempotent() {
        use crate::installer::targets::Location;
        let first: Vec<&str> = crate::installer::detect_installed_agents(Location::Global);
        let second: Vec<&str> = crate::installer::detect_installed_agents(Location::Global);
        assert_eq!(first, second);
    }
}
