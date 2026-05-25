// T-433 — Claude Code target.
// Writes: ~/.claude.json (global MCP) | ./.mcp.json (local MCP)
//         ~/.claude/settings.json    (permissions, if autoAllow)
//         ~/.claude/CLAUDE.md        (instructions marker block)

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use codewiki_mcp::constants::{MCP_BINARY_NAME, MCP_SERVE_ARGS_GLOBAL};

use crate::installer::shared::{
    atomic_write, instructions_template, json_deep_equal, read_json_file, remove_marked_section,
    replace_or_append_marked_section, write_json_file, FileAction, RemoveAction, SectionAction,
    WriteResult, CODEWIKI_PERMISSIONS, CODEWIKI_SECTION_END, CODEWIKI_SECTION_START,
};

use super::{AgentTarget, DetectionResult, InstallOptions, Location};

// ── Path helpers ─────────────────────────────────────────────────────────────

fn config_dir(loc: Location) -> PathBuf {
    match loc {
        Location::Global => home_dir().join(".claude"),
        Location::Local => std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".claude"),
    }
}

fn mcp_json_path(loc: Location) -> PathBuf {
    match loc {
        Location::Global => home_dir().join(".claude.json"),
        Location::Local => std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".mcp.json"),
    }
}

/// Legacy local path (pre-issue-207). Claude Code never reads this file;
/// we migrate entries out of it on install and strip on uninstall.
fn legacy_local_mcp_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".claude.json")
}

fn settings_json_path(loc: Location) -> PathBuf {
    config_dir(loc).join("settings.json")
}

fn instructions_path(loc: Location) -> PathBuf {
    config_dir(loc).join("CLAUDE.md")
}

fn home_dir() -> PathBuf {
    home::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

// ── MCP entry ────────────────────────────────────────────────────────────────

fn mcp_entry() -> Value {
    let args: Vec<Value> = MCP_SERVE_ARGS_GLOBAL
        .iter()
        .map(|s| Value::String(s.to_string()))
        .collect();
    json!({
        "type": "stdio",
        "command": MCP_BINARY_NAME,
        "args": args
    })
}

// ── Per-file writers ─────────────────────────────────────────────────────────

#[tracing::instrument(level = "debug")]
pub fn write_mcp_entry(loc: Location) -> Result<(PathBuf, FileAction)> {
    let file = mcp_json_path(loc);
    let mut existing = read_json_file(&file);
    let before = existing.pointer("/mcpServers/codewiki").cloned();
    let after = mcp_entry();

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

    ensure_mcp_servers_codewiki(&mut existing, after);
    write_json_file(&file, &existing)?;
    Ok((file, action))
}

fn ensure_mcp_servers_codewiki(obj: &mut Value, entry: Value) {
    if obj.get("mcpServers").is_none() {
        obj["mcpServers"] = json!({});
    }
    obj["mcpServers"]["codewiki"] = entry;
}

/// Remove the codewiki entry from a legacy `./.claude.json`.
fn cleanup_legacy_local_mcp() -> Option<(PathBuf, FileAction)> {
    let file = legacy_local_mcp_path();
    if !file.exists() {
        return None;
    }
    let mut config = read_json_file(&file);
    config.pointer("/mcpServers/codewiki")?;
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
    if config.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        let _ = fs::remove_file(&file);
    } else {
        let _ = write_json_file(&file, &config);
    }
    Some((file, FileAction::Removed))
}

#[tracing::instrument(level = "debug")]
pub fn write_permissions_entry(loc: Location) -> Result<(PathBuf, FileAction)> {
    let file = settings_json_path(loc);
    let mut settings = read_json_file(&file);
    let created = !file.exists();

    // Ensure path exists: settings.permissions.allow = []
    if settings.get("permissions").is_none() {
        settings["permissions"] = json!({});
    }
    if settings["permissions"].get("allow").is_none() {
        settings["permissions"]["allow"] = json!([]);
    }

    let allow = settings["permissions"]["allow"]
        .as_array_mut()
        .expect("just ensured this is an array");

    let before_len = allow.len();
    for perm in CODEWIKI_PERMISSIONS {
        let perm_val = Value::String(perm.to_string());
        if !allow.contains(&perm_val) {
            allow.push(perm_val);
        }
    }

    if allow.len() == before_len && !created {
        return Ok((file, FileAction::Unchanged));
    }

    write_json_file(&file, &settings)?;
    Ok((file, if created { FileAction::Created } else { FileAction::Updated }))
}

