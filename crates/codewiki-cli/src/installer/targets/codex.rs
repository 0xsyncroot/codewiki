// T-435 — Codex CLI target (global only).
// Writes: ~/.codex/config.toml   ([mcp_servers.codewiki] block)
//         ~/.codex/AGENTS.md      (instructions marker block)

use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use codewiki_mcp::constants::{MCP_BINARY_NAME, MCP_SERVE_ARGS_GLOBAL};

use crate::installer::shared::{
    atomic_write, instructions_template, remove_marked_section, replace_or_append_marked_section,
    FileAction, RemoveAction, SectionAction, WriteResult, CODEWIKI_SECTION_END,
    CODEWIKI_SECTION_START,
};

use super::{AgentTarget, DetectionResult, InstallOptions, Location};

// ── Path helpers ──────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    home::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn config_dir() -> PathBuf {
    home_dir().join(".codex")
}

fn toml_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

fn instructions_path() -> PathBuf {
    config_dir().join("AGENTS.md")
}

const TOML_HEADER: &str = "mcp_servers.codewiki";

// ── Hand-rolled TOML helpers ──────────────────────────────────────────────────

/// Build the `[mcp_servers.codewiki]` TOML block.
fn build_toml_block() -> String {
    let args_toml = MCP_SERVE_ARGS_GLOBAL
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[{}]\ncommand = \"{}\"\nargs = [{}]\n",
        TOML_HEADER, MCP_BINARY_NAME, args_toml
    )
}

/// Insert or replace the `[mcp_servers.codewiki]` block in `existing` TOML text.
/// Returns `(new_content, "unchanged" | "updated")`.
fn upsert_toml_table(existing: &str, header: &str, block: &str) -> (String, &'static str) {
    let header_line = format!("[{}]", header);

    // Find the start of the existing block.
    if let Some(start) = existing
        .lines()
        .enumerate()
        .find(|(_, l)| l.trim() == header_line.trim())
        .map(|(i, _)| i)
    {
        // Find the end: next top-level section or EOF.
        let lines: Vec<&str> = existing.lines().collect();
        let mut end = lines.len();
        for (i, line) in lines.iter().enumerate().skip(start + 1) {
            if line.starts_with('[') && !line.starts_with("[[") {
                end = i;
                break;
            }
        }

        // Build the existing block text for comparison.
        let existing_block_lines = &lines[start..end];
        // Strip trailing blank lines from the existing block for comparison.
        let mut existing_block = existing_block_lines
            .iter()
            .rev()
            .skip_while(|l| l.trim().is_empty())
            .cloned()
            .collect::<Vec<&str>>();
        existing_block.reverse();
        let existing_block_text = existing_block.join("\n") + "\n";

        let block_trimmed = block.trim_end_matches('\n').to_string() + "\n";
        if existing_block_text == block_trimmed {
            return (existing.to_string(), "unchanged");
        }

        // Splice out old block, splice in new.
        let before_lines = &lines[..start];
        let after_lines = &lines[end..];
        let mut parts: Vec<&str> = before_lines.to_vec();
        parts.extend(block_trimmed.lines());
        parts.extend(after_lines.iter().cloned());

        let mut result = parts.join("\n");
        if !result.ends_with('\n') {
            result.push('\n');
        }
        (result, "updated")
    } else {
        // Append the block.
        let trimmed = existing.trim_end();
        let sep = if trimmed.is_empty() { "" } else { "\n\n" };
        let result = format!("{}{}{}", trimmed, sep, block);
        (result, "updated")
    }
}

/// Remove the `[mcp_servers.codewiki]` block from TOML text.
fn remove_toml_table(content: &str, header: &str) -> (String, &'static str) {
    let header_line = format!("[{}]", header);
    let lines: Vec<&str> = content.lines().collect();

    let start = match lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.trim() == header_line.trim())
        .map(|(i, _)| i)
    {
        Some(i) => i,
        None => return (content.to_string(), "not-found"),
    };

    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.starts_with('[') && !line.starts_with("[[") {
            end = i;
            break;
        }
    }

    let mut result_lines: Vec<&str> = lines[..start].to_vec();
    result_lines.extend_from_slice(&lines[end..]);

    // Trim trailing blank lines.
    while result_lines
        .last()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        result_lines.pop();
    }

    let mut result = result_lines.join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    (result, "removed")
}

// ── Per-file writers ──────────────────────────────────────────────────────────

#[tracing::instrument(level = "debug")]
fn write_mcp_entry() -> Result<(PathBuf, FileAction)> {
    let file = toml_config_path();
    let dir = config_dir();
    fs::create_dir_all(&dir)?;

    let existing = if file.exists() {
        fs::read_to_string(&file)?
    } else {
        String::new()
    };
    let created = existing.is_empty();

    let block = build_toml_block();
    let (next_content, action) = upsert_toml_table(&existing, TOML_HEADER, &block);

    if action == "unchanged" {
        return Ok((file, FileAction::Unchanged));
    }

    atomic_write(&file, &next_content)?;
    Ok((
        file,
        if created {
            FileAction::Created
        } else {
            FileAction::Updated
        },
    ))
}

