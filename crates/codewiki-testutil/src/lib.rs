//! T-504 — codewiki-testutil: synthetic project generator and bench helpers.
//!
//! Provides:
//! - `generate_synthetic_ts_project(n)` — N `.ts` files in a TempDir
//! - `setup_indexed_project(n)` — generate + return TempDir (no full index; bench-safe)
//! - `get_or_create_synthetic_corpus(n)` — cached corpus under CARGO_TARGET_DIR
//! - `stable_node_id(name)` — synthetic ID for a well-known symbol name
//! - `append_line(path, line)` — append a line (for incremental-sync benches)

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub use tempfile::TempDir;

// ── Synthetic project generation ─────────────────────────────────────────────

/// Generate a synthetic TypeScript project with `n` source files.
///
/// Each file `src/module_NNNN.ts` contains:
/// - one exported class `Module<N>Service` with two methods
/// - three call-expression stubs referencing the next module
///
/// Returns a `TempDir` that keeps the directory alive; drop to clean up.
pub fn generate_synthetic_ts_project(n: usize) -> TempDir {
    let dir = TempDir::new().expect("create tempdir");
    let src = dir.path().join("src");
    fs::create_dir_all(&src).expect("create src dir");

    for i in 0..n {
        let next = (i + 1) % n.max(1);
        let content = format!(
            r#"// Auto-generated synthetic file {i}
// codewiki-testutil v{ver}

export class Module{i}Service {{
    private value: number = {i};

    processRequest(input: string): string {{
        const result = this.transform(input);
        return `processed-${{result}}`;
    }}

    transform(data: string): string {{
        return data.toUpperCase() + "-{i}";
    }}
}}

export function utilHelper{i}(x: number): number {{
    return x * {i} + 1;
}}

// Cross-module calls (unresolved refs for resolution phase)
function bootstrapModule{i}(): void {{
    const svc = new Module{next}Service();
    svc.processRequest("init");
    utilHelper{next}(42);
}}
"#,
            i = i,
            next = next,
            ver = env!("CARGO_PKG_VERSION"),
        );

        let file_path = src.join(format!("module_{i:04}.ts", i = i));
        fs::write(&file_path, &content).expect("write ts file");
    }

    // Write a minimal tsconfig.json so the resolver can detect the project root.
    let tsconfig = serde_json::json!({
        "compilerOptions": {
            "target": "ES2020",
            "module": "commonjs",
            "strict": true
        }
    });
    fs::write(
        dir.path().join("tsconfig.json"),
        serde_json::to_string_pretty(&tsconfig).unwrap(),
    )
    .expect("write tsconfig.json");

    dir
}

/// Generate + return a TempDir for bench setup.
///
/// Does NOT call `index_all` — that would require a full StorageImpl dependency
/// which testutil intentionally avoids. Benches that need a pre-indexed project
/// should call `generate_synthetic_ts_project` and then invoke their own
/// StorageImpl setup inline.
///
/// Returns `(TempDir, path_to_first_source_file)` so benches can locate files.
pub fn setup_indexed_project(n: usize) -> (TempDir, PathBuf) {
    let dir = generate_synthetic_ts_project(n);
    let first_file = dir.path().join("src").join("module_0000.ts");
    (dir, first_file)
}

// ── Corpus cache ─────────────────────────────────────────────────────────────