#[tracing::instrument(level = "debug")]
pub fn write_instructions_entry(loc: Location) -> Result<(PathBuf, FileAction)> {
    let file = instructions_path(loc);
    let dir = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;

    // Legacy migration: an unmarked `## CodeWiki` section from old installs.
    if file.exists() {
        let content = fs::read_to_string(&file).unwrap_or_default();
        if !content.contains(CODEWIKI_SECTION_START) {
            if let Some(idx) = content.find("\n## CodeWiki\n") {
                let section_start = idx;
                let after = &content[section_start + 1..];
                let next_header = after.find("\n## ").map(|i| i + 1);
                let section_end = next_header
                    .map(|i| section_start + 1 + i)
                    .unwrap_or(content.len());
                let merged = format!(
                    "{}\n{}{}",
                    &content[..section_start],
                    instructions_template(),
                    &content[section_end..]
                );
                atomic_write(&file, &merged)?;
                return Ok((file, FileAction::Updated));
            }
        }
    }

    let action =
        replace_or_append_marked_section(&file, instructions_template(), CODEWIKI_SECTION_START, CODEWIKI_SECTION_END)?;
    let mapped = match action {
        SectionAction::Created => FileAction::Created,
        SectionAction::Unchanged => FileAction::Unchanged,
        _ => FileAction::Updated,
    };
    Ok((file, mapped))
}

// ── Stale hook cleanup ────────────────────────────────────────────────────────

fn is_legacy_codewiki_hook_command(cmd: &Value) -> bool {
    if let Some(s) = cmd.as_str() {
        s.contains("codewiki mark-dirty") || s.contains("codewiki sync-if-dirty")
    } else {
        false
    }
}

fn cleanup_legacy_hooks(loc: Location) -> Result<(PathBuf, FileAction)> {
    let file = settings_json_path(loc);
    if !file.exists() {
        return Ok((file, FileAction::NotFound));
    }

    let mut settings = read_json_file(&file);
    let hooks = match settings.get_mut("hooks") {
        Some(h) if h.is_object() => h,
        _ => return Ok((file, FileAction::Unchanged)),
    };

    let mut removed_any = false;
    if let Some(obj) = hooks.as_object_mut() {
        for groups in obj.values_mut() {
            if let Some(arr) = groups.as_array_mut() {
                for group in arr.iter_mut() {
                    if let Some(hooks_arr) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) {
                        let before = hooks_arr.len();
                        hooks_arr.retain(|h| {
                            !is_legacy_codewiki_hook_command(&h["command"])
                        });
                        if hooks_arr.len() != before {
                            removed_any = true;
                        }
                    }
                }
            }
        }
    }

    if !removed_any {
        return Ok((file, FileAction::Unchanged));
    }

    // Prune empty groups, events, and hooks key.
    if let Some(obj) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        let events: Vec<String> = obj.keys().cloned().collect();
        for event in &events {
            if let Some(groups) = obj.get_mut(event).and_then(|v| v.as_array_mut()) {
                groups.retain(|g| {
                    g.get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|h| !h.is_empty())
                        .unwrap_or(true)
                });
            }
        }
        let empty_events: Vec<String> = obj
            .iter()
            .filter(|(_, v)| v.as_array().map(|a| a.is_empty()).unwrap_or(false))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &empty_events {
            obj.remove(k);
        }
    }
    let hooks_empty = settings
        .get("hooks")
        .and_then(|v| v.as_object())
        .map(|m| m.is_empty())
        .unwrap_or(false);
    if hooks_empty {
        if let Some(obj) = settings.as_object_mut() {
            obj.remove("hooks");
        }
    }

    write_json_file(&file, &settings)?;
    Ok((file, FileAction::Removed))
}

// ── ClaudeTarget impl ─────────────────────────────────────────────────────────

pub struct ClaudeTarget;