#[tracing::instrument(level = "debug")]
fn write_instructions_entry() -> Result<(PathBuf, FileAction)> {
    let file = instructions_path();
    let dir = config_dir();
    fs::create_dir_all(&dir)?;

    let action = replace_or_append_marked_section(
        &file,
        instructions_template(),
        CODEWIKI_SECTION_START,
        CODEWIKI_SECTION_END,
    )?;
    let mapped = match action {
        SectionAction::Created => FileAction::Created,
        SectionAction::Unchanged => FileAction::Unchanged,
        _ => FileAction::Updated,
    };
    Ok((file, mapped))
}

// ── CodexTarget impl ──────────────────────────────────────────────────────────

pub struct CodexTarget;

impl AgentTarget for CodexTarget {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex CLI"
    }

    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        if loc != Location::Global {
            return DetectionResult {
                installed: false,
                already_configured: false,
                config_path: None,
            };
        }
        let toml_path = toml_config_path();
        let already_configured = if toml_path.exists() {
            fs::read_to_string(&toml_path)
                .map(|s| s.contains(&format!("[{}]", TOML_HEADER)))
                .unwrap_or(false)
        } else {
            false
        };
        DetectionResult {
            installed: config_dir().exists(),
            already_configured,
            config_path: Some(toml_path),
        }
    }

    #[tracing::instrument(level = "debug", skip(self, _opts))]
    fn install(&self, loc: Location, _opts: &InstallOptions) -> Result<WriteResult> {
        if loc != Location::Global {
            let mut result = WriteResult::new();
            result.notes.push(
                "Codex CLI has no project-local config — re-run with --location=global to install."
                    .to_string(),
            );
            return Ok(result);
        }

        let mut result = WriteResult::new();
        let (p, a) = write_mcp_entry()?;
        result.push(p, a);
        let (p, a) = write_instructions_entry()?;
        result.push(p, a);
        Ok(result)
    }

    #[tracing::instrument(level = "debug", skip(self))]
    fn uninstall(&self, loc: Location) -> Result<WriteResult> {
        if loc != Location::Global {
            return Ok(WriteResult::new());
        }

        let mut result = WriteResult::new();

        let toml_path = toml_config_path();
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path)?;
            let (next_content, action) = remove_toml_table(&content, TOML_HEADER);
            if action == "removed" {
                if next_content.trim().is_empty() {
                    let _ = fs::remove_file(&toml_path);
                } else {
                    let trimmed = next_content.trim_end().to_string() + "\n";
                    atomic_write(&toml_path, &trimmed)?;
                }
                result.push(&toml_path, FileAction::Removed);
            } else {
                result.push(&toml_path, FileAction::NotFound);
            }
        } else {
            result.push(&toml_path, FileAction::NotFound);
        }

        let instr = instructions_path();
        let action = remove_marked_section(&instr, CODEWIKI_SECTION_START, CODEWIKI_SECTION_END)?;
        let mapped = match action {
            RemoveAction::Removed => FileAction::Removed,
            _ => FileAction::NotFound,
        };
        result.push(instr, mapped);

        Ok(result)
    }

    fn print_config(&self, loc: Location) -> String {
        if loc != Location::Global {
            return "# Codex CLI has no project-local config — use --location=global.\n"
                .to_string();
        }
        format!(
            "# Add to {}\n\n{}\n",
            toml_config_path().display(),
            build_toml_block()
        )
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        if loc != Location::Global {
            return vec![];
        }
        vec![toml_config_path(), instructions_path()]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use codewiki_mcp::constants::MCP_SERVE_ARGS_GLOBAL;

    #[test]
    fn toml_block_contains_correct_args() {
        let block = build_toml_block();
        assert!(block.contains("[mcp_servers.codewiki]"));
        assert!(block.contains("command = \"codewiki\""));
        assert!(block.contains("\"serve\""));
        assert!(block.contains("\"--mcp\""));
        // Verify against the canonical constant.
        for arg in MCP_SERVE_ARGS_GLOBAL {
            assert!(block.contains(arg));
        }
    }

    #[test]
    fn upsert_toml_table_idempotent() {
        let block = build_toml_block();
        let (content1, _) = upsert_toml_table("", TOML_HEADER, &block);
        let (content2, action) = upsert_toml_table(&content1, TOML_HEADER, &block);
        assert_eq!(action, "unchanged");
        assert_eq!(content1, content2);
    }

    #[test]
    fn remove_toml_table_removes_block() {
        let block = build_toml_block();
        let (with_block, _) = upsert_toml_table("", TOML_HEADER, &block);
        let (removed, action) = remove_toml_table(&with_block, TOML_HEADER);
        assert_eq!(action, "removed");
        assert!(!removed.contains("[mcp_servers.codewiki]"));
    }
}
