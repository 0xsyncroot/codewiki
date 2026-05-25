// T-434 — Cursor target.
// Writes: ~/.cursor/mcp.json (global) | ./.cursor/mcp.json (local)
//         ./.cursor/rules/codewiki.mdc (local only — Cursor rules are project-scoped)

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use codewiki_mcp::constants::{MCP_BINARY_NAME, MCP_SERVE_ARGS_WITH_PATH};

use crate::installer::shared::{
    atomic_write, instructions_template, json_deep_equal, read_json_file,
    replace_or_append_marked_section, write_json_file, FileAction, SectionAction, WriteResult,
    CODEWIKI_SECTION_END, CODEWIKI_SECTION_START,
};

use super::{AgentTarget, DetectionResult, InstallOptions, Location};

// ── Path helpers ─────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    home::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn mcp_json_path(loc: Location) -> PathBuf {
    match loc {
        Location::Global => home_dir().join(".cursor").join("mcp.json"),
        Location::Local => std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".cursor")
            .join("mcp.json"),
    }
}

fn rules_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".cursor")
        .join("rules")
        .join("codewiki.mdc")
}

const MDC_FRONTMATTER: &str = "---\ndescription: CodeWiki MCP usage guide — when to use which tool\nalwaysApply: true\n---\n\n";

// ── MCP entry builder ─────────────────────────────────────────────────────────

/// Build the Cursor MCP entry. Cursor launches MCP servers with the wrong
/// cwd so we always append `--path`:
/// - local: absolute path to cwd (known at install time)
/// - global: `${workspaceFolder}` (Cursor expands per workspace)
fn build_cursor_mcp_config(loc: Location) -> Value {
    let path_arg = match loc {
        Location::Local => std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string()),
        Location::Global => "${workspaceFolder}".to_string(),
    };

    let mut args: Vec<Value> = MCP_SERVE_ARGS_WITH_PATH
        .iter()
        .map(|s| Value::String(s.to_string()))
        .collect();
    args.push(Value::String(path_arg));

    json!({
        "type": "stdio",
        "command": MCP_BINARY_NAME,
        "args": args
    })
}

// ── Per-file writers ──────────────────────────────────────────────────────────

#[tracing::instrument(level = "debug")]
fn write_mcp_entry(loc: Location) -> Result<(PathBuf, FileAction)> {
    let file = mcp_json_path(loc);
    let mut existing = read_json_file(&file);
    let before = existing.pointer("/mcpServers/codewiki").cloned();
    let after = build_cursor_mcp_config(loc);

    if let Some(ref b) = before {
        if json_deep_equal(b, &after) {
            return Ok((file, FileAction::Unchanged));
        }
    }

    let action = if before.is_some() || file.exists() {
        FileAction::Updated
    } else {
        FileAction::Created
    };

    if existing.get("mcpServers").is_none() {
        existing["mcpServers"] = json!({});
    }
    existing["mcpServers"]["codewiki"] = after;
    write_json_file(&file, &existing)?;
    Ok((file, action))
}

