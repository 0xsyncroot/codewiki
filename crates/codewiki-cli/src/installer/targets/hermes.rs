// T-437 — Hermes Agent target (global only).
// Writes: $HERMES_HOME/config.yaml
// Uses line-based text manipulation (no YAML parser dependency) to upsert
// `mcp_servers.codewiki` and `platform_toolsets.cli: mcp-codewiki`.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use codewiki_mcp::constants::MCP_BINARY_NAME;

use crate::installer::shared::{atomic_write, FileAction, WriteResult};

use super::{AgentTarget, DetectionResult, InstallOptions, Location};

// ── Path helpers ──────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    home::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn hermes_home() -> PathBuf {
    std::env::var("HERMES_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".hermes"))
}

fn config_path() -> PathBuf {
    hermes_home().join("config.yaml")
}

// ── YAML rendering ────────────────────────────────────────────────────────────

fn render_codewiki_mcp_child() -> Vec<String> {
    vec![
        format!("  codewiki:"),
        format!("    command: {}", MCP_BINARY_NAME),
        format!("    args:"),
        format!("      - serve"),
        format!("      - --mcp"),
        format!("    timeout: 120"),
        format!("    connect_timeout: 60"),
        format!("    enabled: true"),
    ]
}

fn render_codewiki_mcp_block() -> Vec<String> {
    let mut lines = vec!["mcp_servers:".to_string()];
    lines.extend(render_codewiki_mcp_child());
    lines
}

// ── Line-based YAML manipulation ──────────────────────────────────────────────

fn split_lines(content: &str) -> Vec<String> {
    content.replace("\r\n", "\n").replace('\r', "\n")
        .split('\n')
        .map(|s| s.to_string())
        .collect()
}

fn join_lines(mut lines: Vec<String>) -> String {
    while lines.last().map(|l: &String| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let mut result = lines.join("\n");
    result.push('\n');
    result
}

/// Find the range [start, end) of a top-level `key:` section.
fn top_level_range(lines: &[String], key: &str) -> Option<(usize, usize)> {
    let target = format!("{}:", key);
    let start = lines.iter().position(|l| l.trim() == target.as_str())?;
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        // Top-level key: starts with a non-space char followed by any chars and ':'
        if line.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '-') {
            end = i;
            break;
        }
    }
    Some((start, end))
}

/// Find a child section range `  child:` within parent range [ps, pe).
fn child_range(lines: &[String], parent: (usize, usize), child: &str) -> Option<(usize, usize)> {
    let (ps, pe) = parent;
    let target = format!("  {}:", child);
    let start = lines[ps + 1..pe]
        .iter()
        .position(|l| l == &target)
        .map(|i| ps + 1 + i)?;

    let mut end = pe;
    for (i, line) in lines.iter().enumerate().skip(start + 1).take_while(|(i, _)| *i < pe) {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("  ") && !line.starts_with("   ") {
            end = i;
            break;
        }
    }
    // Trim trailing blank lines from child range.
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    Some((start, end))
}

fn has_codewiki_mcp_server(content: &str) -> bool {
    let lines = split_lines(content);
    if let Some(parent) = top_level_range(&lines, "mcp_servers") {
        child_range(&lines, parent, "codewiki").is_some()
    } else {
        false
    }
}

