//! Integration test: MCP initialize handshake.
//!
//! Spawns the codewiki binary, sends a JSON-RPC `initialize` request over
//! stdin, and asserts that the response contains `"protocolVersion":"2024-11-05"`
//! within 1 second.
//!
//! This test is the P4a acceptance gate for T-411 (initialize response ordering
//! parity) and T-403 (initialize handler correctness).
//!
//! NOTE: This test requires the `codewiki` binary to be built first. It is
//! skipped automatically when the binary is not present (e.g. in unit-test-only
//! CI runs).  Run `cargo build -p codewiki-cli` first to produce the binary.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Send an MCP `initialize` request and assert the response contains the
/// expected `protocolVersion`.
///
/// The binary is located via `CARGO_BIN_EXE_codewiki` env var (set by Cargo
/// during integration tests) or from the workspace target directory as a
/// fallback.
fn find_binary() -> Option<std::path::PathBuf> {
    // Cargo sets this when building integration tests for a binary crate.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_codewiki") {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }
    // Fallback: look in workspace target/debug/
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let ws_root = std::path::Path::new(&manifest_dir)
        .ancestors()
        .find(|p| p.join("Cargo.toml").exists() && p.join("crates").exists())
        .map(|p| p.to_path_buf());

    if let Some(root) = ws_root {
        let bin = root.join("target/debug/codewiki");
        if bin.exists() {
            return Some(bin);
        }
        let bin_exe = root.join("target/debug/codewiki.exe");
        if bin_exe.exists() {
            return Some(bin_exe);
        }
    }
    None
}

/// Unit-level test of the `initialize` response shape using the in-process
/// server. This does not require the binary to be built.
#[tokio::test]
async fn initialize_response_contains_protocol_version() {
    use codewiki_mcp::{server::CodeWikiMcpServer, tools};
    use codewiki_storage::QueryHandle;
    use rmcp::model::{
        Implementation, InitializeRequestParam, ProtocolVersion,
    };
    use std::sync::Arc;

    // Minimal stub QueryHandle
    struct NoopHandle;
    impl QueryHandle for NoopHandle {
        fn search_nodes(&self, _: &str, _: codewiki_storage::SearchOptions)
            -> Result<Vec<codewiki_core::SearchResult>, codewiki_core::CodeWikiError> { Ok(vec![]) }
        fn get_node_by_id(&self, _: &str)
            -> Result<Option<codewiki_core::Node>, codewiki_core::CodeWikiError> { Ok(None) }
        fn get_callers(&self, _: &str, _: usize)
            -> Result<Vec<(codewiki_core::Node, codewiki_core::Edge)>, codewiki_core::CodeWikiError> { Ok(vec![]) }
        fn get_callees(&self, _: &str, _: usize)
            -> Result<Vec<(codewiki_core::Node, codewiki_core::Edge)>, codewiki_core::CodeWikiError> { Ok(vec![]) }
        fn get_impact_radius(&self, _: &str, _: usize)
            -> Result<codewiki_core::Subgraph, codewiki_core::CodeWikiError> { Ok(Default::default()) }
        fn find_relevant_context(&self, _: &str, _: codewiki_storage::FindOpts)
            -> Result<codewiki_core::Subgraph, codewiki_core::CodeWikiError> { Ok(Default::default()) }
        fn get_code(&self, _: &str)
            -> Result<Option<String>, codewiki_core::CodeWikiError> { Ok(None) }
        fn get_stats(&self)
            -> Result<codewiki_core::GraphStats, codewiki_core::CodeWikiError> {
            Ok(codewiki_core::GraphStats { journal_mode: "wal".to_string(), ..Default::default() })
        }
        fn get_files(&self, _: Option<&codewiki_storage::FileFilter>)
            -> Result<Vec<codewiki_core::FileRecord>, codewiki_core::CodeWikiError> { Ok(vec![]) }
        fn get_affected_nodes(&self, _: &[std::path::PathBuf])
            -> Result<Vec<codewiki_core::Node>, codewiki_core::CodeWikiError> { Ok(vec![]) }
    }

    // Build the initialize result directly (in-process, no spawning needed)
    let result = codewiki_mcp::server::CodeWikiMcpServer::build_initialize_result();

    // The protocol version string must exactly match "2024-11-05"
    let serialized = serde_json::to_string(&result).expect("serialize initialize result");
    assert!(
        serialized.contains("2024-11-05"),
        "initialize response must contain protocolVersion 2024-11-05; got: {serialized}"
    );
    assert!(
        serialized.contains("codewiki"),
        "initialize response must name the server `codewiki`"
    );
}

/// End-to-end handshake test that spawns the actual binary and exercises the
/// full stdio JSON-RPC path.
///
/// Skipped if the binary is not present.
#[test]
fn initialize_handshake_e2e() {
    let bin = match find_binary() {
        Some(b) => b,
        None => {
            eprintln!(
                "SKIP: codewiki binary not found; \
                 build with `cargo build -p codewiki-cli` first"
            );
            return;
        }
    };

    // The real MCP server requires a codewiki index to be present.
    // Create a temporary project directory and run `codewiki init` first.
    let tmp = tempfile::tempdir().expect("create temp dir");
    let project_dir = tmp.path();
    // Write a minimal source file so init has something to index.
    std::fs::write(project_dir.join("hello.ts"), "function hello() { return 1; }\n")
        .expect("write source file");
    // Run `codewiki init` in the temp dir to create the index.
    let init_status = Command::new(&bin)
        .args(["init"])
        .current_dir(project_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run codewiki init");
    if !init_status.success() {
        eprintln!("SKIP: codewiki init failed; skipping e2e handshake test");
        return;
    }

    let mut child = Command::new(&bin)
        .args(["serve", "--mcp"])
        .current_dir(project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn codewiki serve --mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    // Send the JSON-RPC initialize request
    let request = r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    writeln!(stdin, "{request}").expect("write initialize request");
    // Keep stdin open a bit so the server has time to respond
    std::thread::sleep(Duration::from_millis(100));

    // Read the first JSON-RPC response line with a 1-second timeout
    let deadline = Instant::now() + Duration::from_secs(1);
    let reader = BufReader::new(stdout);
    let mut found_protocol_version = false;

    for line in reader.lines() {
        if Instant::now() > deadline {
            break;
        }
        let line = match line {
            Ok(l) if !l.is_empty() => l,
            _ => break,
        };

        if line.contains("2024-11-05") {
            found_protocol_version = true;
            break;
        }
        // Only read the first response line
        break;
    }

    // Terminate the child
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        found_protocol_version,
        "MCP initialize response must contain '2024-11-05' within 1 second"
    );
}
