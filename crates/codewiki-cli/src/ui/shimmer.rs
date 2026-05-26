// Determinate, professional progress display for `codewiki init` / `codewiki
// index`.
//
// Design goals:
//   * TTY: a determinate progress bar that fills 0→100% as files are indexed,
//     showing the live phase, a filled bar + percent, the running
//     files/nodes/edges counts and the file currently streaming by. Each
//     completed phase leaves a permanent `✓` summary line; the bar is cleared
//     at the end.
//   * Non-TTY (piped / CI): degrade to plain, non-spammy output — one line per
//     phase transition plus the final summary lines, never escape-code garbage
//     or thousands of redraw lines.
//   * Cheap: the bar advances on every file but the (formatting-heavy) detail
//     line is rate-limited, and indicatif's steady tick animates the spinner,
//     so feeding it per-batch on a 17k-file repo never slows indexing.
//
// The total file count needed to make the bar determinate arrives via
// `set_total`, which the CLI's `StorageAdapter::begin_index` forwards from the
// orchestrator's discovery pass (the count of source files about to stream).
// `IndexProgress` remains the coarse phase contract that `init`/`index` drive
// directly; `on_batch` streams live counts from the storage flush path.

use indicatif::{HumanCount, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::glyphs::{self, Glyphs};

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
/// [`ProgressReporter::on_progress`] to advance the displayed phase. The
/// transition into the determinate "Indexing" bar happens separately via
/// [`ProgressReporter::set_total`] (driven from the storage discovery pass),
/// and live files/nodes/edges counts via [`ProgressReporter::on_batch`], so the
/// phase is all this carries.
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
    /// The progress bar. Always present; drawn to a hidden target (so it emits
    /// nothing) when stdout is not a TTY — we print our own sparse plain lines
    /// instead. All `ProgressBar` methods take `&self` (interior mutability),
    /// so a single shared bar is reused across every phase.
    bar: Option<ProgressBar>,
    is_tty: bool,
    /// Whether the terminal renders Unicode glyphs (drives spinner / bar chars
    /// and the success mark, with an ASCII fallback otherwise).
    unicode: bool,
    /// Whether to emit ANSI color in the hand-rolled summary/detail lines.
    /// True on a TTY unless `NO_COLOR` is set (indicatif strips its own
    /// template colors under the same condition, keeping the two consistent).
    color: bool,
    glyphs: &'static Glyphs,
    counters: Counters,
    /// Throttle: last time we refreshed the dynamic detail line.
    last_draw: std::sync::Mutex<Instant>,
    /// Minimum gap between dynamic detail-line redraws (≤ ~20/sec on a TTY).
    draw_interval: Duration,
}

impl ProgressReporter {
    /// Create a reporter, auto-detecting whether stdout is a terminal.
    pub fn new() -> Self {
        Self::with_tty(std::io::stdout().is_terminal())
    }

    /// Create a reporter with an explicit TTY flag (used by tests).
    pub fn with_tty(is_tty: bool) -> Self {
        let unicode = glyphs::use_unicode();
        let color = is_tty && std::env::var_os("NO_COLOR").is_none();
        let bar = if is_tty {
            // Draw to stdout so the bar, its `println` summary lines, and the
            // commands' trailing `println!` messages share one ordered stream
            // (TTY detection above is on stdout too).
            let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stdout());
            bar.set_style(spinner_style(unicode));
            // Steady tick animates the spinner even when no batch arrives; cap
            // draw rate at ~12.5/sec.
            bar.enable_steady_tick(Duration::from_millis(80));
            Some(bar)
        } else {
            // Hidden draw target: indicatif emits nothing, so piped/CI output
            // stays clean. We print our own sparse plain lines instead.
            Some(ProgressBar::with_draw_target(
                None,
                ProgressDrawTarget::hidden(),
            ))
        };

