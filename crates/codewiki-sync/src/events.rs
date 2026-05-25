//! T-322 — Framework config events and `is_framework_config` pattern matching.
//!
//! A5 broadcasts `FrameworkConfigChanged` over a `tokio::sync::broadcast`
//! channel (capacity 16) whenever a recognised framework configuration file
//! is created, modified, or deleted.  A1 subscribes at startup to invalidate
//! its framework-detection cache.

use std::path::{Path, PathBuf};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The kind of filesystem change that triggered the event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsChangeKind {
    Created,
    Modified,
    Deleted,
}

/// Broadcast event emitted when a framework configuration file changes.
#[derive(Clone, Debug)]
pub struct FrameworkConfigChanged {
    /// Absolute path of the config file that changed.
    pub path: PathBuf,
    /// The kind of change that triggered this event.
    pub change_kind: FsChangeKind,
}

// ---------------------------------------------------------------------------
// Channel factory
// ---------------------------------------------------------------------------

/// Create the broadcast channel used for framework config change events.
///
/// The caller (A5 watcher startup) holds the `Sender`.  A1 subscribes by
/// calling `Sender::subscribe()`.
///
/// A capacity of 16 means up to 16 unread events can be buffered before a
/// lagged receiver gets `RecvError::Lagged`.  A1 treats `Lagged` as a full
/// cache invalidation.
pub fn framework_config_channel(
    capacity: usize,
) -> (
    broadcast::Sender<FrameworkConfigChanged>,
    broadcast::Receiver<FrameworkConfigChanged>,
) {
    broadcast::channel(capacity)
}

// ---------------------------------------------------------------------------
// Pattern matching
// ---------------------------------------------------------------------------

