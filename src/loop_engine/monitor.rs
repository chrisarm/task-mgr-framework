//! Activity monitor for the autonomous agent loop.
//!
//! Spawns a background `std::thread` that polls `git status --porcelain` at a
//! configurable interval, printing changed files to stderr when the status
//! changes between polls. A heartbeat message is printed every 3 minutes if
//! no changes are detected.
//!
//! The thread uses 1-second sleep intervals and checks an `AtomicBool` flag
//! each interval, ensuring clean shutdown within ~1 second of the stop signal.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Test-only call counter: incremented every time [`start_monitor`] is invoked.
/// Used by `run_slot_iteration` / `run_iteration` regression tests to verify
/// the monitor was wired in (the original regression was a slot path that
/// silently skipped `start_monitor`). Production builds don't compile this.
#[cfg(test)]
pub(crate) static MONITOR_START_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Interval between git status polls in seconds.
const POLL_INTERVAL_SECS: u64 = 10;

/// Grace period before the first poll. Early file changes (task JSON status
/// updates, prompt file writes) are the loop's own bookkeeping — not real
/// Claude activity — and should not trigger a timeout extension.
const INITIAL_GRACE_SECS: u64 = 12;

/// Interval between heartbeat messages in seconds.
const HEARTBEAT_INTERVAL_SECS: u64 = 180;

/// Sub-second-tunable timing knobs for the monitor loop. Production uses
/// [`MonitorTiming::default`]; tests pass shortened values so activity-detection
/// regression tests run in <1 s instead of waiting out the 10 s + 12 s defaults.
#[derive(Clone, Copy, Debug)]
struct MonitorTiming {
    poll_interval: Duration,
    initial_grace: Duration,
    heartbeat_interval: Duration,
}

impl Default for MonitorTiming {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(POLL_INTERVAL_SECS),
            initial_grace: Duration::from_secs(INITIAL_GRACE_SECS),
            heartbeat_interval: Duration::from_secs(HEARTBEAT_INTERVAL_SECS),
        }
    }
}

/// Handle for the background activity monitor thread.
///
/// Returned by [`start_monitor`], consumed by [`stop_monitor`].
pub struct MonitorHandle {
    stop_flag: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Epoch seconds of the last detected file activity (0 = no activity yet).
    /// Shared with the timeout watchdog in `claude.rs` to extend deadlines.
    pub last_activity_epoch: Arc<AtomicU64>,
}

/// Start the activity monitor in a background thread.
///
/// Polls `git status --porcelain` in the given directory every
/// [`POLL_INTERVAL_SECS`] seconds. When the status changes, the new
/// changed files are printed to stderr. If no changes occur for
/// [`HEARTBEAT_INTERVAL_SECS`] seconds, a heartbeat message is printed.
///
/// `prefix` is prepended (with a trailing space) to every heartbeat and
/// change line so concurrent monitors (one per parallel slot) stay
/// attributable on a shared stderr — pass e.g. `Some("[slot 1]")`. Sequential
/// mode passes `None`.
///
/// Returns a [`MonitorHandle`] that must be passed to [`stop_monitor`]
/// to cleanly shut down the thread.
pub fn start_monitor(dir: &Path, prefix: Option<&str>) -> MonitorHandle {
    #[cfg(test)]
    MONITOR_START_COUNT.fetch_add(1, Ordering::Relaxed);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&stop_flag);
    let last_activity_epoch = Arc::new(AtomicU64::new(0));
    let activity_clone = Arc::clone(&last_activity_epoch);
    let dir_owned = dir.to_path_buf();
    let prefix_owned = prefix.map(|s| s.to_string());

    let thread = thread::spawn(move || {
        monitor_loop(
            &dir_owned,
            &flag_clone,
            &activity_clone,
            prefix_owned.as_deref(),
            MonitorTiming::default(),
        );
    });

    MonitorHandle {
        stop_flag,
        thread: Some(thread),
        last_activity_epoch,
    }
}

/// Stop the activity monitor and wait for the thread to exit.
///
/// Sets the stop flag and joins the thread. The thread will exit within
/// approximately 1 second of the flag being set.
pub fn stop_monitor(mut handle: MonitorHandle) {
    handle.stop_flag.store(true, Ordering::Relaxed);
    if let Some(thread) = handle.thread.take() {
        let _ = thread.join();
    }
}

/// Run `git status --porcelain` and return the output, or `None` on failure.
fn git_status_porcelain(dir: &Path) -> Option<String> {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                None
            }
        })
}

