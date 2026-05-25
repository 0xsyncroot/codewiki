//! T-503 — Benchmark: incremental sync after a single-file modification.
//!
//! Acceptance gate: ≤100ms for a 1k-file indexed project.
//!
//! Measures the file-system level of the sync path: read + append to a file,
//! which is the I/O cost that precedes the DB update. When the full
//! ExtractionOrchestrator + StorageImpl pipeline is available, replace the
//! file-append + read with `cg.sync()`.

use codewiki_testutil::{append_line, setup_indexed_project};
use criterion::{criterion_group, criterion_main, Criterion};
use std::time::Duration;

fn bench_incremental_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_sync");
    group.measurement_time(Duration::from_secs(20));

    // Setup: create a 1k-file project once outside the measurement loop.
    let (project_dir, target_file) = setup_indexed_project(1_000);

    group.bench_function("single_file_modify_1k_project", |b| {
        b.iter(|| {
            // Simulate a one-line edit: append a comment.
            append_line(&target_file, "// bench edit");

            // Simulate the read-back that the sync loop performs:
            // hash the modified file to detect changes.
            let content = std::fs::read(&target_file).expect("read file");
            // Use std::hint::black_box so the compiler does not elide the read.
            std::hint::black_box(content.len())
        });
    });

    group.finish();

    // Keep project_dir alive until end of bench function.
    drop(project_dir);
}

criterion_group!(benches, bench_incremental_sync);
criterion_main!(benches);