/// Returns `true` if `path` is a recognised framework configuration file.
///
/// Matching uses a fast two-step:
/// 1. Check the basename against the exact-name set.
/// 2. Check the extension against `{csproj, vbproj, fsproj}`.
/// 3. Check name prefixes `tsconfig.` and `requirements`.
pub fn is_framework_config(path: &Path) -> bool {
    const EXACT_NAMES: &[&str] = &[
        // Node.js / package managers
        "package.json",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        // TypeScript / JavaScript
        "tsconfig.json",
        "jsconfig.json",
        // Python
        "pyproject.toml",
        "setup.cfg",
        "setup.py",
        // Rust
        "Cargo.toml",
        "Cargo.lock",
        // Go
        "go.mod",
        "go.sum",
        // JVM (Gradle)
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        // JVM (Maven)
        "pom.xml",
        // PHP
        "composer.json",
        "composer.lock",
        // npm/yarn config
        ".npmrc",
        ".yarnrc",
        ".yarnrc.yml",
        // MSBuild shared config
        "Directory.Build.props",
        "Directory.Packages.props",
    ];

    // Extensions that indicate .NET project files.
    const GLOB_EXTENSIONS: &[&str] = &["csproj", "vbproj", "fsproj"];

    // Name prefixes.
    const GLOB_PREFIXES: &[&str] = &["tsconfig.", "requirements"];

    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    // Step 1: exact name match.
    if EXACT_NAMES.contains(&name) {
        return true;
    }

    // Step 2: extension match (e.g. foo.csproj).
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if GLOB_EXTENSIONS.contains(&ext) {
            return true;
        }
    }

    // Step 3: prefix match (e.g. tsconfig.app.json, requirements-dev.txt).
    GLOB_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    // --- Positive matches ---

    #[test]
    fn exact_package_json() {
        assert!(is_framework_config(&p("package.json")));
    }

    #[test]
    fn exact_package_lock_json() {
        assert!(is_framework_config(&p("package-lock.json")));
    }

    #[test]
    fn exact_yarn_lock() {
        assert!(is_framework_config(&p("yarn.lock")));
    }

    #[test]
    fn exact_pnpm_lock() {
        assert!(is_framework_config(&p("pnpm-lock.yaml")));
    }

    #[test]
    fn exact_tsconfig_json() {
        assert!(is_framework_config(&p("tsconfig.json")));
    }

    #[test]
    fn exact_jsconfig_json() {
        assert!(is_framework_config(&p("jsconfig.json")));
    }

    #[test]
    fn exact_pyproject_toml() {
        assert!(is_framework_config(&p("pyproject.toml")));
    }

    #[test]
    fn exact_setup_cfg() {
        assert!(is_framework_config(&p("setup.cfg")));
    }

    #[test]
    fn exact_setup_py() {
        assert!(is_framework_config(&p("setup.py")));
    }

    #[test]
    fn exact_cargo_toml() {
        assert!(is_framework_config(&p("Cargo.toml")));
    }

    #[test]
    fn exact_cargo_lock() {
        assert!(is_framework_config(&p("Cargo.lock")));
    }

    #[test]
    fn exact_go_mod() {
        assert!(is_framework_config(&p("go.mod")));
    }

    #[test]
    fn exact_go_sum() {
        assert!(is_framework_config(&p("go.sum")));
    }

    #[test]
    fn exact_build_gradle() {
        assert!(is_framework_config(&p("build.gradle")));
    }

    #[test]
    fn exact_pom_xml() {
        assert!(is_framework_config(&p("pom.xml")));
    }

    #[test]
    fn exact_composer_json() {
        assert!(is_framework_config(&p("composer.json")));
    }

    #[test]
    fn exact_npmrc() {
        assert!(is_framework_config(&p(".npmrc")));
    }

    #[test]
    fn exact_directory_build_props() {
        assert!(is_framework_config(&p("Directory.Build.props")));
    }

    #[test]
    fn glob_csproj() {
        assert!(is_framework_config(&p("MyApp.csproj")));
        assert!(is_framework_config(&p("/path/to/project/App.csproj")));
    }

    #[test]
    fn glob_vbproj() {
        assert!(is_framework_config(&p("Legacy.vbproj")));
    }

    #[test]
    fn glob_fsproj() {
        assert!(is_framework_config(&p("FSharpLib.fsproj")));
    }

    #[test]
    fn prefix_tsconfig_variant() {
        assert!(is_framework_config(&p("tsconfig.app.json")));
        assert!(is_framework_config(&p("tsconfig.prod.json")));
        assert!(is_framework_config(&p("tsconfig.base.json")));
    }

    #[test]
    fn prefix_requirements_txt() {
        assert!(is_framework_config(&p("requirements.txt")));
        assert!(is_framework_config(&p("requirements-dev.txt")));
        assert!(is_framework_config(&p("requirements_prod.txt")));
    }

    // --- Negative matches ---

    #[test]
    fn rust_source_not_config() {
        assert!(!is_framework_config(&p("main.rs")));
        assert!(!is_framework_config(&p("lib.rs")));
    }

    #[test]
    fn typescript_source_not_config() {
        assert!(!is_framework_config(&p("index.ts")));
        assert!(!is_framework_config(&p("app.tsx")));
    }

    #[test]
    fn arbitrary_json_not_config() {
        assert!(!is_framework_config(&p("config.json")));
        assert!(!is_framework_config(&p("data.json")));
    }

    #[test]
    fn path_in_subdir() {
        // Even in a subdirectory, basename matching works.
        assert!(is_framework_config(&p("/project/sub/Cargo.toml")));
        assert!(!is_framework_config(&p("/project/sub/main.rs")));
    }

    #[test]
    fn empty_path_returns_false() {
        assert!(!is_framework_config(&PathBuf::new()));
    }

    // --- Channel ---

    #[test]
    fn channel_factory_works() {
        let (tx, mut rx) = framework_config_channel(16);
        let event = FrameworkConfigChanged {
            path: PathBuf::from("package.json"),
            change_kind: FsChangeKind::Modified,
        };
        tx.send(event.clone()).unwrap();
        let received = rx.try_recv().unwrap();
        assert_eq!(received.path, event.path);
        assert_eq!(received.change_kind, FsChangeKind::Modified);
    }
}
