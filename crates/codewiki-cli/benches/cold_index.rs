//! T-501/T-502 — Benchmark: cold index of a synthetic TypeScript project.
//!
//! Acceptance gate: ≤15s for 100k files (proxied by linear extrapolation from 1k).
//!
//! Since the full StorageImpl + ExtractionOrchestrator pipeline involves C FFI (tree-sitter)
//! and SQLite, this bench measures the **corpus generation + file I/O** path and acts as a
//! compilation-and-scaling smoke test. When the full pipeline is wired in P4/P5 the
//! `index_all()` call can replace the file-system walk.

use codewiki_testutil::generate_synthetic_ts_project;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::time::Duration;

fn bench_cold_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_index");
    group.measurement_time(Duration::from_secs(30));
    group.sample_size(10); // index is expensive; 10 samples is enough for p50/p99

    for &file_count in &[100usize, 500, 1_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(file_count),
            &file_count,
            |b, &n| {
                b.iter_batched(
                    || n, // setup: just pass n (project generation is inside iter)
                    |n| {
                        // Generate the synthetic project — this exercises the
                        // file-generation + I/O path which is the corpus creation
                        // bottleneck at 100k scale.
                        let project_dir = generate_synthetic_ts_project(n);

                        // Count files to verify corpus integrity (exercises the dir-walk path).
                        let count = std::fs::read_dir(project_dir.path().join("src"))
                            .expect("read src dir")
                            .count();
                        assert_eq!(count, n);

                        // Return project_dir so its drop time is not measured.
                        project_dir
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_cold_index);
criterion_main!(benches);
