// `codewiki upgrade [--check]` — self-update against the latest GitHub release.
//
// Design goals (mirrors install.sh / install.ps1 upgrade semantics):
//   * OFFLINE-SAFE: any network failure prints a friendly note and exits 0 —
//     never a hard error, never a panic.
//   * No new HTTP dependency: we shell out to `curl` (Unix/macOS) or PowerShell's
//     `Invoke-RestMethod` (Windows) to read the GitHub "latest release" tag,
//     exactly as the install scripts do. The API base URL is overridable via the
//     `CODEWIKI_RELEASE_API` env var so tests can point it at a local fixture.
//   * `--check` reports only; without it we re-invoke the platform installer
//     pinned to the resolved latest version.

use std::process::Command;

use anyhow::Result;

use crate::version::Version;

const REPO: &str = "0xsyncroot/codewiki";

/// Entry point for the `upgrade` subcommand.
pub fn run(check_only: bool) -> Result<()> {
    let current_str = env!("CARGO_PKG_VERSION");
    let current = Version::parse(current_str);

    // Resolve the latest release tag. Offline / parse failures are NOT errors:
    // we report and return Ok so scripts and CI never see a non-zero exit here.
    let latest_tag = match fetch_latest_tag() {
        Some(tag) => tag,
        None => {
            println!("codewiki {current_str}: could not check for updates (offline).");
            return Ok(());
        }
    };

    let latest = Version::parse(&latest_tag);

    match (current, latest) {
        (Some(cur), Some(lat)) => {
            use std::cmp::Ordering;
            match cur.cmp(&lat) {
                Ordering::Equal => {
                    println!("codewiki is up to date ({cur}).");
                    Ok(())
                }
                Ordering::Less => {
                    if check_only {
                        println!("update available: {cur} -> {lat}");
                        Ok(())
                    } else {
                        println!("Updating codewiki {cur} -> {lat}…");
                        run_installer(&latest_tag)
                    }
                }
                Ordering::Greater => {
                    // Installed build is newer than the latest *published* release
                    // (e.g. a dev/pre-release build). Nothing to do.
                    println!(
                        "codewiki {cur} is newer than the latest release ({lat}); nothing to do."
                    );
                    Ok(())
                }
            }
        }
        // If either version is unparseable, fall back to a best-effort path:
        // for --check we just report what we know; otherwise we still offer to
        // re-run the installer at the resolved tag (treated as a force install).
        _ => {
            if check_only {
                println!(
                    "codewiki {current_str}: latest release is {latest_tag} (could not compare versions)."
                );
                Ok(())
            } else {
                println!("Reinstalling codewiki at {latest_tag}…");
                run_installer(&latest_tag)
            }
        }
    }
}

/// Fetch the latest release tag from the GitHub API (or the override URL).
///
/// Returns `None` on ANY failure (no network tool, request error, missing
/// `tag_name`) so callers can degrade gracefully.
fn fetch_latest_tag() -> Option<String> {
    let url = release_api_url();
    let body = http_get(&url)?;
    extract_tag_name(&body)
}