        ProgressReporter {
            inner: Arc::new(Inner {
                bar,
                is_tty,
                unicode,
                color,
                glyphs: glyphs::glyphs(),
                counters: Counters::default(),
                last_draw: std::sync::Mutex::new(Instant::now() - Duration::from_secs(1)),
                draw_interval: Duration::from_millis(50), // ≤ 20 redraws/sec
            }),
        }
    }

    /// Coarse phase update (the stable `IndexProgress` contract).
    ///
    /// Renders the phase as an indeterminate spinner. The determinate
    /// "Indexing" bar is entered separately via [`Self::set_total`]; reaching
    /// the [`Phase::Resolving`] phase switches back to a spinner because the
    /// resolution count is only known once it completes.
    pub fn on_progress(&self, progress: &IndexProgress) {
        let inner = &self.inner;
        if inner.is_tty {
            if let Some(bar) = &inner.bar {
                bar.set_style(spinner_style(inner.unicode));
                bar.set_prefix(progress.phase.label().to_string());
                bar.set_message(String::new());
            }
        } else {
            // Non-TTY: one plain line per phase transition only.
            println!("codewiki: {}…", progress.phase.label());
        }
    }

    /// Enter the determinate "Indexing" phase with `total` files to process.
    ///
    /// Forwarded from `StorageAdapter::begin_index` — `total` is the count of
    /// source files the orchestrator is about to stream, i.e. exactly the
    /// number of [`Self::on_batch`] calls that follow, so the bar fills to 100%.
    pub fn set_total(&self, total: u64) {
        if total == 0 {
            // Empty repo / nothing to index — leave the spinner; the caller
            // abandons it once `index_all` returns.
            return;
        }
        if !self.inner.is_tty {
            return;
        }
        if let Some(bar) = &self.inner.bar {
            bar.set_style(indexing_style(self.inner.unicode));
            bar.set_length(total);
            bar.set_position(0);
            bar.set_prefix(Phase::Storing.label().to_string());
            bar.set_message(String::new());
        }
    }

    /// Live per-batch update from the storage flush path: the file just stored
    /// plus the node/edge deltas it contributed. Accumulates totals and (on a
    /// TTY) advances the bar, streaming the current file by — the bar position
    /// updates every file while the formatting-heavy detail line is throttled.
    pub fn on_batch(&self, file: &str, nodes_delta: u64, edges_delta: u64) {
        let c = &self.inner.counters;
        let files = c.files.fetch_add(1, Ordering::Relaxed) + 1;
        c.nodes.fetch_add(nodes_delta, Ordering::Relaxed);
        c.edges.fetch_add(edges_delta, Ordering::Relaxed);

        if !self.inner.is_tty {
            // Non-TTY: counters only; the final summary reports them. No
            // per-file spam in piped/CI logs.
            return;
        }

        if !self.should_draw() {
            return;
        }
        if let Some(bar) = &self.inner.bar {
            bar.set_position(files);
            bar.set_message(self.detail_line(file));
        }
    }

    /// Finish the indexing phase: snap the bar to 100% and leave a permanent
    /// `✓ Indexed …` summary line. The bar stays alive for the resolving phase.
    pub fn finish_index(&self, files: u64, nodes: u64, edges: u64, elapsed_secs: f64) {
        if self.inner.is_tty {
            if let Some(bar) = &self.inner.bar {
                if let Some(len) = bar.length() {
                    bar.set_position(len); // snap to 100% even if a file failed to read
                }
                bar.set_message(String::new());
                let color = self.inner.color;
                let dot = paint(color, "2", "·");
                bar.println(format!(
                    "{}  Indexed {} files {} {} nodes {} {} edges {}",
                    paint(color, "32;1", self.inner.glyphs.success),
                    paint(color, "36;1", &HumanCount(files).to_string()),
                    dot,
                    paint(color, "32;1", &HumanCount(nodes).to_string()),
                    dot,
                    paint(color, "35;1", &HumanCount(edges).to_string()),
                    paint(color, "2", &format!("in {elapsed_secs:.1}s")),
                ));
            }
        } else {
            println!(
                "Indexed {} files, {} nodes, {} edges in {:.1}s",
                files, nodes, edges, elapsed_secs
            );
        }
        tracing::info!(indexed_files = files, nodes, edges, "indexed {files} files");
    }

    /// Finish the resolving phase: leave a permanent `✓ Resolved …` line and
    /// clear the now-idle bar.
    pub fn finish_resolve(&self, resolved: u64) {
        if self.inner.is_tty {
            if let Some(bar) = &self.inner.bar {
                let color = self.inner.color;
                bar.println(format!(
                    "{}  Resolved {} references",
                    paint(color, "32;1", self.inner.glyphs.success),
                    paint(color, "36;1", &HumanCount(resolved).to_string()),
                ));
                bar.finish_and_clear();
            }
        } else {
            println!("Resolved {} references", resolved);
        }
    }

    /// Abandon the bar (e.g. on error), leaving a clear marker on a TTY.
    pub fn abandon(&self) {
        if let Some(bar) = &self.inner.bar {
            if self.inner.is_tty {
                bar.abandon_with_message("aborted");
            } else {
                bar.finish_and_clear();
            }
        }
    }

    fn totals(&self) -> (u64, u64, u64) {
        let c = &self.inner.counters;
        (
            c.files.load(Ordering::Relaxed),
            c.nodes.load(Ordering::Relaxed),
            c.edges.load(Ordering::Relaxed),
        )
    }

    /// Build the bar's second line: running node/edge counts plus the file
    /// currently streaming by. Nodes green, edges magenta, path + separators dim.
    fn detail_line(&self, current_file: &str) -> String {
        let (_, n, e) = self.totals();
        let color = self.inner.color;
        let dot = paint(color, "2", "·");
        format!(
            "{} {} {} {} {}",
            paint(color, "32", &format!("{} nodes", HumanCount(n))),
            dot,
            paint(color, "35", &format!("{} edges", HumanCount(e))),
            dot,
            paint(color, "2", &shorten(current_file)),
        )
    }

    /// Rate-limit dynamic detail-line redraws to `draw_interval`.
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

