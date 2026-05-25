// T-430 — `uninit` subcommand: remove .codewiki/ from the project.

use anyhow::Result;
use std::path::PathBuf;

use crate::commands::util::resolve_root;

pub fn run(force: bool, path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let codewiki_dir = root.join(".codewiki");

    if !codewiki_dir.exists() {
        println!("No .codewiki/ directory found at {}", root.display());
        return Ok(());
    }

    let confirmed = if force || !is_tty() {
        true
    } else {
        // Prompt via cliclack.
        use cliclack::confirm;
        confirm(format!(
            "Delete .codewiki/ in {}? This removes the entire index.",
            root.display()
        ))
        .interact()
        .unwrap_or(false)
    };

    if !confirmed {
        println!("Aborted.");
        return Ok(());
    }

    std::fs::remove_dir_all(&codewiki_dir)?;
    println!("Removed .codewiki/ from {}", root.display());
    Ok(())
}

/// Return true if stdout is a real TTY.
fn is_tty() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: isatty is pure and has no side effects.
        unsafe { libc::isatty(libc::STDIN_FILENO) != 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
