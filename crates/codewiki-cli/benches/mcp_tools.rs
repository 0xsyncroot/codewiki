//! T-504 — Benchmark: MCP tool latency at corpus scale.
//!
//! Uses a pre-built read-only index (or falls back to a synthetic 1k-file corpus
//! if `CODEWIKI_BENCH_CORPUS` is not set).
//!
//! Acceptance gates (A7-perf-tests.md §5):
//!   - search p50 ≤ 30ms
//!   - callers/callees p50 ≤ 80ms
//!   - explore p50 ≤ 500ms
//!
//! When the full QueryHandle + MCP tool pipeline is available these benches
//! will call `cg.search_nodes(...)`, `cg.get_callers(...)`, and
//! `cg.find_relevant_context(...)`. Until then they benchmark the corpus
//! filesystem access path which is the common denominator.

use codewiki_testutil::get_or_create_synthetic_corpus;
use criterion::{criterion_group, criterion_main, Criterion};
use std::time::Duration;

/// Path to a pre-built large index. Set CODEWIKI_BENCH_CORPUS env var to override.
fn corpus_path() -> std::path::PathBuf {
    std::env::var("CODEWIKI_BENCH_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            // Fall back to the 1k-file synthetic corpus so CI always has something.
            get_or_create_synthetic_corpus(1_000)
        })
}

fn bench_mcp_tools(c: &mut Criterion) {
    let corpus = corpus_path();

    let mut group = c.benchmark_group("mcp_tools");
    group.measurement_time(Duration::from_secs(15));

    // Bench: enumerate source files (proxy for search index scan).
    group.bench_function("search_exact_name", |b| {
        let src = corpus.join("src");
        b.iter(|| {
            // Proxy: scan the corpus source directory and find files whose names
            // contain "0042" (deterministic exact-name lookup proxy).
            let found: Vec<_> = std::fs::read_dir(&src)
                .expect("read corpus src")
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains("0042"))
                .collect();
            std::hint::black_box(found)
        });
    });

    group.bench_function("search_fuzzy_bm25", |b| {
        let src = corpus.join("src");
        b.iter(|| {
            // Proxy: scan all files looking for a token (BM25 fuzzy match proxy).
            let count = std::fs::read_dir(&src)
                .expect("read corpus src")
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains("module"))
                .count();
            std::hint::black_box(count)
        });
    });

    group.bench_function("explore_50_nodes_3_hops", |b| {
        let src = corpus.join("src");
        b.iter(|| {
            // Proxy: read up to 50 files (node materialization proxy).
            let files: Vec<_> = std::fs::read_dir(&src)
                .expect("read corpus src")
                .filter_map(|e| e.ok())
                .take(50)
                .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
                .collect();
            std::hint::black_box(files.len())
        });
    });

    group.bench_function("callers_10_results", |b| {
        let src = corpus.join("src");
        b.iter(|| {
            // Proxy: scan files for call references to "processRequest" (callers proxy).
            let callers: Vec<_> = std::fs::read_dir(&src)
                .expect("read corpus src")
                .filter_map(|e| e.ok())
                .filter(|e| {
                    std::fs::read_to_string(e.path())
                        .unwrap_or_default()
                        .contains("processRequest")
                })
                .take(10)
                .collect();
            std::hint::black_box(callers.len())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_mcp_tools);
criterion_main!(benches);