/// Indeterminate spinner style for the scanning / parsing / resolving phases.
/// Bright-cyan spinner, bold-cyan phase label (the brand accent).
fn spinner_style(unicode: bool) -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan.bold} {prefix:.cyan.bold} {msg:.dim}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_chars(tick_chars(unicode))
}

/// Determinate two-line style for the indexing phase: cyan spinner + bold-cyan
/// label + a green bar that glides over a dim track + bold-green percent +
/// `pos / len files` on line 1, and the live node/edge counts + current file on
/// line 2 (colored by `detail_line`).
fn indexing_style(unicode: bool) -> ProgressStyle {
    // Unicode: full block + 7 fractional eighth-blocks for a smooth leading
    // edge, dim `░` track. ASCII: classic `[===> ---]`.
    let progress_chars = if unicode {
        "█▉▊▋▌▍▎▏░"
    } else {
        "=>-"
    };
    ProgressStyle::with_template(
        "{spinner:.cyan.bold} {prefix:.cyan.bold} [{bar:24.green/dim}] \
         {percent:>3.green.bold}%  {human_pos:.cyan} / {human_len:.dim} files\n   {msg}",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .tick_chars(tick_chars(unicode))
    .progress_chars(progress_chars)
}

fn tick_chars(unicode: bool) -> &'static str {
    if unicode {
        "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "
    } else {
        "-\\|/ "
    }
}

/// Wrap `body` in an ANSI SGR sequence when `color` is on, else return it bare.
fn paint(color: bool, sgr: &str, body: &str) -> String {
    if color {
        format!("\x1b[{sgr}m{body}\x1b[0m")
    } else {
        body.to_string()
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
    fn full_phase_sequence_runs_without_panic() {
        // Exercise the whole determinate-bar lifecycle in both modes: phase →
        // set_total → batches → finish_index → resolve phase → finish_resolve.
        for tty in [true, false] {
            let r = ProgressReporter::with_tty(tty);
            r.on_progress(&IndexProgress {
                phase: Phase::Parsing,
            });
            r.set_total(2);
            r.on_batch("src/a.rs", 5, 3);
            r.on_batch("src/b.rs", 4, 2);
            r.finish_index(2, 9, 5, 1.25);
            r.on_progress(&IndexProgress {
                phase: Phase::Resolving,
            });
            r.finish_resolve(7);
        }
    }

    #[test]
    fn set_total_zero_is_a_noop() {
        // Empty-repo path: no total, bar stays a spinner; must not divide by zero.
        let r = ProgressReporter::with_tty(true);
        r.set_total(0);
        r.abandon();
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