fn upsert_codewiki_mcp_server(content: &str) -> String {
    let mut lines = split_lines(content);
    let replacement = render_codewiki_mcp_child();

    if let Some(parent) = top_level_range(&lines, "mcp_servers") {
        if let Some(child) = child_range(&lines, parent, "codewiki") {
            let existing: Vec<String> = lines[child.0..child.1].to_vec();
            if existing == replacement {
                return join_lines(lines);
            }
            lines.splice(child.0..child.1, replacement);
        } else {
            let insert_at = parent.1;
            lines.splice(insert_at..insert_at, replacement);
        }
    } else {
        if let Some(last) = lines.last() {
            if last.trim().is_empty() {
                lines.pop();
            }
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(render_codewiki_mcp_block());
    }

    join_lines(lines)
}

fn remove_codewiki_mcp_server(content: &str) -> String {
    let mut lines = split_lines(content);
    if let Some(parent) = top_level_range(&lines, "mcp_servers") {
        if let Some(child) = child_range(&lines, parent, "codewiki") {
            lines.drain(child.0..child.1);
        }
    }
    join_lines(lines)
}

fn upsert_codewiki_toolset(content: &str) -> String {
    let mut lines = split_lines(content);

    if let Some(parent) = top_level_range(&lines, "platform_toolsets") {
        if let Some(cli) = child_range(&lines, parent, "cli") {
            // Check if mcp-codewiki is already present.
            let has_entry = lines[cli.0 + 1..cli.1]
                .iter()
                .any(|l| l.trim() == "- mcp-codewiki");
            if !has_entry {
                lines.insert(cli.1, "    - mcp-codewiki".to_string());
            }
        } else {
            let insert_at = parent.1;
            let new_lines = vec![
                "  cli:".to_string(),
                "    - hermes-cli".to_string(),
                "    - mcp-codewiki".to_string(),
            ];
            lines.splice(insert_at..insert_at, new_lines);
        }
    } else {
        if let Some(last) = lines.last() {
            if last.trim().is_empty() {
                lines.pop();
            }
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(vec![
            "platform_toolsets:".to_string(),
            "  cli:".to_string(),
            "    - hermes-cli".to_string(),
            "    - mcp-codewiki".to_string(),
        ]);
    }

    join_lines(lines)
}

fn remove_codewiki_toolset(content: &str) -> String {
    let lines = split_lines(content);
    if let Some(parent) = top_level_range(&lines, "platform_toolsets") {
        if let Some(cli) = child_range(&lines, parent, "cli") {
            let filtered: Vec<String> = lines
                .iter()
                .enumerate()
                .filter(|(i, l)| {
                    // Keep lines outside the cli child range entirely.
                    if *i <= cli.0 || *i >= cli.1 {
                        return true;
                    }
                    // Inside the range: keep anything that isn't "- mcp-codewiki".
                    l.trim() != "- mcp-codewiki"
                })
                .map(|(_, l)| l.clone())
                .collect();
            return join_lines(filtered);
        }
    }
    join_lines(lines)
}

fn ensure_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

// ── Per-file writer ───────────────────────────────────────────────────────────

#[tracing::instrument(level = "debug")]
fn write_hermes_config() -> Result<(PathBuf, FileAction)> {
    let file = config_path();
    let dir = hermes_home();
    fs::create_dir_all(&dir)?;

    let existed = file.exists();
    let before = if existed {
        fs::read_to_string(&file).unwrap_or_default()
    } else {
        String::new()
    };

    let after_mcp = upsert_codewiki_mcp_server(&before);
    let after = upsert_codewiki_toolset(&after_mcp);

    if after == before {
        return Ok((file, FileAction::Unchanged));
    }

    atomic_write(&file, &ensure_trailing_newline(&after))?;
    Ok((file, if existed { FileAction::Updated } else { FileAction::Created }))
}

// ── HermesTarget impl ─────────────────────────────────────────────────────────

pub struct HermesTarget;

impl AgentTarget for HermesTarget {
    fn id(&self) -> &'static str {
        "hermes"
    }

    fn display_name(&self) -> &'static str {
        "Hermes Agent"
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
        let file = config_path();
        let content = fs::read_to_string(&file).unwrap_or_default();
        DetectionResult {
            installed: hermes_home().exists() || file.exists(),
            already_configured: has_codewiki_mcp_server(&content),
            config_path: Some(file),
        }
    }

    #[tracing::instrument(level = "debug", skip(self, _opts))]
    fn install(&self, loc: Location, _opts: &InstallOptions) -> Result<WriteResult> {
        if loc != Location::Global {
            let mut result = WriteResult::new();
            result.notes.push(
                "Hermes Agent uses $HERMES_HOME/config.yaml; re-run with --location=global."
                    .to_string(),
            );
            return Ok(result);
        }

        let mut result = WriteResult::new();
        let (p, a) = write_hermes_config()?;
        result.push(p, a);
        result.notes.push(
            "Start a new Hermes session for MCP changes to take effect.".to_string(),
        );
        Ok(result)
    }

    #[tracing::instrument(level = "debug", skip(self))]
    fn uninstall(&self, loc: Location) -> Result<WriteResult> {
        if loc != Location::Global {
            return Ok(WriteResult::new());
        }

        let mut result = WriteResult::new();
        let file = config_path();

        if !file.exists() {
            result.push(&file, FileAction::NotFound);
            return Ok(result);
        }

        let before = fs::read_to_string(&file).unwrap_or_default();
        let after = remove_codewiki_toolset(&remove_codewiki_mcp_server(&before));

        if after == before {
            result.push(&file, FileAction::NotFound);
        } else {
            atomic_write(&file, &ensure_trailing_newline(&after))?;
            result.push(&file, FileAction::Removed);
        }

        Ok(result)
    }

    fn print_config(&self, loc: Location) -> String {
        if loc != Location::Global {
            return "# Hermes Agent uses $HERMES_HOME/config.yaml; use --location=global.\n"
                .to_string();
        }
        let block_lines = render_codewiki_mcp_block()
            .into_iter()
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "# Add to {}\n\n{}\n\nplatform_toolsets:\n  cli:\n    - hermes-cli\n    - mcp-codewiki\n",
            config_path().display(),
            block_lines
        )
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        if loc == Location::Global {
            vec![config_path()]
        } else {
            vec![]
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_mcp_server_idempotent() {
        let content1 = upsert_codewiki_mcp_server("");
        let content2 = upsert_codewiki_mcp_server(&content1);
        assert_eq!(content1, content2);
        assert!(content1.contains("codewiki:"));
        assert!(content1.contains("command: codewiki"));
    }

    #[test]
    fn upsert_toolset_idempotent() {
        let content1 = upsert_codewiki_toolset("");
        let content2 = upsert_codewiki_toolset(&content1);
        assert_eq!(content1, content2);
        assert!(content1.contains("mcp-codewiki"));
    }

    #[test]
    fn remove_mcp_server_removes_codewiki() {
        let content = upsert_codewiki_mcp_server("");
        let removed = remove_codewiki_mcp_server(&content);
        assert!(!removed.contains("  codewiki:"));
    }

    #[test]
    fn has_codewiki_mcp_server_detects() {
        let empty = "";
        assert!(!has_codewiki_mcp_server(empty));
        let with_entry = upsert_codewiki_mcp_server(empty);
        assert!(has_codewiki_mcp_server(&with_entry));
    }
}
