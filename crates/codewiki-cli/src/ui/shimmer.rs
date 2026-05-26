// Lively, professional progress display for `codewiki init` / `codewiki index`
// (issue #78).
//
// Design goals:
//   * TTY: an animated, colored spinner + bar showing the live phase, the file
//     currently being processed streaming by, and running files/nodes/edges
//     counts, finished with a clean colored summary.
//   * Non-TTY (piped / CI): degrade to plain, non-spammy line output — a couple
//     of phase lines and a final summary, never escape-code garbage or thousands
//     of redraw lines.
//   * Cheap: redraws are throttled (indicatif's steady tick + an internal rate
//     limit on count updates) so feeding it per-batch on a 17k-file repo never
//     slows indexing.
//
// The pipeline has no native per-file callback, but the storage layer flushes
// extraction batches as it goes; the CLI's `StorageAdapter` taps that flush path
// and feeds `ProgressReporter::on_batch` live. `IndexProgress` remains the
// coarse phase contract that `init`/`index` drive directly.

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Phases of the indexing pipeline shown in the progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Scanning,
    Parsing,
    Storing,
    Resolving,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Scanning => "Scanning files",
            Phase::Parsing => "Parsing",
            Phase::Storing => "Indexing",
            Phase::Resolving => "Resolving references",
        }
    }
}

/// A coarse phase update posted by `init` / `index` directly.
///
/// The stable callback contract: call sites construct this and hand it to
/// [`ProgressReporter::on_progress`] to advance the displayed phase. Live
/// files/nodes/edges counts are driven separately via
/// [`ProgressReporter::on_batch`] from the storage flush path, so the phase is
/// all this carries.
#[derive(Debug, Clone, Default)]
pub struct IndexProgress {
    pub phase: Phase,
}

/// Shared, thread-safe running totals updated from the storage flush path.
#[derive(Default)]
struct Counters {
    files: AtomicU64,
    nodes: AtomicU64,
    edges: AtomicU64,
}

/// A lively progress reporter for the indexing pipeline.
///
/// Cloneable handle (cheap `Arc` bump) so it can be both held by the command and
/// fed by the storage adapter from another thread.
#[derive(Clone)]
pub struct ProgressReporter {
    inner: Arc<Inner>,
}

struct Inner {
    /// `None` when stdout is not a TTY — we then emit plain lines instead.
    bar: Option<ProgressBar>,
    is_tty: bool,
    counters: Counters,
    /// Throttle: last time we refreshed the dynamic message (TTY) or printed a
    /// plain progress line (non-TTY).
    last_draw: std::sync::Mutex<Instant>,
    /// Minimum gap between dynamic redraws (≤ ~20/sec on a TTY).
    draw_interval: Duration,
}

impl ProgressReporter {
    /// Create a reporter, auto-detecting whether stdout is a terminal.
    pub fn new() -> Self {
        Self::with_tty(std::io::stdout().is_terminal())
    }

