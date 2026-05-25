#![allow(dead_code)]
// T-440 — Shimmer progress bar using indicatif.
// Replaces the TS shimmer worker-thread approach with an indicatif ProgressBar
// running on a background tokio task (spawned from the calling async context).
//
// Acceptance (T-440): `codewiki index --path <dir>` exits 0 within 120s;
// final stderr line contains "indexed N files" when CODEWIKI_LOG=info.

use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// Phases of the indexing pipeline shown in the progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Scanning,
    Parsing,
    Storing,
    Resolving,
    Done,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Scanning => "Scanning files",
            Phase::Parsing => "Parsing",
            Phase::Storing => "Storing",
            Phase::Resolving => "Resolving references",
            Phase::Done => "Done",
        }
    }
}

/// A progress update posted by the extraction pipeline.
#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub phase: Phase,
    pub percent: u8,
    pub count: u64,
}

/// An animated progress bar for the indexing pipeline.
pub struct ShimmerProgress {
    bar: ProgressBar,
}

impl ShimmerProgress {
    /// Create a new progress bar writing to stderr.
    pub fn new() -> Self {
        let bar = ProgressBar::new(100);
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg} [{bar:40.cyan/blue}] {pos}%")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=>-"),
        );
        bar.enable_steady_tick(Duration::from_millis(80));
        ShimmerProgress { bar }
    }

    /// Update the bar with the latest progress.
    pub fn on_progress(&self, progress: &IndexProgress) {
        self.bar.set_position(progress.percent as u64);
        self.bar.set_message(format!(
            "{} ({} files)",
            progress.phase.label(),
            progress.count
        ));
    }

    /// Finish the bar with a summary message.
    pub fn finish(&self, total_files: u64) {
        self.bar
            .finish_with_message(format!("indexed {} files", total_files));
        tracing::info!(indexed_files = total_files, "indexed {} files", total_files);
    }

    /// Abandon the bar (e.g. on error).
    pub fn abandon(&self) {
        self.bar.abandon_with_message("aborted");
    }
}

impl Default for ShimmerProgress {
    fn default() -> Self {
        Self::new()
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
            Phase::Done,
        ] {
            assert!(!phase.label().is_empty());
        }
    }

    #[test]
    fn shimmer_creates_without_panic() {
        // Just verify the ProgressBar is created successfully (no TTY required).
        let _bar = ShimmerProgress::new();
    }
}
