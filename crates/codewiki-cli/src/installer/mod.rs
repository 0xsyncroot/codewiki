// T-438 — Installer orchestrator.
// T-439 — Uninstaller.
// Uses cliclack for interactive prompts; bypassed with --yes.

pub mod shared;
pub mod targets;

use anyhow::Result;
use targets::{all_targets, Location};

// ── Options ───────────────────────────────────────────────────────────────────

/// Options for the `codewiki install` command.
#[derive(Debug, Clone)]
pub struct InstallerOpts {
    /// Skip all prompts.
    pub yes: bool,
    /// Comma-separated targets: "auto" | "all" | "none" | "claude,cursor,…"
    pub target: String,
    /// Location: "global" | "local"
    pub location: String,
    /// Use ASCII glyphs.
    pub ascii: bool,
    /// Auto-allow MCP tools (Claude only).
    pub auto_allow: bool,
}

/// Options for the `codewiki uninstall` command.
#[derive(Debug, Clone)]
pub struct UninstallOpts {
    #[allow(dead_code)]
    pub yes: bool,
    pub target: String,
    pub location: String,
}

// ── Target resolution ─────────────────────────────────────────────────────────

fn resolve_target_ids(flag: &str, loc: Location) -> Result<Vec<&'static str>> {
    let all = all_targets();

    match flag {
        "auto" => {
            // Return detected targets; fall back to ["claude"] if none found.
            let detected: Vec<&'static str> = all
                .iter()
                .filter(|t| t.supports_location(loc) && t.detect(loc).installed)
                .map(|t| t.id())
                .collect();
            if detected.is_empty() {
                Ok(vec!["claude"])
            } else {
                Ok(detected)
            }
        }
        "all" => Ok(all.iter().map(|t| t.id()).collect()),
        "none" => Ok(vec![]),
        csv => {
            let ids: Vec<&str> = csv.split(',').map(|s| s.trim()).collect();
            let valid: Vec<&'static str> = all
                .iter()
                .filter(|t| ids.contains(&t.id()))
                .map(|t| t.id())
                .collect();
            for id in &ids {
                if !all.iter().any(|t| t.id() == *id) {
                    return Err(anyhow::anyhow!("Unknown target: '{}'. Valid: claude, cursor, codex, opencode, hermes", id));
                }
            }
            Ok(valid)
        }
    }
}

// ── Installer ─────────────────────────────────────────────────────────────────

