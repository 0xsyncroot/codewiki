// Integration tests for `codewiki upgrade` release-fetch behaviour.
//
// No real network: we point `CODEWIKI_RELEASE_API` at a local `file://` URL
// (fetched via `curl`, exactly as the production code does) or at an
// unreachable URL to exercise the offline path. We assert that `run()` never
// errors (offline-safe) and behaves correctly for the up-to-date / update /
// malformed cases.
//
// These tests are gated to non-Windows because the production fetch uses
// PowerShell's Invoke-WebRequest on Windows (which has no `file://` curl
// equivalent here); the platform-independent parsing logic is covered by the
// unit tests in `src/commands/upgrade.rs` and `src/version.rs`.

#![cfg(not(windows))]

use std::io::Write;

use codewiki_cli::commands::upgrade;
use serial_test::serial;
use tempfile::NamedTempFile;

/// Write a JSON fixture and return a `file://` URL pointing at it. The file
/// handle is returned so the caller keeps it alive for the test's duration.
fn fixture_url(json: &str) -> (NamedTempFile, String) {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(json.as_bytes()).unwrap();
    f.flush().unwrap();
    let url = format!("file://{}", f.path().display());
    (f, url)
}

struct ApiGuard;
impl Drop for ApiGuard {
    fn drop(&mut self) {
        std::env::remove_var("CODEWIKI_RELEASE_API");
    }
}

fn set_api(url: &str) -> ApiGuard {
    std::env::set_var("CODEWIKI_RELEASE_API", url);
    ApiGuard
}

#[test]
#[serial]
fn up_to_date_is_noop() {
    // Point the API at a fixture whose tag matches the current crate version.
    let current = env!("CARGO_PKG_VERSION");
    let (_f, url) = fixture_url(&format!(r#"{{"tag_name":"v{current}"}}"#));
    let _g = set_api(&url);
    // --check must succeed and not install anything.
    upgrade::run(true).expect("up-to-date check must be Ok");
}

#[test]
#[serial]
fn update_available_check_only_is_ok() {
    // A far-future tag → an update is "available", but --check must not install
    // and must return Ok.
    let (_f, url) = fixture_url(r#"{"tag_name":"v999.0.0"}"#);
    let _g = set_api(&url);
    upgrade::run(true).expect("update-available --check must be Ok");
}

#[test]
#[serial]
fn newer_local_build_is_noop() {
    // Older published release than the current build → nothing to do, still Ok.
    let (_f, url) = fixture_url(r#"{"tag_name":"v0.0.1"}"#);
    let _g = set_api(&url);
    upgrade::run(true).expect("newer-local --check must be Ok");
}

#[test]
#[serial]
fn malformed_release_json_check_only_is_ok() {
    // Unparseable tag → graceful report, never an error.
    let (_f, url) = fixture_url(r#"{"tag_name":"not-a-version"}"#);
    let _g = set_api(&url);
    upgrade::run(true).expect("malformed-tag --check must be Ok");
}

#[test]
#[serial]
fn offline_is_graceful() {
    // A nonexistent file:// path makes curl fail → offline path → Ok, no panic.
    let _g = set_api("file:///nonexistent/codewiki/release/fixture.json");
    upgrade::run(true).expect("offline --check must be Ok");
    // Also exercise the non-check path: offline must still be a graceful no-op
    // (it returns before attempting to run any installer).
    upgrade::run(false).expect("offline install must be Ok");
}

#[test]
#[serial]
fn empty_api_override_does_not_panic_offline() {
    // Empty override → falls back to the real GitHub URL. In CI without network
    // this hits the offline branch; with network it would resolve a real tag.
    // Either way it must return Ok and never panic.
    let _g = set_api("");
    upgrade::run(true).expect("empty-override --check must be Ok");
}
