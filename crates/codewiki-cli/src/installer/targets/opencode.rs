// T-436 — opencode target.
// Writes: ~/.config/opencode/opencode.jsonc (global) | ./opencode.jsonc (local)
//         ~/.config/opencode/AGENTS.md (global) | ./AGENTS.md (local)
//
// The opencode config shape differs from Claude/Cursor:
//   { "mcp": { "codewiki": { "type": "local", "command": [...], "enabled": true } } }
//
// JSONC comment preservation (A6-resolutions §M3): comments (`//` and `/* */`)
// are stripped before parsing and are therefore lost on re-write. Data keys are
// preserved. If the file cannot be parsed even after stripping, the installer
// returns a hard error rather than silently overwriting the file.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use codewiki_mcp::constants::MCP_BINARY_NAME;
use codewiki_mcp::constants::MCP_SERVE_ARGS_GLOBAL;

use crate::installer::shared::{
    instructions_template, json_deep_equal, remove_marked_section,
    replace_or_append_marked_section, write_json_file, FileAction, RemoveAction, SectionAction,
    WriteResult, CODEWIKI_SECTION_END, CODEWIKI_SECTION_START,
};

use super::{AgentTarget, DetectionResult, InstallOptions, Location};

// ── Path helpers ──────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    home::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn global_config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let app_data = std::env::var("APPDATA").unwrap_or_else(|_| {
            home_dir()
                .join("AppData")
                .join("Roaming")
                .to_string_lossy()
                .to_string()
        });
        PathBuf::from(app_data).join("opencode")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let xdg = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"));
        xdg.join("opencode")
    }
}

fn config_base_dir(loc: Location) -> PathBuf {
    match loc {
        Location::Global => global_config_dir(),
        Location::Local => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// Pick existing .jsonc, then .json; default to .jsonc for new files.
fn config_path(loc: Location) -> PathBuf {
    let dir = config_base_dir(loc);
    let jsonc = dir.join("opencode.jsonc");
    let json = dir.join("opencode.json");
    if jsonc.exists() {
        return jsonc;
    }
    if json.exists() {
        return json;
    }
    jsonc
}

fn instructions_path(loc: Location) -> PathBuf {
    config_base_dir(loc).join("AGENTS.md")
}

// ── Config entry ──────────────────────────────────────────────────────────────

fn opencode_server_entry() -> Value {
    let mut cmd = vec![Value::String(MCP_BINARY_NAME.to_string())];
    cmd.extend(
        MCP_SERVE_ARGS_GLOBAL
            .iter()
            .map(|s| Value::String(s.to_string())),
    );
    json!({
        "type": "local",
        "command": cmd,
        "enabled": true
    })
}

// ── Per-file writers ──────────────────────────────────────────────────────────

#[tracing::instrument(level = "debug")]
fn write_mcp_entry(loc: Location) -> Result<(PathBuf, FileAction)> {
    let file = config_path(loc);
    let dir = config_base_dir(loc);
    fs::create_dir_all(&dir)?;

    let existed = file.exists();
    let mut obj: Value = if existed {
        let text = fs::read_to_string(&file).unwrap_or_default();
        let stripped = strip_jsonc_comments(&text);
        serde_json::from_str(&stripped).map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse {}: existing file is malformed JSONC. \
                 Backup and retry. ({})",
                file.display(),
                e
            )
        })?
    } else {
        // Seed with the $schema key so the result is a valid opencode config.
        json!({ "$schema": "https://opencode.ai/config.json" })
    };

    let before = obj.pointer("/mcp/codewiki").cloned();
    let after = opencode_server_entry();

    if let Some(ref b) = before {
        if json_deep_equal(b, &after) {
            return Ok((file, FileAction::Unchanged));
        }
    }

    // Ensure $schema is present.
    if obj.get("$schema").is_none() {
        if let Some(m) = obj.as_object_mut() {
            m.insert(
                "$schema".to_string(),
                Value::String("https://opencode.ai/config.json".to_string()),
            );
        }
    }

    // Inject the codewiki entry.
    if obj.get("mcp").is_none() {
        obj["mcp"] = json!({});
    }
    obj["mcp"]["codewiki"] = after;

    write_json_file(&file, &obj)?;
    Ok((file, if existed { FileAction::Updated } else { FileAction::Created }))
}

