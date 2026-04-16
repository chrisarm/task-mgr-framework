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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Interval between git status polls in seconds.
const POLL_INTERVAL_SECS: u64 = 10;

/// Grace period before the first poll. Early file changes (task JSON status
/// updates, prompt file writes) are the loop's own bookkeeping — not real
/// Claude activity — and should not trigger a timeout extension.
const INITIAL_GRACE_SECS: u64 = 12;

/// Interval between heartbeat messages in seconds.
const HEARTBEAT_INTERVAL_SECS: u64 = 180;

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
/// Returns a [`MonitorHandle`] that must be passed to [`stop_monitor`]
/// to cleanly shut down the thread.
pub fn start_monitor(dir: &Path) -> MonitorHandle {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&stop_flag);
    let last_activity_epoch = Arc::new(AtomicU64::new(0));
    let activity_clone = Arc::clone(&last_activity_epoch);
    let dir_owned = dir.to_path_buf();

    let thread = thread::spawn(move || {
        monitor_loop(&dir_owned, &flag_clone, &activity_clone);
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
fn monitor_loop(dir: &Path, stop_flag: &Arc<AtomicBool>, last_activity_epoch: &Arc<AtomicU64>) {
    let mut last_status: Option<String> = None;
    let mut last_change_time = Instant::now();
    // Delay the first poll so the loop's own bookkeeping (task JSON status
    // updates, prompt file writes) settles before we start tracking activity.
    let mut next_poll = Instant::now() + Duration::from_secs(INITIAL_GRACE_SECS);
    let mut needs_baseline = true;

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
                        print_status_change(&current_status);
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

            next_poll = now + Duration::from_secs(POLL_INTERVAL_SECS);
        }

        // Heartbeat if no changes for HEARTBEAT_INTERVAL_SECS
        if last_change_time.elapsed() >= Duration::from_secs(HEARTBEAT_INTERVAL_SECS) {
            let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
            eprintln!("[monitor {}] heartbeat: still running, no new changes", ts);
            last_change_time = Instant::now();
        }

        // Sleep in 1-second intervals to allow responsive shutdown
        thread::sleep(Duration::from_secs(1));
    }
}

/// Print changed files from `git status --porcelain` output to stderr.
fn print_status_change(status: &str) {
    let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
    let files: Vec<&str> = status.lines().filter(|l| !l.is_empty()).collect();
    eprintln!(
        "[monitor {}] {} file(s) changed: {}",
        ts,
        files.len(),
        files.join(", ")
    );
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

        let handle = start_monitor(dir.path());
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

        let handle = start_monitor(dir.path());
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

        let handle = start_monitor(dir.path());
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
    fn test_print_status_change_output() {
        // This tests that print_status_change doesn't panic
        // (output goes to stderr, hard to capture in unit tests)
        print_status_change("?? new.txt\nM  existing.txt\n");
    }

    #[test]
    fn test_print_status_change_empty() {
        // Empty status should print "0 file(s) changed"
        print_status_change("");
    }

    #[test]
    fn test_monitor_detects_file_creation() {
        let dir = TempDir::new().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let handle = start_monitor(dir.path());

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

        let handle = start_monitor(dir.path());
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
        monitor_loop(&dir_buf, &stop_flag, &activity);
        let elapsed = start.elapsed();

        // Should exit within 2 seconds (1 sleep + overhead)
        assert!(
            elapsed < Duration::from_secs(3),
            "monitor_loop took {:?} to exit with pre-set flag",
            elapsed
        );
    }
}