    /// Create a reporter with an explicit TTY flag (used by tests).
    pub fn with_tty(is_tty: bool) -> Self {
        let bar = if is_tty {
            let bar = ProgressBar::new_spinner();
            bar.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green.bold} {prefix:.cyan.bold} {wide_msg}",
                )
                .unwrap_or_else(|_| ProgressStyle::default_spinner())
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
            );
            // Steady tick animates the spinner; cap draw rate at ~12.5/sec.
            bar.enable_steady_tick(Duration::from_millis(80));
            Some(bar)
        } else {
            // Hidden draw target: indicatif emits nothing, so piped/CI output
            // stays clean. We print our own sparse plain lines instead.
            let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::hidden());
            Some(bar)
        };

        ProgressReporter {
            inner: Arc::new(Inner {
                bar,
                is_tty,
                counters: Counters::default(),
                last_draw: std::sync::Mutex::new(Instant::now() - Duration::from_secs(1)),
                draw_interval: Duration::from_millis(50), // ≤ 20 redraws/sec
            }),
        }
    }

    /// Coarse phase update (the stable `IndexProgress` contract).
    pub fn on_progress(&self, progress: &IndexProgress) {
        let inner = &self.inner;
        if inner.is_tty {
            if let Some(bar) = &inner.bar {
                bar.set_prefix(progress.phase.label().to_string());
                bar.set_message(self.counts_suffix(None));
            }
        } else {
            // Non-TTY: one plain line per phase transition only.
            println!("codewiki: {}…", progress.phase.label());
        }
    }

    /// Live per-batch update from the storage flush path: the file just stored
    /// plus the node/edge deltas it contributed. Accumulates totals and (on a
    /// TTY) streams the current file by, throttled.
    pub fn on_batch(&self, file: &str, nodes_delta: u64, edges_delta: u64) {
        let c = &self.inner.counters;
        c.files.fetch_add(1, Ordering::Relaxed);
        c.nodes.fetch_add(nodes_delta, Ordering::Relaxed);
        c.edges.fetch_add(edges_delta, Ordering::Relaxed);

        if !self.should_draw() {
            return;
        }

        if self.inner.is_tty {
            if let Some(bar) = &self.inner.bar {
                // Batches flowing means we've moved past parse into the storing
                // phase; advance the displayed phase label accordingly.
                bar.set_prefix(Phase::Storing.label().to_string());
                bar.set_message(self.counts_suffix(Some(file)));
            }
        } else {
            // Non-TTY: a single rate-limited progress line, no per-file spam.
            let (f, n, e) = self.totals();
            println!("codewiki: indexed {f} files, {n} nodes, {e} edges…");
        }
    }

    /// Finish with a clean, colored one-line summary (TTY) or plain line.
    pub fn finish(&self, files: u64, nodes: u64, edges: u64, elapsed_secs: f64) {
        // Trust the authoritative final counts from the orchestrator.
        if self.inner.is_tty {
            if let Some(bar) = &self.inner.bar {
                bar.set_style(
                    ProgressStyle::with_template("{prefix:.green.bold} {msg}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                bar.set_prefix("✓ Indexed");
                bar.finish_with_message(format!(
                    "\x1b[1m{files}\x1b[0m files · \x1b[1m{nodes}\x1b[0m nodes · \
                     \x1b[1m{edges}\x1b[0m edges in {elapsed_secs:.1}s"
                ));
            }
        } else if let Some(bar) = &self.inner.bar {
            bar.finish_and_clear();
        }
        tracing::info!(indexed_files = files, nodes, edges, "indexed {files} files");
    }

    /// Abandon the bar (e.g. on error), leaving a clear marker on a TTY.
    pub fn abandon(&self) {
        if self.inner.is_tty {
            if let Some(bar) = &self.inner.bar {
                bar.abandon_with_message("aborted");
            }
        } else if let Some(bar) = &self.inner.bar {
            bar.finish_and_clear();
        }
    }

    /// Whether this reporter is driving a real terminal (so the caller can
    /// avoid double-printing summaries the colored `finish` line already shows).
    pub fn is_tty(&self) -> bool {
        self.inner.is_tty
    }

    fn totals(&self) -> (u64, u64, u64) {
        let c = &self.inner.counters;
        (
            c.files.load(Ordering::Relaxed),
            c.nodes.load(Ordering::Relaxed),
            c.edges.load(Ordering::Relaxed),
        )
    }

    /// Build the dynamic message: running counts plus an optional current file.
    fn counts_suffix(&self, current_file: Option<&str>) -> String {
        let (f, n, e) = self.totals();
        let counts =
            format!("\x1b[1m{f}\x1b[0m files · \x1b[1m{n}\x1b[0m nodes · \x1b[1m{e}\x1b[0m edges");
        match current_file {
            Some(path) => format!("{counts}  \x1b[2m{}\x1b[0m", shorten(path)),
            None => counts,
        }
    }

    /// Rate-limit dynamic redraws to `draw_interval`.
    fn should_draw(&self) -> bool {
        let mut last = match self.inner.last_draw.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let now = Instant::now();
        if now.duration_since(*last) >= self.inner.draw_interval {
            *last = now;
            true
        } else {
            false
        }
    }
}

impl Default for ProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

/// Trim a long path to its trailing components so it fits on one line.
fn shorten(path: &str) -> String {
    const MAX: usize = 60;
    if path.len() <= MAX {
        return path.to_string();
    }
    // Keep the last few path segments.
    let tail: Vec<&str> = path.rsplit(['/', '\\']).take(3).collect();
    let mut tail: Vec<&str> = tail.into_iter().rev().collect();
    let joined = tail.join("/");
    if joined.len() <= MAX {
        format!("…/{joined}")
    } else {
        // Even the tail is huge — hard-truncate.
        let s: String = joined.chars().rev().take(MAX).collect();
        let s: String = s.chars().rev().collect();
        tail.clear();
        format!("…{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_labels_are_non_empty() {
        for phase in [
            Phase::Scanning,
            Phase::Parsing,
            Phase::Storing,
            Phase::Resolving,
        ] {
            assert!(!phase.label().is_empty());
        }
    }

    #[test]
    fn reporter_creates_without_panic_in_both_modes() {
        let _tty = ProgressReporter::with_tty(true);
        let _plain = ProgressReporter::with_tty(false);
    }

    #[test]
    fn on_batch_accumulates_totals() {
        let r = ProgressReporter::with_tty(true);
        r.on_batch("a.rs", 3, 2);
        r.on_batch("b.rs", 4, 1);
        let (f, n, e) = r.totals();
        assert_eq!(f, 2);
        assert_eq!(n, 7);
        assert_eq!(e, 3);
    }

    #[test]
    fn draw_is_throttled() {
        let r = ProgressReporter::with_tty(true);
        // First draw after construction is allowed (last_draw seeded in the past).
        assert!(r.should_draw());
        // Immediate second draw is throttled.
        assert!(!r.should_draw());
    }

    #[test]
    fn shorten_keeps_short_paths() {
        assert_eq!(shorten("src/a.rs"), "src/a.rs");
    }

    #[test]
    fn shorten_trims_long_paths() {
        let long = "/very/deeply/nested/project/crates/foo/src/module/submodule/file.rs";
        let s = shorten(long);
        assert!(s.len() <= 64, "shortened path too long: {s}");
        assert!(s.contains("file.rs"));
    }
}