/// Run the interactive/non-interactive installer.
///
/// Acceptance (T-438): `HOME=$(mktemp -d) codewiki install --yes --target claude --location global`
/// exits 0; `$HOME/.claude.json` exists; `jq .mcpServers.codewiki.args` == `["serve","--mcp"]`.
#[tracing::instrument(level = "debug", skip(opts))]
pub fn run_installer(opts: InstallerOpts) -> Result<()> {
    // Glyph selection (T-441).
    let ascii = opts.ascii
        || std::env::var("CODEWIKI_ASCII").as_deref() == Ok("1")
        || std::env::var("TERM").as_deref() == Ok("linux")
        || cfg!(target_os = "windows");

    let loc: Location = opts.location.parse()?;
    let all_targets = all_targets();

    // ── Target selection ──────────────────────────────────────────────────────
    // Skip the interactive prompt when:
    //   • --yes is given (full non-interactive mode), OR
    //   • stdout is not a TTY (scripted / piped), OR
    //   • --target was explicitly set to something other than the "auto"
    //     default (e.g. `--target claude` or `--target all`).
    let explicit_target = opts.target != "auto";
    let selected_ids: Vec<&'static str> = if opts.yes || !is_tty() || explicit_target {
        resolve_target_ids(&opts.target, loc)?
    } else {
        // Interactive: use cliclack multi_select.
        cliclack::intro(if ascii { "codewiki installer" } else { "◆ codewiki installer" })?;

        let all_ids: Vec<(&'static str, &'static str, bool)> = all_targets
            .iter()
            .filter(|t| t.supports_location(loc))
            .map(|t| {
                let detected = t.detect(loc);
                (t.id(), t.display_name(), detected.installed)
            })
            .collect();

        let initial: Vec<&str> = all_ids
            .iter()
            .filter(|(_, _, detected)| *detected)
            .map(|(id, _, _)| *id)
            .collect();

        let initial = if initial.is_empty() { vec!["claude"] } else { initial };

        let items: Vec<(&str, &str, &str)> = all_ids
            .iter()
            .map(|(id, name, _)| (*id, *name, ""))
            .collect();

        let chosen = cliclack::multiselect("Select targets to configure")
            .items(
                &items
                    .iter()
                    .map(|(id, name, hint)| (*id, *name, *hint))
                    .collect::<Vec<_>>(),
            )
            .initial_values(initial)
            .interact()?;

        if chosen.is_empty() {
            cliclack::outro("Nothing selected — exiting.")?;
            return Ok(());
        }

        chosen
    };

    if selected_ids.is_empty() {
        if !opts.yes {
            let _ = cliclack::outro("Nothing selected — exiting.");
        } else {
            println!("No targets selected.");
        }
        return Ok(());
    }

    // ── Location selection (interactive only, when ambiguous) ─────────────────
    // Already resolved via --location flag in non-interactive mode.

    // ── Per-target install loop ───────────────────────────────────────────────
    let install_opts = targets::InstallOptions {
        auto_allow: opts.auto_allow,
        project_root: None,
    };

    if !opts.yes {
        let sp = cliclack::spinner();
        sp.start("Writing configs…");
        for id in &selected_ids {
            if let Some(target) = all_targets.iter().find(|t| t.id() == *id) {
                match target.install(loc, &install_opts) {
                    Ok(result) => {
                        for file in &result.files {
                            let glyph = if ascii { "[OK]" } else { "✓" };
                            eprintln!("  {} {} ({})", glyph, file.path.display(), file.action);
                        }
                        for note in &result.notes {
                            eprintln!("  {} {}", if ascii { "[i]" } else { "ℹ" }, note);
                        }
                    }
                    Err(e) => {
                        eprintln!("  {} {}: {}", if ascii { "[ERR]" } else { "✗" }, id, e);
                    }
                }
            }
        }
        sp.stop(if ascii { "[OK] Done." } else { "✓ Done." });
        let _ = cliclack::outro("Restart your AI agents to apply changes.");
    } else {
        // Non-interactive: plain stdout.
        for id in &selected_ids {
            if let Some(target) = all_targets.iter().find(|t| t.id() == *id) {
                match target.install(loc, &install_opts) {
                    Ok(result) => {
                        for file in &result.files {
                            println!("{}: {} ({})", id, file.path.display(), file.action);
                        }
                        for note in &result.notes {
                            println!("{}: note: {}", id, note);
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: error: {}", id, e);
                    }
                }
            }
        }
        println!("Install complete. Restart your AI agents to apply changes.");
    }

    Ok(())
}

// ── Uninstaller ───────────────────────────────────────────────────────────────

#[tracing::instrument(level = "debug", skip(opts))]
pub fn run_uninstaller(opts: UninstallOpts) -> Result<()> {
    let loc: Location = opts.location.parse()?;
    let all_targets = all_targets();

    let selected_ids = resolve_target_ids(&opts.target, loc)?;

    for id in &selected_ids {
        if let Some(target) = all_targets.iter().find(|t| t.id() == *id) {
            match target.uninstall(loc) {
                Ok(result) => {
                    for file in &result.files {
                        println!("{}: {} ({})", id, file.path.display(), file.action);
                    }
                }
                Err(e) => {
                    eprintln!("{}: uninstall error: {}", id, e);
                }
            }
        }
    }

    println!("Uninstall complete. Note: .codewiki/ index is preserved — run `codewiki uninit` to remove it.");
    Ok(())
}

// ── detect_installed_agents ───────────────────────────────────────────────────

/// Returns the IDs of AI agents detected as installed on this machine.
///
/// This is a **pure read** — no config files are written or modified.
/// `setup.rs` and `doctor.rs` call this to display detected agents before
/// any prompt or wiring step.
pub fn detect_installed_agents(loc: Location) -> Vec<&'static str> {
    all_targets()
        .iter()
        .filter(|t| t.supports_location(loc) && t.detect(loc).installed)
        .map(|t| t.id())
        .collect()
}

// ── TTY detection ─────────────────────────────────────────────────────────────

fn is_tty() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: isatty is a simple syscall with no side effects.
        unsafe { libc::isatty(libc::STDOUT_FILENO) != 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
