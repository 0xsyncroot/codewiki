// T-432 — AgentTarget trait and location types.

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod hermes;
pub mod opencode;

use crate::installer::shared::WriteResult;
use anyhow::Result;

/// Install location: user-wide (global) or project-specific (local).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Global,
    Local,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Location::Global => write!(f, "global"),
            Location::Local => write!(f, "local"),
        }
    }
}

impl std::str::FromStr for Location {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "global" => Ok(Location::Global),
            "local" => Ok(Location::Local),
            _ => Err(anyhow::anyhow!(
                "unknown location '{}'; use global or local",
                s
            )),
        }
    }
}

/// Options passed to `install()`.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Auto-allow MCP tools in the agent's permission system (Claude only).
    pub auto_allow: bool,
    /// Project root path for local installs.
    #[allow(dead_code)]
    pub project_root: Option<std::path::PathBuf>,
}

/// Detection result for a target at a given location.
#[derive(Debug, Clone)]
pub struct DetectionResult {
    /// Whether the agent appears to be installed on this machine.
    pub installed: bool,
    /// Whether codewiki is already configured for this agent.
    #[allow(dead_code)]
    pub already_configured: bool,
    /// The config file path that was probed.
    #[allow(dead_code)]
    pub config_path: Option<std::path::PathBuf>,
}

/// An AI agent target: knows how to write/remove its MCP config and
/// instruction file.
#[allow(dead_code)]
pub trait AgentTarget: Send + Sync {
    /// Short ID used in `--target` flag: "claude", "cursor", "codex", etc.
    fn id(&self) -> &'static str;
    /// Human-friendly name shown in the installer UI.
    fn display_name(&self) -> &'static str;
    /// Whether this target supports the given location.
    fn supports_location(&self, loc: Location) -> bool;
    /// Probe whether the agent is installed and/or already configured.
    fn detect(&self, loc: Location) -> DetectionResult;
    /// Write all required config files for this target.
    fn install(&self, loc: Location, opts: &InstallOptions) -> Result<WriteResult>;
    /// Remove all config files written by `install`.
    fn uninstall(&self, loc: Location) -> Result<WriteResult>;
    /// Returns `true` if codewiki is already installed for this target.
    fn is_installed(&self, loc: Location) -> bool {
        self.detect(loc).already_configured
    }
    /// Print the config snippet that would be written (dry-run).
    fn print_config(&self, loc: Location) -> String;
    /// List all file paths this target writes.
    fn describe_paths(&self, loc: Location) -> Vec<std::path::PathBuf>;
    /// Optional: write project-local surfaces for a globally-installed target.
    fn wire_project_surfaces(&self) -> Option<Result<WriteResult>> {
        None
    }
}

/// Build the standard list of all five targets.
pub fn all_targets() -> Vec<Box<dyn AgentTarget>> {
    vec![
        Box::new(claude::ClaudeTarget),
        Box::new(cursor::CursorTarget),
        Box::new(codex::CodexTarget),
        Box::new(opencode::OpencodeTarget),
        Box::new(hermes::HermesTarget),
    ]
}
