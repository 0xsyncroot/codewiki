// End-to-end shell test for the install.sh UPGRADE path — fully offline.
//
// We build local "release" fixtures (a tar.gz containing a tiny `codewiki`
// shell script that prints a version, plus its `.sha256`) and point the
// installer at them via `CODEWIKI_DOWNLOAD_BASE` (asset base) and
// `CODEWIKI_RELEASE_API` (latest-tag JSON). No real GitHub access occurs.
//
// Covered scenarios:
//   * fresh install
//   * re-run on the same version → no-op (no replacement)
//   * upgrade old -> new
//   * rollback when the freshly downloaded binary is non-functional
//
// Unix-only: the Windows installer is PowerShell and is reviewed separately.
// Also skipped if the required POSIX tooling is unavailable on the runner.

#![cfg(not(windows))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Repo root = two levels up from this crate (crates/codewiki-cli -> repo).
fn install_sh() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("install.sh")
}

fn have_tools() -> bool {
    for tool in ["bash", "curl", "tar", "sha256sum"] {
        let ok = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {tool} >/dev/null 2>&1"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return false;
        }
    }
    true
}

/// Detect the target triple the installer will compute for this host, so our
/// fixture archive is named exactly what install.sh asks curl for.
fn target_triple() -> Option<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let osfam = match os {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        _ => return None,
    };
    let arch_norm = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    Some(format!("{arch_norm}-{osfam}"))
}

/// Build a release fixture: a tar.gz with a `codewiki` script + its `.sha256`,
/// laid out under `<base>/` so `CODEWIKI_DOWNLOAD_BASE=<base>` resolves the
/// archive. `functional` controls whether the fake binary's `--version`
/// succeeds (exit 0) or fails (exit 1) — the latter drives the rollback path.
fn make_release(base: &Path, triple: &str, version: &str, functional: bool) {
    fs::create_dir_all(base).unwrap();
    let staging = base.join(format!("stage-{version}"));
    fs::create_dir_all(&staging).unwrap();

    // The fake binary: a shell script named `codewiki`.
    let script = if functional {
        format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"codewiki {version}\"; exit 0; fi\nexit 0\n")
    } else {
        // Non-functional: `--version` exits non-zero so the smoke test fails.
        "#!/bin/sh\nexit 3\n".to_string()
    };
    let bin_path = staging.join("codewiki");
    fs::write(&bin_path, script).unwrap();
    // chmod +x.
    let mut perms = fs::metadata(&bin_path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&bin_path, perms).unwrap();

    let archive = format!("codewiki-{triple}.tar.gz");
    // Tar the single `codewiki` file (no leading dir) so install.sh's
    // `find -maxdepth 1 -name codewiki` locates it after extraction.
    let status = Command::new("tar")
        .args(["-czf", base.join(&archive).to_str().unwrap(), "-C"])
        .arg(&staging)
        .arg("codewiki")
        .status()
        .unwrap();
    assert!(status.success(), "tar must succeed");

    // Compute the checksum file in the format `sha256sum -c` expects:
    // "<hash>  <archive>" with the archive referenced by bare name.
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "cd {} && sha256sum {} > {}.sha256",
            base.to_str().unwrap(),
            archive,
            archive
        ))
        .status()
        .unwrap();
    assert!(out.success(), "sha256sum must succeed");
}

/// Run install.sh with the given pinned version, download base, and install dir.
/// Returns (success, combined stdout+stderr).
fn run_install(install_dir: &Path, download_base: &Path, version: &str) -> (bool, String) {
    let api = install_dir.join("__api.json");
    // Latest-release JSON pointing curl at the pinned tag (only used when the
    // installer resolves "latest"; harmless when --version is passed).
    fs::write(&api, format!(r#"{{"tag_name":"{version}"}}"#)).unwrap();

    let output = Command::new("bash")
        .arg(install_sh())
        .args(["--version", version, "--dir", install_dir.to_str().unwrap()])
        .env(
            "CODEWIKI_DOWNLOAD_BASE",
            format!("file://{}", download_base.display()),
        )
        .env("CODEWIKI_RELEASE_API", format!("file://{}", api.display()))
        // Keep PATH so curl/tar/sha256sum resolve.
        .output()
        .unwrap();
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), combined)
}

fn installed_version(install_dir: &Path) -> String {
    let bin = install_dir.join("codewiki");
    let out = Command::new(&bin).arg("--version").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn install_sh_fresh_noop_upgrade_and_rollback() {
    if !have_tools() {
        eprintln!("skipping: required POSIX tooling not present");
        return;
    }
    let triple = match target_triple() {
        Some(t) => t,
        None => {
            eprintln!("skipping: unsupported target");
            return;
        }
    };

    let tmp = TempDir::new().unwrap();
    let install_dir = tmp.path().join("bin");
    fs::create_dir_all(&install_dir).unwrap();

    // Build three release fixtures, each under its own base dir.
    let base_old = tmp.path().join("rel-old");
    let base_new = tmp.path().join("rel-new");
    let base_broken = tmp.path().join("rel-broken");
    make_release(&base_old, &triple, "0.1.0", true);
    make_release(&base_new, &triple, "0.2.0", true);
    make_release(&base_broken, &triple, "0.3.0", false);

    // 1) Fresh install of v0.1.0.
    let (ok, log) = run_install(&install_dir, &base_old, "v0.1.0");
    assert!(ok, "fresh install must succeed; log:\n{log}");
    assert_eq!(installed_version(&install_dir), "codewiki 0.1.0");

    // 2) Re-run on the same version → no-op, no download/replace.
    let (ok, log) = run_install(&install_dir, &base_old, "v0.1.0");
    assert!(ok, "re-run-same must succeed; log:\n{log}");
    assert!(
        log.contains("already on"),
        "re-run-same must report a no-op; log:\n{log}"
    );
    assert_eq!(installed_version(&install_dir), "codewiki 0.1.0");

    // 3) Upgrade v0.1.0 -> v0.2.0.
    let (ok, log) = run_install(&install_dir, &base_new, "v0.2.0");
    assert!(ok, "upgrade must succeed; log:\n{log}");
    assert!(
        log.contains("Upgraded") || log.contains("Upgrading"),
        "upgrade must announce itself; log:\n{log}"
    );
    assert_eq!(installed_version(&install_dir), "codewiki 0.2.0");

    // 4) Rollback: attempt to "upgrade" to a broken v0.3.0 whose --version fails.
    //    The installer must fail, restore the v0.2.0 binary, and leave it working.
    let (ok, log) = run_install(&install_dir, &base_broken, "v0.3.0");
    assert!(!ok, "broken upgrade must fail (non-zero exit); log:\n{log}");
    assert!(
        log.contains("smoke test") || log.contains("Rolling back"),
        "broken upgrade must mention the smoke-test failure / rollback; log:\n{log}"
    );
    assert_eq!(
        installed_version(&install_dir),
        "codewiki 0.2.0",
        "after rollback the previous working version must remain"
    );
}
