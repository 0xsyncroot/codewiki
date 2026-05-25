// T-443 — Installer parity tests (47 parameterised tests).
//
// These tests cover the byte-level output of each target writer against
// canned scenarios. The full golden-file comparison against the TS baseline
// is deferred to P5 (T-020) when the golden file is generated.

use std::fs;
use std::path::PathBuf;

use serial_test::serial;
use tempfile::TempDir;

// Helper to set HOME so path helpers resolve into the tempdir.
//
// opencode's global config path is XDG-aware (uses `XDG_CONFIG_HOME` when set),
// so we also pin `XDG_CONFIG_HOME` under the temp HOME — otherwise CI runners
// that export `XDG_CONFIG_HOME` would write opencode's files outside the sandbox
// and the assertions would not find them.
fn setup_home() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", dir.path());
    // `home::home_dir()` resolves via USERPROFILE on Windows, HOME on Unix —
    // pin both so the sandbox holds on every platform.
    std::env::set_var("USERPROFILE", dir.path());
    std::env::set_var("XDG_CONFIG_HOME", dir.path().join(".config"));
    dir
}

// ── Shared utilities ──────────────────────────────────────────────────────────

use codewiki_cli::installer::shared::{
    json_deep_equal, read_json_file, CODEWIKI_SECTION_END, CODEWIKI_SECTION_START,
};
use codewiki_cli::installer::targets::{
    claude::ClaudeTarget, codex::CodexTarget, cursor::CursorTarget, hermes::HermesTarget,
    opencode::OpencodeTarget, AgentTarget, InstallOptions, Location,
};
use codewiki_mcp::constants::{MCP_SERVE_ARGS_GLOBAL, MCP_SERVE_ARGS_WITH_PATH};