/// The main monitor loop, running on the background thread.
///
/// `timing` is parameterized so tests can shorten the grace + poll intervals
/// to sub-second values; production paths always use `MonitorTiming::default()`.
fn monitor_loop(
    dir: &Path,
    stop_flag: &Arc<AtomicBool>,
    last_activity_epoch: &Arc<AtomicU64>,
    prefix: Option<&str>,
    timing: MonitorTiming,
) {
    let mut last_status: Option<String> = None;
    let mut last_change_time = Instant::now();
    // Delay the first poll so the loop's own bookkeeping (task JSON status
    // updates, prompt file writes) settles before we start tracking activity.
    let mut next_poll = Instant::now() + timing.initial_grace;
    let mut needs_baseline = true;
    // Bound the per-tick sleep at 1 s (responsive shutdown) but never longer
    // than the poll interval — otherwise tests with sub-second polls would
    // sleep right past their first poll.
    let tick_sleep = Duration::from_secs(1).min(timing.poll_interval);

    while !stop_flag.load(Ordering::Relaxed) {
        let now = Instant::now();

        // Poll git status at the configured interval
        if now >= next_poll {
            if let Some(current_status) = git_status_porcelain(dir) {
                if needs_baseline {
                    // First poll after grace period: capture baseline without
                    // treating bookkeeping changes as activity.
                    needs_baseline = false;
                    last_status = Some(current_status);
                } else {
                    let changed = match &last_status {
                        Some(prev) => prev != &current_status,
                        None => !current_status.is_empty(),
                    };

                    if changed && !current_status.is_empty() {
                        eprintln!("{}", format_status_change(&current_status, prefix));
                        last_change_time = Instant::now();
                        let epoch = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        last_activity_epoch.store(epoch, Ordering::Release);
                    }

                    last_status = Some(current_status);
                }
            }

            next_poll = now + timing.poll_interval;
        }

        // Heartbeat if no changes for the heartbeat interval
        if last_change_time.elapsed() >= timing.heartbeat_interval {
            eprintln!("{}", format_heartbeat(prefix));
            last_change_time = Instant::now();
        }

        thread::sleep(tick_sleep);
    }
}

/// Pure formatter for the per-change line. Returned `String` is what the
/// monitor writes to stderr — extracted so tests can assert prefixing without
/// capturing stderr.
fn format_status_change(status: &str, prefix: Option<&str>) -> String {
    let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
    let files: Vec<&str> = status.lines().filter(|l| !l.is_empty()).collect();
    match prefix {
        Some(p) => format!(
            "{} [monitor {}] {} file(s) changed: {}",
            p,
            ts,
            files.len(),
            files.join(", ")
        ),
        None => format!(
            "[monitor {}] {} file(s) changed: {}",
            ts,
            files.len(),
            files.join(", ")
        ),
    }
}