/// Return a path to a cached synthetic corpus of `n` files.
///
/// The corpus is generated once under `$CARGO_TARGET_DIR/codewiki-testutil/corpus/n-<N>/`
/// and reused across bench runs. If `CARGO_TARGET_DIR` is not set, falls back to
/// a temporary directory under `/tmp`.
///
/// **Note**: the returned `PathBuf` is a plain directory, NOT a `TempDir`;
/// it persists across runs for cache efficiency.
pub fn get_or_create_synthetic_corpus(n: usize) -> PathBuf {
    let cache_root = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("codewiki-testutil")
        .join("corpus")
        .join(format!("n-{n}"));

    if cache_root.exists() && cache_root.join("tsconfig.json").exists() {
        // Already generated; check file count.
        let count = fs::read_dir(cache_root.join("src"))
            .map(|rd| rd.count())
            .unwrap_or(0);
        if count == n {
            return cache_root;
        }
    }

    // (Re)generate.
    let _ = fs::remove_dir_all(&cache_root);
    fs::create_dir_all(cache_root.join("src")).expect("create corpus dir");

    // Write files directly (no TempDir — we want persistence).
    for i in 0..n {
        let next = (i + 1) % n.max(1);
        let content = format!(
            r#"// Corpus file {i} of {n}
export class Module{i}Service {{
    processRequest(input: string): string {{ return input + "-{i}"; }}
    transform(data: string): string {{ return data.toUpperCase(); }}
}}
export function utilHelper{i}(x: number): number {{ return x + {i}; }}
function bootstrap{i}(): void {{
    const s = new Module{next}Service();
    s.processRequest("x");
    utilHelper{next}(0);
}}
"#,
            i = i,
            n = n,
            next = next,
        );
        let p = cache_root.join("src").join(format!("module_{i:04}.ts", i = i));
        fs::write(&p, &content).expect("write corpus file");
    }

    let tsconfig = serde_json::json!({
        "compilerOptions": { "target": "ES2020", "module": "commonjs" }
    });
    fs::write(
        cache_root.join("tsconfig.json"),
        serde_json::to_string_pretty(&tsconfig).unwrap(),
    )
    .expect("write tsconfig");

    cache_root
}

// ── Bench helpers ─────────────────────────────────────────────────────────────

/// Return a stable synthetic node ID for a well-known symbol name.
///
/// Used in bench setup (not in iteration) to get a node ID to pass to
/// `get_callers`, etc. The ID format mirrors extraction: `sha256(path + name)`.
///
/// For synthetic corpora the formula is deterministic:
/// `"bench-node::{name}"` — a sentinel that the bench corpus can recognise.
/// Benches that need a real DB node ID should look it up from the live index.
pub fn stable_node_id(_name: &str) -> String {
    // In the synthetic corpus, node IDs are generated by the extractor.
    // For bench purposes, return a deterministic placeholder; the actual bench
    // should substitute a real ID from `search_nodes` if the DB is live.
    "bench-synthetic-node-id-for-processRequest".to_string()
}

/// Append a line to a file.
///
/// Used in incremental-sync benches to simulate a single-line edit.
pub fn append_line(path: &Path, line: &str) {
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("append_line: open {:?}: {e}", path));
    writeln!(f, "{}", line).expect("append_line: write");
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_creates_n_files() {
        let dir = generate_synthetic_ts_project(10);
        let count = fs::read_dir(dir.path().join("src"))
            .expect("read src dir")
            .count();
        assert_eq!(count, 10, "expected 10 files, got {count}");
    }

    #[test]
    fn generate_files_are_valid_typescript() {
        let dir = generate_synthetic_ts_project(3);
        for i in 0..3 {
            let p = dir.path().join("src").join(format!("module_{i:04}.ts", i = i));
            assert!(p.exists(), "module_{i:04}.ts should exist");
            let content = fs::read_to_string(&p).expect("read file");
            assert!(
                content.contains("export class"),
                "file should contain a class"
            );
            assert!(
                content.contains("processRequest"),
                "file should contain processRequest"
            );
        }
    }

    #[test]
    fn tsconfig_written() {
        let dir = generate_synthetic_ts_project(5);
        let tsconfig = dir.path().join("tsconfig.json");
        assert!(tsconfig.exists(), "tsconfig.json should exist");
        let val: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&tsconfig).unwrap()).unwrap();
        assert!(val.get("compilerOptions").is_some());
    }

    #[test]
    fn append_line_works() {
        let dir = generate_synthetic_ts_project(1);
        let p = dir.path().join("src").join("module_0000.ts");
        let before = fs::read_to_string(&p).unwrap();
        append_line(&p, "// bench edit");
        let after = fs::read_to_string(&p).unwrap();
        assert!(after.len() > before.len(), "file should be longer after append");
        assert!(after.ends_with("// bench edit\n"), "last line should be the appended line");
    }

    #[test]
    fn setup_indexed_project_returns_first_file() {
        let (dir, first) = setup_indexed_project(5);
        assert!(first.exists(), "first file should exist");
        let _ = dir; // keep alive
    }

    #[test]
    fn generate_100_files() {
        let dir = generate_synthetic_ts_project(100);
        let count = fs::read_dir(dir.path().join("src"))
            .expect("read src dir")
            .count();
        assert_eq!(count, 100);
    }
}