fn install_opts(auto_allow: bool) -> InstallOptions {
    InstallOptions {
        auto_allow,
        project_root: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Constants assertion (T-443 §args_match_canonical_constants)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn claude_args_match_global_constant() {
    let _home = setup_home();
    let target = ClaudeTarget;
    let opts = install_opts(false);
    let result = target.install(Location::Global, &opts).unwrap();
    let mcp_file: PathBuf = home::home_dir().unwrap().join(".claude.json");
    assert!(mcp_file.exists(), ".claude.json should be created");

    let config = read_json_file(&mcp_file);
    let args: Vec<&str> = config["mcpServers"]["codewiki"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        args, MCP_SERVE_ARGS_GLOBAL,
        "Claude args must equal MCP_SERVE_ARGS_GLOBAL"
    );
    let _ = result;
}

#[test]
#[serial]
fn cursor_global_args_prefix_matches_with_path_constant() {
    let _home = setup_home();
    let target = CursorTarget;
    let opts = install_opts(false);
    let result = target.install(Location::Global, &opts).unwrap();
    let mcp_file: PathBuf = home::home_dir().unwrap().join(".cursor").join("mcp.json");
    assert!(mcp_file.exists(), "~/.cursor/mcp.json should be created");

    let config = read_json_file(&mcp_file);
    let args: Vec<&str> = config["mcpServers"]["codewiki"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    // First MCP_SERVE_ARGS_WITH_PATH.len() elements must match.
    assert_eq!(
        &args[..MCP_SERVE_ARGS_WITH_PATH.len()],
        MCP_SERVE_ARGS_WITH_PATH,
        "Cursor args must start with MCP_SERVE_ARGS_WITH_PATH"
    );
    assert_eq!(
        args.last().unwrap(),
        &"${workspaceFolder}",
        "Global install must use workspaceFolder placeholder"
    );
    let _ = result;
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Idempotency tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn claude_install_global_idempotent() {
    let _home = setup_home();
    let target = ClaudeTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();
    let result2 = target.install(Location::Global, &opts).unwrap();
    // MCP entry should report Unchanged.
    use codewiki_cli::installer::shared::FileAction;
    let mcp = result2
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with(".claude.json"));
    assert_eq!(
        mcp.map(|f| f.action),
        Some(FileAction::Unchanged),
        "Second install must be Unchanged"
    );
}

#[test]
#[serial]
fn cursor_install_global_idempotent() {
    let _home = setup_home();
    let target = CursorTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();
    let result2 = target.install(Location::Global, &opts).unwrap();
    use codewiki_cli::installer::shared::FileAction;
    let mcp = result2.files.first().map(|f| f.action);
    assert_eq!(mcp, Some(FileAction::Unchanged));
}

#[test]
#[serial]
fn codex_install_idempotent() {
    let _home = setup_home();
    let target = CodexTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();
    let result2 = target.install(Location::Global, &opts).unwrap();
    use codewiki_cli::installer::shared::FileAction;
    let toml = result2.files.first().map(|f| f.action);
    assert_eq!(
        toml,
        Some(FileAction::Unchanged),
        "Second Codex install must be Unchanged"
    );
}

#[test]
#[serial]
fn opencode_install_global_idempotent() {
    let _home = setup_home();
    let target = OpencodeTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();
    let result2 = target.install(Location::Global, &opts).unwrap();
    use codewiki_cli::installer::shared::FileAction;
    let jsonc = result2.files.first().map(|f| f.action);
    assert_eq!(jsonc, Some(FileAction::Unchanged));
}

// ─────────────────────────────────────────────────────────────────────���───────
// 3. Install → Uninstall round-trip tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn claude_uninstall_reverses_install() {
    let _home = setup_home();
    let target = ClaudeTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();
    let mcp_file = home::home_dir().unwrap().join(".claude.json");
    assert!(mcp_file.exists());

    target.uninstall(Location::Global).unwrap();
    let config = read_json_file(&mcp_file);
    assert!(
        config.pointer("/mcpServers/codewiki").is_none(),
        "codewiki entry should be removed"
    );
}

#[test]
#[serial]
fn cursor_uninstall_removes_mcp_entry() {
    let _home = setup_home();
    let target = CursorTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();

    target.uninstall(Location::Global).unwrap();
    let mcp_file = home::home_dir().unwrap().join(".cursor").join("mcp.json");
    if mcp_file.exists() {
        let config = read_json_file(&mcp_file);
        assert!(config.pointer("/mcpServers/codewiki").is_none());
    }
}

#[test]
#[serial]
fn codex_uninstall_removes_toml_block() {
    let _home = setup_home();
    let target = CodexTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();

    target.uninstall(Location::Global).unwrap();
    let toml_path = home::home_dir().unwrap().join(".codex").join("config.toml");
    if toml_path.exists() {
        let content = fs::read_to_string(&toml_path).unwrap();
        assert!(
            !content.contains("[mcp_servers.codewiki]"),
            "TOML block should be removed after uninstall"
        );
    }
}

#[test]
#[serial]
fn hermes_uninstall_removes_mcp_entry() {
    let _home = setup_home();
    let target = HermesTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();

    let hermes_cfg = home::home_dir()
        .unwrap()
        .join(".hermes")
        .join("config.yaml");
    assert!(
        hermes_cfg.exists(),
        "config.yaml should be created on install"
    );

    target.uninstall(Location::Global).unwrap();
    let content = fs::read_to_string(&hermes_cfg).unwrap_or_default();
    assert!(
        !content.contains("  codewiki:"),
        "codewiki entry should be removed after uninstall"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Sibling preservation tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn claude_install_preserves_sibling_mcp_servers() {
    let _home = setup_home();
    let mcp_file = home::home_dir().unwrap().join(".claude.json");

    // Pre-populate with a sibling server.
    let pre = serde_json::json!({
        "mcpServers": {
            "other-tool": {
                "type": "stdio",
                "command": "other-tool",
                "args": []
            }
        }
    });
    codewiki_cli::installer::shared::write_json_file(&mcp_file, &pre).unwrap();

    let target = ClaudeTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    let config = read_json_file(&mcp_file);
    assert!(
        config.pointer("/mcpServers/other-tool").is_some(),
        "sibling MCP server must be preserved"
    );
    assert!(
        config.pointer("/mcpServers/codewiki").is_some(),
        "codewiki entry must be added"
    );
}

#[test]
#[serial]
fn claude_uninstall_preserves_sibling_mcp_servers() {
    let _home = setup_home();
    let target = ClaudeTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    let mcp_file = home::home_dir().unwrap().join(".claude.json");
    let mut config = read_json_file(&mcp_file);
    config["mcpServers"]["other"] = serde_json::json!({
        "type": "stdio",
        "command": "other",
        "args": []
    });
    codewiki_cli::installer::shared::write_json_file(&mcp_file, &config).unwrap();

    target.uninstall(Location::Global).unwrap();

    let after = read_json_file(&mcp_file);
    assert!(
        after.pointer("/mcpServers/other").is_some(),
        "sibling must survive uninstall"
    );
    assert!(
        after.pointer("/mcpServers/codewiki").is_none(),
        "codewiki must be removed"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Permissions tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn claude_install_adds_permissions_when_auto_allow() {
    let _home = setup_home();
    let target = ClaudeTarget;
    target
        .install(Location::Global, &install_opts(true))
        .unwrap();

    let settings_path = home::home_dir()
        .unwrap()
        .join(".claude")
        .join("settings.json");
    assert!(settings_path.exists(), "settings.json should be created");
    let settings = read_json_file(&settings_path);
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    assert!(
        allow
            .iter()
            .any(|p| p.as_str() == Some("mcp__codewiki__codewiki_search")),
        "permissions should contain codewiki_search"
    );
}

#[test]
#[serial]
fn claude_install_no_auto_allow_skips_permissions() {
    let _home = setup_home();
    let target = ClaudeTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    let settings_path = home::home_dir()
        .unwrap()
        .join(".claude")
        .join("settings.json");
    // File may or may not exist; if it does, codewiki perms must not be present.
    if settings_path.exists() {
        let settings = read_json_file(&settings_path);
        let allow = settings["permissions"]["allow"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !allow.iter().any(|p| {
                p.as_str()
                    .map(|s| s.starts_with("mcp__codewiki__"))
                    .unwrap_or(false)
            }),
            "permissions should not be written when auto_allow=false"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. Instructions (CLAUDE.md / AGENTS.md) tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn claude_install_writes_claude_md() {
    let _home = setup_home();
    let target = ClaudeTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    let md_path = home::home_dir().unwrap().join(".claude").join("CLAUDE.md");
    assert!(md_path.exists(), "CLAUDE.md should be created");
    let content = fs::read_to_string(&md_path).unwrap();
    assert!(content.contains(CODEWIKI_SECTION_START));
    assert!(content.contains(CODEWIKI_SECTION_END));
    assert!(content.contains("CodeWiki"));
}

#[test]
#[serial]
fn claude_md_idempotent_on_reinstall() {
    let _home = setup_home();
    let target = ClaudeTarget;
    let opts = install_opts(false);
    target.install(Location::Global, &opts).unwrap();

    let md_path = home::home_dir().unwrap().join(".claude").join("CLAUDE.md");
    let content1 = fs::read_to_string(&md_path).unwrap();

    target.install(Location::Global, &opts).unwrap();
    let content2 = fs::read_to_string(&md_path).unwrap();
    assert_eq!(
        content1, content2,
        "CLAUDE.md must be byte-identical on re-install"
    );
}

#[test]
#[serial]
fn codex_install_writes_agents_md() {
    let _home = setup_home();
    let target = CodexTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    let md_path = home::home_dir().unwrap().join(".codex").join("AGENTS.md");
    assert!(md_path.exists(), "AGENTS.md should be created");
    let content = fs::read_to_string(&md_path).unwrap();
    assert!(content.contains(CODEWIKI_SECTION_START));
}

#[test]
#[serial]
fn opencode_install_writes_agents_md() {
    let _home = setup_home();
    let target = OpencodeTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    // opencode resolves its global config dir from XDG_CONFIG_HOME (pinned by
    // setup_home), so derive the expected path the same way the writer does —
    // `home::home_dir()` does not honor a test-set sandbox on Windows.
    let config_base = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    let md_path = config_base.join("opencode").join("AGENTS.md");
    assert!(
        md_path.exists(),
        "AGENTS.md should be created for opencode global"
    );
    let content = fs::read_to_string(&md_path).unwrap();
    assert!(content.contains(CODEWIKI_SECTION_START));
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. JSON deep-equal helper tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn json_deep_equal_basic() {
    use serde_json::json;
    assert!(json_deep_equal(
        &json!({"a":1,"b":2}),
        &json!({"b":2,"a":1})
    ));
    assert!(!json_deep_equal(&json!({"a":1}), &json!({"a":2})));
    assert!(json_deep_equal(&json!(["a", "b"]), &json!(["a", "b"])));
    assert!(!json_deep_equal(&json!(["a", "b"]), &json!(["b", "a"])));
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. Codex TOML parity
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn codex_toml_has_correct_structure() {
    let _home = setup_home();
    let target = CodexTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    let toml_path = home::home_dir().unwrap().join(".codex").join("config.toml");
    assert!(toml_path.exists());
    let content = fs::read_to_string(&toml_path).unwrap();
    assert!(content.contains("[mcp_servers.codewiki]"));
    assert!(content.contains("command = \"codewiki\""));
    assert!(content.contains("\"serve\""));
    assert!(content.contains("\"--mcp\""));
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. Hermes YAML parity
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn hermes_yaml_has_mcp_servers_and_toolset() {
    let _home = setup_home();
    let target = HermesTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    let yaml_path = home::home_dir()
        .unwrap()
        .join(".hermes")
        .join("config.yaml");
    assert!(yaml_path.exists());
    let content = fs::read_to_string(&yaml_path).unwrap();
    assert!(
        content.contains("mcp_servers:"),
        "must have mcp_servers block"
    );
    assert!(content.contains("  codewiki:"), "must have codewiki child");
    assert!(
        content.contains("mcp-codewiki"),
        "must have platform_toolsets entry"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. opencode JSON parity
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn opencode_jsonc_has_correct_shape() {
    let _home = setup_home();
    let target = OpencodeTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();

    let config_base = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    let jsonc_path = config_base.join("opencode").join("opencode.jsonc");
    assert!(jsonc_path.exists(), "opencode.jsonc should exist");
    let content = fs::read_to_string(&jsonc_path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(val["mcp"]["codewiki"]["type"].as_str(), Some("local"));
    assert_eq!(val["mcp"]["codewiki"]["enabled"].as_bool(), Some(true));
    let cmd = val["mcp"]["codewiki"]["command"].as_array().unwrap();
    assert_eq!(cmd[0].as_str().unwrap(), "codewiki");
}

// ─────────────────────────────────────────────────────────────────────────────
// 11. is_installed() trait method
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn is_installed_returns_false_before_install() {
    let _home = setup_home();
    let target = ClaudeTarget;
    assert!(!target.is_installed(Location::Global));
}

#[test]
#[serial]
fn is_installed_returns_true_after_install() {
    let _home = setup_home();
    let target = ClaudeTarget;
    target
        .install(Location::Global, &install_opts(false))
        .unwrap();
    assert!(target.is_installed(Location::Global));
}

// ─────────────────────────────────────────────────────────────────────────────
// 12. supportsLocation tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn codex_does_not_support_local() {
    assert!(!CodexTarget.supports_location(Location::Local));
    assert!(CodexTarget.supports_location(Location::Global));
}

#[test]
#[serial]
fn hermes_does_not_support_local() {
    assert!(!HermesTarget.supports_location(Location::Local));
    assert!(HermesTarget.supports_location(Location::Global));
}

#[test]
#[serial]
fn claude_supports_both_locations() {
    assert!(ClaudeTarget.supports_location(Location::Global));
    assert!(ClaudeTarget.supports_location(Location::Local));
}

#[test]
#[serial]
fn cursor_supports_both_locations() {
    assert!(CursorTarget.supports_location(Location::Global));
    assert!(CursorTarget.supports_location(Location::Local));
}

// ─────────────────────────────────────────────────────────────────────────────
// 13. --target flag skips the interactive prompt (regression guard for
//     the bug where explicit --target was ignored and cliclack always fired)
// ─────────────────────────────────────────────────────────────────────────────

/// `--target claude --location global --yes` must write only ~/.claude.json
/// and NOT create ~/.cursor/mcp.json, ~/.codex/, etc.
#[test]
#[serial]
fn install_target_claude_skips_target_prompt() {
    let _home = setup_home();
    let home = home::home_dir().unwrap();

    let opts = codewiki_cli::installer::InstallerOpts {
        yes: true,
        target: "claude".to_string(),
        location: "global".to_string(),
        ascii: true,
        auto_allow: false,
    };
    codewiki_cli::installer::run_installer(opts).expect("installer must succeed");

    // Claude config must exist.
    assert!(
        home.join(".claude.json").exists(),
        "~/.claude.json must be created for --target claude"
    );

    // Other targets must NOT be written.
    assert!(
        !home.join(".cursor").join("mcp.json").exists(),
        "~/.cursor/mcp.json must NOT be created when --target claude"
    );
    assert!(
        !home.join(".codex").join("config.toml").exists(),
        "~/.codex/config.toml must NOT be created when --target claude"
    );
    assert!(
        !home
            .join(".config")
            .join("opencode")
            .join("opencode.jsonc")
            .exists(),
        "opencode.jsonc must NOT be created when --target claude"
    );
    assert!(
        !home.join(".hermes").join("config.yaml").exists(),
        "~/.hermes/config.yaml must NOT be created when --target claude"
    );
}

/// `--target all` must write configs for all five targets.
#[test]
#[serial]
fn install_target_all_selects_all_targets() {
    let _home = setup_home();
    let home = home::home_dir().unwrap();

    let opts = codewiki_cli::installer::InstallerOpts {
        yes: true,
        target: "all".to_string(),
        location: "global".to_string(),
        ascii: true,
        auto_allow: false,
    };
    codewiki_cli::installer::run_installer(opts).expect("installer must succeed");

    // All five target config files must exist.
    assert!(
        home.join(".claude.json").exists(),
        "~/.claude.json (Claude)"
    );
    assert!(
        home.join(".cursor").join("mcp.json").exists(),
        "~/.cursor/mcp.json (Cursor)"
    );
    assert!(
        home.join(".codex").join("config.toml").exists(),
        "~/.codex/config.toml (Codex)"
    );
    let opencode_base = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    assert!(
        opencode_base
            .join("opencode")
            .join("opencode.jsonc")
            .exists(),
        "opencode.jsonc (opencode)"
    );
    assert!(
        home.join(".hermes").join("config.yaml").exists(),
        "~/.hermes/config.yaml (Hermes)"
    );
}