impl AgentTarget for ClaudeTarget {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        let mcp_path = mcp_json_path(loc);
        let config = read_json_file(&mcp_path);
        let already_configured = config.pointer("/mcpServers/codewiki").is_some();
        let installed = match loc {
            Location::Global => config_dir(loc).exists() || mcp_path.exists(),
            Location::Local => mcp_path.exists() || config_dir(loc).exists(),
        };
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(mcp_path),
        }
    }

    #[tracing::instrument(level = "debug", skip(self, opts))]
    fn install(&self, loc: Location, opts: &InstallOptions) -> Result<WriteResult> {
        let mut result = WriteResult::new();

        // 1. MCP server entry
        let (p, a) = write_mcp_entry(loc)?;
        result.push(p, a);

        // 1b. Migrate legacy ./.claude.json
        if loc == Location::Local {
            if let Some((p, a)) = cleanup_legacy_local_mcp() {
                result.push(p, a);
            }
        }

        // 2. Permissions
        if opts.auto_allow {
            let (p, a) = write_permissions_entry(loc)?;
            result.push(p, a);
        }

        // 2b. Stale hook cleanup
        let (p, a) = cleanup_legacy_hooks(loc)?;
        if a == FileAction::Removed {
            result.push(p, a);
        }

        // 3. Instructions
        let (p, a) = write_instructions_entry(loc)?;
        result.push(p, a);

        Ok(result)
    }

    #[tracing::instrument(level = "debug", skip(self))]
    fn uninstall(&self, loc: Location) -> Result<WriteResult> {
        let mut result = WriteResult::new();

        // 1. MCP entry
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

        // 1b. Legacy local migration
        if loc == Location::Local {
            if let Some((p, a)) = cleanup_legacy_local_mcp() {
                result.push(p, a);
            }
        }

        // 2. Permissions
        let settings_path = settings_json_path(loc);
        let mut settings = read_json_file(&settings_path);
        let had_perms = settings
            .pointer("/permissions/allow")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .any(|p| p.as_str().map(|s| s.starts_with("mcp__codewiki__")).unwrap_or(false))
            })
            .unwrap_or(false);

        if had_perms {
            if let Some(allow) = settings
                .get_mut("permissions")
                .and_then(|p| p.get_mut("allow"))
                .and_then(|a| a.as_array_mut())
            {
                allow.retain(|p| {
                    !p.as_str()
                        .map(|s| s.starts_with("mcp__codewiki__"))
                        .unwrap_or(false)
                });
            }
            // Prune empty allow/permissions
            let allow_empty = settings
                .pointer("/permissions/allow")
                .and_then(|v| v.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(false);
            if allow_empty {
                if let Some(perms) = settings.get_mut("permissions").and_then(|v| v.as_object_mut()) {
                    perms.remove("allow");
                }
            }
            let perms_empty = settings
                .get("permissions")
                .and_then(|v| v.as_object())
                .map(|m| m.is_empty())
                .unwrap_or(false);
            if perms_empty {
                if let Some(obj) = settings.as_object_mut() {
                    obj.remove("permissions");
                }
            }
            write_json_file(&settings_path, &settings)?;
            result.push(&settings_path, FileAction::Removed);
        } else {
            result.push(&settings_path, FileAction::NotFound);
        }

        // 2b. Stale hooks
        let (p, a) = cleanup_legacy_hooks(loc)?;
        if a == FileAction::Removed {
            result.push(p, a);
        }

        // 3. Instructions
        let instr = instructions_path(loc);
        let action = remove_marked_section(&instr, CODEWIKI_SECTION_START, CODEWIKI_SECTION_END)?;
        let mapped = match action {
            RemoveAction::Removed => FileAction::Removed,
            RemoveAction::NotFound => FileAction::NotFound,
            RemoveAction::Kept => FileAction::NotFound,
        };
        result.push(instr, mapped);

        Ok(result)
    }

    fn print_config(&self, loc: Location) -> String {
        let target = mcp_json_path(loc);
        let snippet = serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": { "codewiki": mcp_entry() }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", target.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        vec![mcp_json_path(loc), settings_json_path(loc), instructions_path(loc)]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn setup_home() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        dir
    }

    #[test]
    fn mcp_args_match_constants() {
        let entry = mcp_entry();
        let args: Vec<&str> = entry["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(args, MCP_SERVE_ARGS_GLOBAL);
    }

    #[test]
    #[serial]
    fn install_global_creates_claude_json() {
        let _home = setup_home();
        let target = ClaudeTarget;
        let opts = InstallOptions { auto_allow: false, project_root: None };
        let result = target.install(Location::Global, &opts).unwrap();
        let mcp_file = mcp_json_path(Location::Global);
        assert!(mcp_file.exists(), ".claude.json should be created");
        let config = read_json_file(&mcp_file);
        let args: Vec<&str> = config["mcpServers"]["codewiki"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(args, MCP_SERVE_ARGS_GLOBAL);
        let _ = result;
    }

    #[test]
    #[serial]
    fn install_idempotent_returns_unchanged() {
        let _home = setup_home();
        let target = ClaudeTarget;
        let opts = InstallOptions { auto_allow: false, project_root: None };
        target.install(Location::Global, &opts).unwrap();
        let result2 = target.install(Location::Global, &opts).unwrap();
        // The MCP entry file should report unchanged on the second run.
        let mcp_action = result2.files.first().map(|f| f.action);
        assert_eq!(mcp_action, Some(FileAction::Unchanged));
    }
}