/// Strip both `//` line comments and `/* */` block comments from JSONC text.
///
/// Handles:
/// - `//` comments (terminated by newline)
/// - `/* */` block comments (may span multiple lines)
/// - String literals containing `//` or `/*` are left intact
///
/// Comments are removed; all other content (including trailing commas,
/// which opencode permits) is passed through unchanged.
///
/// Limitation (A6-resolutions §M3): comments are lost on re-write.
/// Data keys are preserved.
fn strip_jsonc_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    let mut escape = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escape {
                escape = false;
                continue;
            }
            if c == '\\' {
                escape = true;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                // Line comment: consume until newline (newline itself is kept
                // so line numbers stay consistent).
                while let Some(&ch) = chars.peek() {
                    if ch == '\n' {
                        break;
                    }
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next(); // consume `*`
                while let Some(ch) = chars.next() {
                    if ch == '*' && chars.peek() == Some(&'/') {
                        chars.next(); // consume closing `/`
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[tracing::instrument(level = "debug")]
fn write_instructions_entry(loc: Location) -> Result<(PathBuf, FileAction)> {
    let file = instructions_path(loc);
    let fallback_dir = PathBuf::from(".");
    let dir = file.parent().unwrap_or(&fallback_dir);
    fs::create_dir_all(dir)?;

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

// ── OpencodeTarget impl ───────────────────────────────────────────────────────

pub struct OpencodeTarget;

impl AgentTarget for OpencodeTarget {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "opencode"
    }

    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        let file = config_path(loc);
        let already_configured = if file.exists() {
            let text = fs::read_to_string(&file).unwrap_or_default();
            let stripped = strip_jsonc_comments(&text);
            serde_json::from_str::<Value>(&stripped)
                .map(|v| v.pointer("/mcp/codewiki").is_some())
                .unwrap_or(false)
        } else {
            false
        };
        let installed = match loc {
            Location::Global => global_config_dir().exists(),
            Location::Local => file.exists(),
        };
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(file),
        }
    }

    #[tracing::instrument(level = "debug", skip(self, _opts))]
    fn install(&self, loc: Location, _opts: &InstallOptions) -> Result<WriteResult> {
        let mut result = WriteResult::new();
        let (p, a) = write_mcp_entry(loc)?;
        result.push(p, a);
        let (p, a) = write_instructions_entry(loc)?;
        result.push(p, a);
        Ok(result)
    }

    #[tracing::instrument(level = "debug", skip(self))]
    fn uninstall(&self, loc: Location) -> Result<WriteResult> {
        let mut result = WriteResult::new();
        let file = config_path(loc);

        if !file.exists() {
            result.push(&file, FileAction::NotFound);
        } else {
            let text = fs::read_to_string(&file).unwrap_or_default();
            let stripped = strip_jsonc_comments(&text);
            let mut obj: Value = serde_json::from_str(&stripped).map_err(|e| {
                anyhow::anyhow!(
                    "Cannot parse {}: existing file is malformed JSONC. \
                     Backup and retry. ({})",
                    file.display(),
                    e
                )
            })?;

            if obj.pointer("/mcp/codewiki").is_none() {
                result.push(&file, FileAction::NotFound);
            } else {
                // Remove the codewiki key.
                if let Some(mcp) = obj.get_mut("mcp").and_then(|v| v.as_object_mut()) {
                    mcp.remove("codewiki");
                }
                // Remove `mcp` wrapper if empty.
                let mcp_empty = obj
                    .get("mcp")
                    .and_then(|v| v.as_object())
                    .map(|m| m.is_empty())
                    .unwrap_or(false);
                if mcp_empty {
                    if let Some(o) = obj.as_object_mut() {
                        o.remove("mcp");
                    }
                }
                write_json_file(&file, &obj)?;
                result.push(&file, FileAction::Removed);
            }
        }

        let instr = instructions_path(loc);
        let action = remove_marked_section(&instr, CODEWIKI_SECTION_START, CODEWIKI_SECTION_END)?;
        let mapped = match action {
            RemoveAction::Removed => FileAction::Removed,
            _ => FileAction::NotFound,
        };
        result.push(instr, mapped);

        Ok(result)
    }

    fn print_config(&self, loc: Location) -> String {
        let target = config_path(loc);
        let snippet = serde_json::to_string_pretty(&json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": { "codewiki": opencode_server_entry() }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", target.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        vec![config_path(loc), instructions_path(loc)]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use codewiki_mcp::constants::MCP_SERVE_ARGS_GLOBAL;
    use serial_test::serial;
    use tempfile::TempDir;

    // ── Helper ────────────────────────────────────────────────────────────────

    fn setup_home() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join(".config").to_str().unwrap());
        dir
    }

    // ── Unit: opencode_server_entry ───────────────────────────────────────────

    #[test]
    fn opencode_command_starts_with_binary() {
        let entry = opencode_server_entry();
        let cmd = entry["command"].as_array().unwrap();
        assert_eq!(cmd[0].as_str().unwrap(), MCP_BINARY_NAME);
        // Remaining elements should match MCP_SERVE_ARGS_GLOBAL.
        let rest: Vec<&str> = cmd[1..].iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(rest, MCP_SERVE_ARGS_GLOBAL);
    }

    // ── Unit: strip_jsonc_comments ────────────────────────────────────────────

    #[test]
    fn strip_handles_line_comment() {
        let input = "{ \"key\": \"value\" // comment\n}";
        let stripped = strip_jsonc_comments(input);
        assert!(!stripped.contains("// comment"), "line comment should be removed");
        assert!(stripped.contains("\"value\""), "value should be preserved");
        // The newline that terminates the comment must still be present so JSON
        // structure is intact.
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["key"].as_str().unwrap(), "value");
    }

    #[test]
    fn strip_handles_block_comment_single_line() {
        let input = "{ \"key\": /* inline */ \"value\" }";
        let stripped = strip_jsonc_comments(input);
        assert!(!stripped.contains("inline"), "block comment should be removed");
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["key"].as_str().unwrap(), "value");
    }

    #[test]
    fn strip_handles_block_comment_multi_line() {
        let input = "{\n  /* this is\n     a multi-line\n     comment */\n  \"key\": \"value\"\n}";
        let stripped = strip_jsonc_comments(input);
        assert!(!stripped.contains("multi-line"), "block comment content should be removed");
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["key"].as_str().unwrap(), "value");
    }

    #[test]
    fn strip_preserves_strings_containing_slash_slash() {
        let input = r#"{ "url": "https://example.com" }"#;
        let stripped = strip_jsonc_comments(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["url"].as_str().unwrap(), "https://example.com");
    }

    #[test]
    fn strip_preserves_strings_containing_block_comment_markers() {
        let input = r#"{ "pattern": "/* not a comment */" }"#;
        let stripped = strip_jsonc_comments(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["pattern"].as_str().unwrap(), "/* not a comment */");
    }

    // ── Integration: block comment in existing opencode.jsonc ─────────────────

    #[test]
    #[serial]
    fn opencode_preserves_block_comments_in_existing_file() {
        let _home = setup_home();
        let target = OpencodeTarget;
        let opts = InstallOptions { auto_allow: false, project_root: None };

        // Write an opencode.jsonc that has a $schema, a block comment, and a
        // sibling key that must survive the round-trip.
        let config_dir = global_config_dir();
        std::fs::create_dir_all(&config_dir).unwrap();
        let jsonc_path = config_dir.join("opencode.jsonc");
        std::fs::write(
            &jsonc_path,
            r#"{
  "$schema": "https://opencode.ai/config.json",
  /* this block comment describes the sibling key */
  "theme": "dark"
}
"#,
        )
        .unwrap();

        // Install should succeed.
        let result = target.install(Location::Global, &opts).unwrap();
        assert!(result.files.iter().any(|f| f.action == FileAction::Updated || f.action == FileAction::Created));

        // Read back and verify data survival.
        let written = std::fs::read_to_string(&jsonc_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();

        // The codewiki MCP entry must be present.
        assert!(
            parsed.pointer("/mcp/codewiki").is_some(),
            "codewiki entry should be injected"
        );
        // $schema must survive.
        assert_eq!(
            parsed["$schema"].as_str().unwrap(),
            "https://opencode.ai/config.json",
            "$schema key must be preserved"
        );
        // Sibling key must survive.
        assert_eq!(
            parsed["theme"].as_str().unwrap(),
            "dark",
            "sibling key 'theme' must be preserved after round-trip"
        );
    }
}