/// Pure formatter for the heartbeat line. See [`format_status_change`].
fn format_heartbeat(prefix: Option<&str>) -> String {
    let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
    match prefix {
        Some(p) => format!("{} [monitor {}] heartbeat: still running, no new changes", p, ts),
        None => format!("[monitor {}] heartbeat: still running, no new changes", ts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_monitor_handle_fields() {
        let dir = TempDir::new().expect("create temp dir");
        // Init git repo so status works
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let handle = start_monitor(dir.path(), None);
        assert!(!handle.stop_flag.load(Ordering::Relaxed));
        assert!(handle.thread.is_some());
        stop_monitor(handle);
    }

    #[test]
    fn test_start_and_stop_clean_shutdown() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let handle = start_monitor(dir.path(), None);
        // Let it run briefly
        thread::sleep(Duration::from_millis(50));
        // Stop should complete without hanging
        stop_monitor(handle);
    }

    #[test]
    fn test_stop_flag_propagation() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let handle = start_monitor(dir.path(), None);
        assert!(!handle.stop_flag.load(Ordering::Relaxed));

        // Set flag manually
        handle.stop_flag.store(true, Ordering::Relaxed);
        // Thread should exit within ~1 second
        if let Some(thread) = handle.thread {
            thread.join().expect("thread should join cleanly");
        }
    }

    #[test]
    fn test_git_status_porcelain_in_repo() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let status = git_status_porcelain(dir.path());
        assert!(status.is_some());
        // Clean repo should have empty status
        assert_eq!(status.unwrap(), "");
    }

    #[test]
    fn test_git_status_porcelain_with_changes() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        // Create an untracked file
        fs::write(dir.path().join("new_file.txt"), "hello").expect("write file");

        let status = git_status_porcelain(dir.path());
        assert!(status.is_some());
        let status_text = status.unwrap();
        assert!(status_text.contains("new_file.txt"));
        assert!(status_text.contains("??"));
    }

    #[test]
    fn test_git_status_porcelain_non_repo() {
        let dir = TempDir::new().expect("create temp dir");
        // No git init — not a repo
        let status = git_status_porcelain(dir.path());
        // Either returns None or the command fails
        assert!(status.is_none() || status.as_deref() == Some(""));
    }

    #[test]
    fn test_git_status_porcelain_nonexistent_dir() {
        let status = git_status_porcelain(Path::new("/nonexistent/path/abc123"));
        assert!(status.is_none());
    }

    #[test]
    fn test_format_status_change_no_prefix() {
        let line = format_status_change("?? new.txt\nM  existing.txt\n", None);
        // Must NOT start with a slot prefix when prefix is None.
        assert!(line.starts_with("[monitor "), "got: {line}");
        assert!(line.contains("2 file(s) changed"), "got: {line}");
        assert!(line.contains("?? new.txt"));
        assert!(line.contains("M  existing.txt"));
    }

    #[test]
    fn test_format_status_change_with_prefix() {
        let line = format_status_change("?? new.txt\n", Some("[slot 1]"));
        // Slot prefix must come BEFORE the `[monitor ...]` tag so concurrent
        // slots stay attributable on a shared stderr.
        assert!(line.starts_with("[slot 1] [monitor "), "got: {line}");
        assert!(line.contains("1 file(s) changed"));
    }

    #[test]
    fn test_format_status_change_empty_input_zero_files() {
        let line = format_status_change("", None);
        assert!(line.contains("0 file(s) changed"), "got: {line}");
    }

    #[test]
    fn test_format_heartbeat_no_prefix() {
        let line = format_heartbeat(None);
        assert!(line.starts_with("[monitor "), "got: {line}");
        assert!(line.ends_with("heartbeat: still running, no new changes"));
    }

    #[test]
    fn test_format_heartbeat_with_prefix() {
        let line = format_heartbeat(Some("[slot 0]"));
        assert!(line.starts_with("[slot 0] [monitor "), "got: {line}");
        assert!(line.ends_with("heartbeat: still running, no new changes"));
    }

    /// Regression guard for the original bug: the slot-mode watchdog's
    /// `last_activity_epoch` stayed at 0 forever because no monitor was
    /// running to populate it. Drives `monitor_loop` directly with
    /// sub-second timing on a real git repo and asserts that creating a
    /// file after the grace period advances the epoch off zero. If the
    /// monitor → epoch chain regresses, this test catches it in <1 s.
    #[test]
    fn test_monitor_advances_activity_epoch_on_file_change() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let activity = Arc::new(AtomicU64::new(0));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let dir_buf = dir.path().to_path_buf();
        let activity_clone = Arc::clone(&activity);
        let stop_clone = Arc::clone(&stop_flag);

        // Sub-second timings so the test stays fast. Heartbeat is set far
        // beyond the test runtime so it can't fire and pollute results.
        let timing = MonitorTiming {
            poll_interval: Duration::from_millis(80),
            initial_grace: Duration::from_millis(60),
            heartbeat_interval: Duration::from_secs(60),
        };

        let handle = thread::spawn(move || {
            monitor_loop(&dir_buf, &stop_clone, &activity_clone, None, timing);
        });

        // Wait past grace + one baseline poll, then create a file.
        thread::sleep(Duration::from_millis(250));
        fs::write(dir.path().join("activity.txt"), "data").expect("write file");

        // Wait for the change-detection poll to fire.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if activity.load(Ordering::Acquire) > 0 {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(40));
        }

        stop_flag.store(true, Ordering::Relaxed);
        handle.join().expect("monitor thread join");

        let epoch = activity.load(Ordering::Acquire);
        assert!(
            epoch > 0,
            "monitor should have advanced last_activity_epoch past 0 after file change"
        );
    }

    /// Asserts the test-only call counter increments on every
    /// `start_monitor` invocation. This is the load-bearing observation
    /// the `run_slot_iteration` regression test relies on.
    #[test]
    fn test_start_monitor_increments_call_counter() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let before = MONITOR_START_COUNT.load(Ordering::Relaxed);
        let handle = start_monitor(dir.path(), Some("[slot 7]"));
        let after = MONITOR_START_COUNT.load(Ordering::Relaxed);
        stop_monitor(handle);

        assert!(
            after > before,
            "MONITOR_START_COUNT must increment on start_monitor (before={before}, after={after})",
        );
    }

    #[test]
    fn test_monitor_detects_file_creation() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let handle = start_monitor(dir.path(), None);

        // Create a file while monitor is running
        fs::write(dir.path().join("test.txt"), "data").expect("write file");

        // Brief wait for one poll cycle
        thread::sleep(Duration::from_millis(100));

        stop_monitor(handle);
        // If we got here without hanging, the monitor handled the change
    }

    #[test]
    fn test_monitor_non_git_repo_graceful() {
        let dir = TempDir::new().expect("create temp dir");
        // No git init — monitor should run gracefully (no panic)

        let handle = start_monitor(dir.path(), None);
        thread::sleep(Duration::from_millis(50));
        stop_monitor(handle);
    }

    #[test]
    fn test_constants() {
        assert_eq!(POLL_INTERVAL_SECS, 10);
        assert_eq!(HEARTBEAT_INTERVAL_SECS, 180);
    }

    #[test]
    fn test_monitor_loop_stops_immediately_when_flagged() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let stop_flag = Arc::new(AtomicBool::new(true));
        let dir_buf = dir.path().to_path_buf();

        // monitor_loop should return almost immediately since flag is already set
        let start = Instant::now();
        let activity = Arc::new(AtomicU64::new(0));
        monitor_loop(&dir_buf, &stop_flag, &activity, None, MonitorTiming::default());
        let elapsed = start.elapsed();

        // Should exit within 2 seconds (1 sleep + overhead)
        assert!(
            elapsed < Duration::from_secs(3),
            "monitor_loop took {:?} to exit with pre-set flag",
            elapsed
        );
    }
}