/// The GitHub "latest release" endpoint, overridable for tests via
/// `CODEWIKI_RELEASE_API` (which may point at a `file://` URL or local path —
/// anything `curl` can fetch).
fn release_api_url() -> String {
    if let Ok(base) = std::env::var("CODEWIKI_RELEASE_API") {
        if !base.trim().is_empty() {
            return base;
        }
    }
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

/// Perform a best-effort HTTP(S)/file GET via an external tool.
///
/// We do not add an HTTP client crate; instead we use `curl` on Unix/macOS and
/// PowerShell's `Invoke-RestMethod` on Windows — the same tools the install
/// scripts rely on. Any failure yields `None`.
fn http_get(url: &str) -> Option<String> {
    #[cfg(windows)]
    {
        // -UseBasicParsing for older PowerShell; a UA header keeps GitHub happy.
        let script = format!(
            "$ProgressPreference='SilentlyContinue'; \
             try {{ (Invoke-WebRequest -Uri '{url}' -UseBasicParsing \
             -Headers @{{ 'User-Agent' = 'codewiki-cli' }}).Content }} \
             catch {{ exit 1 }}"
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let body = String::from_utf8_lossy(&out.stdout).into_owned();
        if body.trim().is_empty() {
            return None;
        }
        Some(body)
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("curl")
            .args(["-fsSL", "-H", "User-Agent: codewiki-cli", url])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let body = String::from_utf8_lossy(&out.stdout).into_owned();
        if body.trim().is_empty() {
            return None;
        }
        Some(body)
    }
}

/// Extract `"tag_name": "vX.Y.Z"` from a GitHub release JSON payload.
///
/// We parse with `serde_json` (already a dependency) but tolerate malformed
/// input by returning `None` rather than erroring.
fn extract_tag_name(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let tag = value.get("tag_name")?.as_str()?.trim();
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

/// Re-invoke the platform installer, pinned to `tag`, by downloading and running
/// the published install script. This reuses the battle-tested upgrade/rollback
/// logic in install.sh / install.ps1 rather than reimplementing it here.
fn run_installer(tag: &str) -> Result<()> {
    #[cfg(windows)]
    {
        let script = format!(
            "$ErrorActionPreference='Stop'; \
             $s = (Invoke-WebRequest -Uri \
             'https://raw.githubusercontent.com/{REPO}/main/install.ps1' \
             -UseBasicParsing).Content; \
             & ([scriptblock]::Create($s)) -Version '{tag}'"
        );
        let status = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status();
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(_) => {
                eprintln!(
                    "Installer did not complete. You can upgrade manually:\n  \
                     iwr https://raw.githubusercontent.com/{REPO}/main/install.ps1 | iex"
                );
                Ok(())
            }
            Err(_) => {
                eprintln!(
                    "Could not launch the installer. Upgrade manually:\n  \
                     iwr https://raw.githubusercontent.com/{REPO}/main/install.ps1 | iex"
                );
                Ok(())
            }
        }
    }
    #[cfg(not(windows))]
    {
        // `curl … | sh -s -- --version <tag>` — pass the pinned tag through to
        // the install script's argument parser.
        let cmd = format!(
            "curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh \
             | sh -s -- --version '{tag}'"
        );
        let status = Command::new("sh").args(["-c", &cmd]).status();
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(_) => {
                eprintln!(
                    "Installer did not complete. You can upgrade manually:\n  \
                     curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | sh"
                );
                Ok(())
            }
            Err(_) => {
                eprintln!(
                    "Could not launch the installer. Upgrade manually:\n  \
                     curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | sh"
                );
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tag_name_parses_valid_payload() {
        let body = r#"{"tag_name":"v0.2.0","name":"v0.2.0"}"#;
        assert_eq!(extract_tag_name(body), Some("v0.2.0".to_string()));
    }

    #[test]
    fn extract_tag_name_handles_malformed() {
        assert_eq!(extract_tag_name("not json"), None);
        assert_eq!(extract_tag_name("{}"), None);
        assert_eq!(extract_tag_name(r#"{"tag_name":""}"#), None);
        assert_eq!(extract_tag_name(r#"{"tag_name":42}"#), None);
    }

    #[test]
    fn release_api_url_honors_override() {
        // Use a unique value to avoid clobbering parallel tests; restore after.
        let prev = std::env::var("CODEWIKI_RELEASE_API").ok();
        std::env::set_var("CODEWIKI_RELEASE_API", "file:///tmp/fixture.json");
        assert_eq!(release_api_url(), "file:///tmp/fixture.json");
        // Empty override falls back to the default GitHub URL.
        std::env::set_var("CODEWIKI_RELEASE_API", "");
        assert!(release_api_url().contains("api.github.com"));
        match prev {
            Some(v) => std::env::set_var("CODEWIKI_RELEASE_API", v),
            None => std::env::remove_var("CODEWIKI_RELEASE_API"),
        }
    }
}