#[tracing::instrument(level = "debug")]
fn write_rules_entry() -> Result<(PathBuf, FileAction)> {
    let file = rules_path();
    let dir = file.parent().unwrap();
    fs::create_dir_all(dir)?;

    let body = format!("{}{}", MDC_FRONTMATTER, instructions_template());

    if !file.exists() {
        atomic_write(&file, &format!("{body}\n"))?;
        return Ok((file, FileAction::Created));
    }

    let existing = fs::read_to_string(&file).unwrap_or_default();
    let want_with_nl = format!("{body}\n");
    if existing == want_with_nl {
        return Ok((file, FileAction::Unchanged));
    }

    // Marker-based section swap (preserves user-added content outside markers).
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

fn remove_rules_entry() -> Result<(PathBuf, FileAction)> {
    let file = rules_path();
    if !file.exists() {
        return Ok((file, FileAction::NotFound));
    }

    let content = fs::read_to_string(&file).unwrap_or_default();
    let our_frontmatter = MDC_FRONTMATTER.trim();
    let start_idx = content.find(CODEWIKI_SECTION_START);
    let end_idx = content.find(CODEWIKI_SECTION_END);

    if let (Some(si), Some(ei)) = (start_idx, end_idx) {
        if ei > si {
            let before = content[..si].trim_end();
            let after = content[ei + CODEWIKI_SECTION_END.len()..].trim_start();
            let remainder = if !before.is_empty() && !after.is_empty() {
                format!("{before}\n\n{after}").trim().to_string()
            } else {
                format!("{before}{after}").trim().to_string()
            };
            if remainder.is_empty() || remainder == our_frontmatter {
                let _ = fs::remove_file(&file);
            } else {
                atomic_write(&file, &format!("{remainder}\n"))?;
            }
            return Ok((file, FileAction::Removed));
        }
    }

    // No markers but file is just our frontmatter → remove it.
    if content.trim() == our_frontmatter {
        let _ = fs::remove_file(&file);
        return Ok((file, FileAction::Removed));
    }

    Ok((file, FileAction::NotFound))
}

// ── CursorTarget impl ─────────────────────────────────────────────────────────

pub struct CursorTarget;

impl AgentTarget for CursorTarget {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn display_name(&self) -> &'static str {
        "Cursor"
    }

    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        let mcp_path = mcp_json_path(loc);
        let config = read_json_file(&mcp_path);
        let already_configured = config.pointer("/mcpServers/codewiki").is_some();
        let installed = match loc {
            Location::Global => home_dir().join(".cursor").exists(),
            Location::Local => std::env::current_dir()
                .map(|d| d.join(".cursor").exists())
                .unwrap_or(false),
        };
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(mcp_path),
        }
    }

    #[tracing::instrument(level = "debug", skip(self, _opts))]
    fn install(&self, loc: Location, _opts: &InstallOptions) -> Result<WriteResult> {
        let mut result = WriteResult::new();

        let (p, a) = write_mcp_entry(loc)?;
        result.push(p, a);

        if loc == Location::Local {
            let (p, a) = write_rules_entry()?;
            result.push(p, a);
        }

        result.notes.push("Restart Cursor for MCP changes to take effect.".to_string());
        Ok(result)
    }

    #[tracing::instrument(level = "debug", skip(self))]
    fn uninstall(&self, loc: Location) -> Result<WriteResult> {
        let mut result = WriteResult::new();

        let mcp_path = mcp_json_path(loc);
        let mut config = read_json_file(&mcp_path);
        if config.pointer("/mcpServers/codewiki").is_some() {
            if let Some(servers) = config.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
                servers.remove("codewiki");
            }
            let mcp_empty = config
                .get("mcpServers")
                .and_then(|v| v.as_object())
                .map(|m| m.is_empty())
                .unwrap_or(false);
            if mcp_empty {
                if let Some(obj) = config.as_object_mut() {
                    obj.remove("mcpServers");
                }
            }
            write_json_file(&mcp_path, &config)?;
            result.push(&mcp_path, FileAction::Removed);
        } else {
            result.push(&mcp_path, FileAction::NotFound);
        }

        if loc == Location::Local {
            let (p, a) = remove_rules_entry()?;
            result.push(p, a);
        }

        Ok(result)
    }

    fn print_config(&self, loc: Location) -> String {
        let target = mcp_json_path(loc);
        let snippet = serde_json::to_string_pretty(&json!({
            "mcpServers": { "codewiki": build_cursor_mcp_config(loc) }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", target.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        match loc {
            Location::Local => vec![mcp_json_path(loc), rules_path()],
            Location::Global => vec![mcp_json_path(loc)],
        }
    }

    fn wire_project_surfaces(&self) -> Option<Result<WriteResult>> {
        let mut result = WriteResult::new();
        match write_rules_entry() {
            Ok((p, a)) => {
                result.push(p, a);
                Some(Ok(result))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use codewiki_mcp::constants::MCP_SERVE_ARGS_WITH_PATH;

    #[test]
    fn cursor_args_start_with_with_path_constants() {
        // Global: args = [serve, --mcp, --path, ${workspaceFolder}]
        let entry = build_cursor_mcp_config(Location::Global);
        let args: Vec<&str> = entry["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(&args[..MCP_SERVE_ARGS_WITH_PATH.len()], MCP_SERVE_ARGS_WITH_PATH);
        assert_eq!(args.last().unwrap(), &"${workspaceFolder}");
    }
}
