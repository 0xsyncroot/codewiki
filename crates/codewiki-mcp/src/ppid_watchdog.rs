//! T-404 / T-405 — PPID watchdog: auto-shutdown when the parent process exits.
//!
//! On Linux/macOS, `SIGKILL` on the parent does not propagate to children
//! and `stdin.on('end')` may never fire. This module polls `getppid()` every
//! `poll_ms` milliseconds; if the PPID has changed AND the original host
//! process is no longer alive, it initiates a graceful shutdown.
//!
//! POSIX only: on Windows the watchdog is a no-op (the OS propagates
//! `CTRL_BREAK` events through the job object, so polling is unnecessary).
//!
//! T-405: at the start of `serve --mcp` we ignore SIGPIPE so a write to a
//! closed stdout doesn't terminate the process with an unhandled signal.
//! This is handled in the [`ignore_sigpipe`] function below.

use tokio_util::sync::CancellationToken;

/// Ignore SIGPIPE so that writing to a closed stdout/pipe doesn't kill the
/// MCP server process immediately.
///
/// On Unix, the default action for SIGPIPE is to terminate the process.
/// MCP hosts may close their read end of the stdio pipe while the server
/// is still writing (e.g. during a fast restart), which would otherwise
/// cause an unexpected termination.
///
/// This function is a no-op on Windows.
///
/// # Safety
/// Calls `libc::signal` which is an unsafe FFI call. The call is safe in
/// practice because we are only ignoring SIGPIPE, which has well-defined
/// POSIX semantics.
#[cfg(unix)]
pub fn ignore_sigpipe() {
    // SAFETY: signal(SIGPIPE, SIG_IGN) is a standard POSIX pattern. The only
    // risk is race conditions with multi-threaded signal masking, but calling
    // this before spawning any threads (at MCP serve startup) avoids that.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

#[cfg(not(unix))]
pub fn ignore_sigpipe() {
    // No-op on Windows.
}

/// Spawn a background task that polls for parent-process liveness.
///
/// If the recorded `original_ppid` diverges from the actual current parent PID
/// (or the host process at `host_ppid` is no longer alive), the
/// `cancel` token is cancelled, which should propagate shutdown to the server.
///
/// Set `poll_ms = 0` (via `CODEWIKI_PPID_POLL_MS=0`) to disable the watchdog.
#[allow(unused_variables)]
pub fn spawn_ppid_watchdog(
    original_ppid: u32,
    host_ppid: Option<u32>,
    poll_ms: u64,
    cancel: CancellationToken,
) {
    if poll_ms == 0 {
        tracing::debug!("PPID watchdog disabled (poll_ms=0)");
        return;
    }

    #[cfg(unix)]
    {
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(poll_ms));
            // The first tick fires immediately; discard it.
            interval.tick().await;

            loop {
                interval.tick().await;

                let ppid_changed = current_ppid() != original_ppid;
                let host_gone = host_ppid
                    .map(|pid| !is_process_alive(pid))
                    .unwrap_or(false);

                if ppid_changed || host_gone {
                    tracing::warn!(
                        original_ppid,
                        current = current_ppid(),
                        ?host_ppid,
                        "[CodeWiki MCP] Parent exited; shutting down."
                    );
                    cancel_clone.cancel();
                    break;
                }
            }
        });
    }

    #[cfg(not(unix))]
    {
        tracing::debug!("PPID watchdog is POSIX-only; skipping on this platform.");
    }
}

/// Return the current parent process ID on Unix.
#[cfg(unix)]
fn current_ppid() -> u32 {
    // SAFETY: getppid() is always safe to call.
    (unsafe { libc::getppid() }) as u32
}

/// Check whether a process with the given PID is alive on Unix.
///
/// Uses `kill(pid, 0)`: if the call succeeds (returns 0) the process exists.
/// EPERM means it exists but we lack permission to signal it — still alive.
/// ESRCH means no such process.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is a standard liveness probe; no signal is sent.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_sigpipe_does_not_panic() {
        // Simply ensure the function runs without panicking.
        ignore_sigpipe();
    }

    #[cfg(unix)]
    #[test]
    fn current_process_is_alive() {
        let pid = std::process::id();
        assert!(is_process_alive(pid));
    }

    #[cfg(unix)]
    #[test]
    fn nonexistent_pid_is_not_alive() {
        // PID 0 is reserved and should not be a normal process we spawned.
        // We use a very large fake PID unlikely to exist.
        // Note: this test is best-effort; on some systems pid may wrap around.
        assert!(!is_process_alive(u32::MAX - 1));
    }
}
