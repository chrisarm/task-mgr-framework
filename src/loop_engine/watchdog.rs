//! Subprocess timeout monitoring and process kill logic.
//!
//! Provides `TimeoutConfig` for per-iteration time limits with activity-based
//! extensions, `watchdog_loop` for polling signal flags and enforcing timeouts,
//! and `kill_process_group` for SIGTERM → grace → SIGKILL termination on Unix.

use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::loop_engine::signals::SignalFlag;

/// Timeout constants for per-iteration time limits.
const TIMEOUT_LOW_SECS: u64 = 20 * 60;
const TIMEOUT_MEDIUM_SECS: u64 = 30 * 60;
const TIMEOUT_HIGH_SECS: u64 = 40 * 60;
pub(crate) const INITIAL_EXTENSION_SECS: u64 = 7 * 60;
pub(crate) const EXTENSION_DECREMENT_SECS: u64 = 60;

/// Seconds to keep the child alive after the current task's `<completed>` tag
/// is seen in the stream, before force-terminating.
///
/// Gives the agent a bounded window to flush straggling text, commit/push a
/// final change, and emit any additional `<completed>` tags for other tasks
/// it finished en route. Any output in this window is captured in the stream
/// buffer and processed by the engine's post-process path like normal.
pub(crate) const POST_COMPLETION_GRACE_SECS: u64 = 180;

/// Configuration for per-iteration timeout with activity-based extensions.
#[derive(Clone)]
pub(crate) struct TimeoutConfig {
    /// Maximum time allowed for the iteration.
    pub(crate) base_timeout: Duration,
    /// First activity extension amount (decreases by `EXTENSION_DECREMENT_SECS` each use).
    pub(crate) initial_extension: Duration,
    /// Shared epoch timestamp of last file activity from the monitor thread.
    pub(crate) last_activity_epoch: Arc<AtomicU64>,
}

impl TimeoutConfig {
    /// Create a `TimeoutConfig` from a task difficulty string.
    ///
    /// Maps: `"low"` → 20min, `"high"` → 40min, anything else (including `None`) → 30min.
    pub(crate) fn from_difficulty(
        difficulty: Option<&str>,
        last_activity_epoch: Arc<AtomicU64>,
    ) -> Self {
        let base_secs = match difficulty.map(|d| d.to_ascii_lowercase()).as_deref() {
            Some("low") => TIMEOUT_LOW_SECS,
            Some("high") => TIMEOUT_HIGH_SECS,
            _ => TIMEOUT_MEDIUM_SECS,
        };
        TimeoutConfig {
            base_timeout: Duration::from_secs(base_secs),
            initial_extension: Duration::from_secs(INITIAL_EXTENSION_SECS),
            last_activity_epoch,
        }
    }
}

/// Extract exit code from process status, using 128+signal convention on Unix.
pub(crate) fn exit_code_from_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }

    // Process was killed by a signal (code() returns None)
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }

    1 // fallback
}

/// Send SIGTERM → wait grace period → SIGKILL to a process group.
///
/// Returns once the process group is dead or force-killed.
/// `stop` is checked during the grace period to detect early reaping by the main thread.
#[cfg(unix)]
pub(crate) fn kill_process_group(child_pid: u32, stop: &AtomicBool, reason: &str) {
    const GRACE_PERIOD: Duration = Duration::from_secs(3);
    const GRACE_POLL: Duration = Duration::from_millis(100);

    let pgid = -(child_pid as i32);

    eprintln!(
        "\n{}, terminating Claude process group (pgid {})...",
        reason, child_pid
    );

    let ret = unsafe { libc::kill(pgid, libc::SIGTERM) };
    if ret == -1 {
        return; // ESRCH: already exited
    }

    let start = Instant::now();
    while start.elapsed() < GRACE_PERIOD {
        std::thread::sleep(GRACE_POLL);
        if stop.load(Ordering::Acquire) {
            return; // Main thread reaped
        }
        let ret = unsafe { libc::kill(pgid, 0) };
        if ret == -1 {
            return; // Exited
        }
    }

    eprintln!(
        "Grace period expired, sending SIGKILL to process group {}...",
        child_pid
    );
    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
}

/// Watchdog loop: polls signal flag and timeout, terminates child on either.
///
/// Runs on a dedicated OS thread (not tokio) so it works even when the
/// main thread is blocked in synchronous I/O.
#[cfg(unix)]
pub(crate) fn watchdog_loop(
    child_pid: u32,
    signal_flag: Option<&SignalFlag>,
    stop: &AtomicBool,
    timeout: Option<&TimeoutConfig>,
    timed_out: &AtomicBool,
    completion_epoch: Option<&AtomicU64>,
    target_task_id: Option<&str>,
    completion_killed: Option<&AtomicBool>,
) {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    // Timeout tracking
    let mut deadline = timeout.map(|t| Instant::now() + t.base_timeout);
    let mut last_seen_activity: u64 = 0;
    let mut extensions_used: u32 = 0;

    while !stop.load(Ordering::Acquire) {
        // Check signal
        if let Some(flag) = signal_flag
            && flag.is_signaled()
        {
            kill_process_group(child_pid, stop, "Signal received");
            return;
        }

        // Check post-completion grace: the tee loop sets completion_epoch
        // when it sees `<completed>CURRENT_TASK</completed>` in the stream.
        // After POST_COMPLETION_GRACE_SECS elapse, force-exit — this is a
        // successful completion, not a timeout, so timed_out stays false.
        if let Some(epoch) = completion_epoch {
            let set_at = epoch.load(Ordering::Acquire);
            if set_at > 0 {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if now.saturating_sub(set_at) >= POST_COMPLETION_GRACE_SECS {
                    eprintln!(
                        "[completion] grace window ({}s) elapsed after <completed>{}</completed> — terminating Claude process group",
                        POST_COMPLETION_GRACE_SECS,
                        target_task_id.unwrap_or("?"),
                    );
                    // Set BEFORE kill so the SIGTERM-induced wait() in
                    // spawn_claude observes the flag. This distinguishes
                    // an internal grace kill from an external Ctrl+C that
                    // hit the child via the terminal foreground group.
                    if let Some(flag) = completion_killed {
                        flag.store(true, Ordering::Release);
                    }
                    kill_process_group(child_pid, stop, "Post-completion grace expired");
                    return;
                }
            }
        }

        // Check timeout
        if let (Some(dl), Some(tc)) = (&mut deadline, timeout) {
            // Check for new activity
            let current_activity = tc.last_activity_epoch.load(Ordering::Acquire);
            if current_activity > last_seen_activity {
                last_seen_activity = current_activity;
                let ext_secs = tc
                    .initial_extension
                    .as_secs()
                    .saturating_sub(extensions_used as u64 * EXTENSION_DECREMENT_SECS);
                if ext_secs > 0 {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    let extended = remaining + Duration::from_secs(ext_secs);
                    let capped = extended.min(tc.base_timeout);
                    *dl = Instant::now() + capped;
                    extensions_used += 1;
                    eprintln!(
                        "[timeout] Activity detected, extended deadline by {}s ({} remaining, {} extensions used)",
                        ext_secs,
                        capped.as_secs(),
                        extensions_used
                    );
                }
            }

            if Instant::now() >= *dl {
                eprintln!(
                    "[timeout] Iteration exceeded {}s timeout",
                    tc.base_timeout.as_secs()
                );
                timed_out.store(true, Ordering::Release);
                kill_process_group(child_pid, stop, "Timeout exceeded");
                return;
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(not(unix))]
pub(crate) fn watchdog_loop(
    _child_pid: u32,
    signal_flag: Option<&SignalFlag>,
    stop: &AtomicBool,
    _timeout: Option<&TimeoutConfig>,
    _timed_out: &AtomicBool,
    _completion_epoch: Option<&AtomicU64>,
    _target_task_id: Option<&str>,
    _completion_killed: Option<&AtomicBool>,
) {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    while !stop.load(Ordering::Acquire) {
        if let Some(flag) = signal_flag {
            if flag.is_signaled() {
                eprintln!("\nSignal received, Claude subprocess will be terminated...");
                return;
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // --- exit_code_from_status tests ---

    #[test]
    fn test_exit_code_from_normal_exit() {
        let status = Command::new("true").status().unwrap();
        assert_eq!(exit_code_from_status(status), 0);

        let status = Command::new("false").status().unwrap();
        assert_eq!(exit_code_from_status(status), 1);
    }

    #[cfg(unix)]
    #[test]
    fn test_exit_code_from_signal_killed_process() {
        // Spawn a sleep, then kill it with SIGKILL
        let mut child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let status = child.wait().unwrap();
        let code = exit_code_from_status(status);
        assert_eq!(code, 128 + libc::SIGKILL, "Should be 128 + SIGKILL (137)");
    }

    // --- TimeoutConfig tests ---

    #[test]
    fn test_timeout_config_from_difficulty() {
        let activity = Arc::new(AtomicU64::new(0));
        let low = TimeoutConfig::from_difficulty(Some("low"), Arc::clone(&activity));
        assert_eq!(low.base_timeout, Duration::from_secs(20 * 60));

        let med = TimeoutConfig::from_difficulty(None, Arc::clone(&activity));
        assert_eq!(med.base_timeout, Duration::from_secs(30 * 60));

        let med2 = TimeoutConfig::from_difficulty(Some("medium"), Arc::clone(&activity));
        assert_eq!(med2.base_timeout, Duration::from_secs(30 * 60));

        let high = TimeoutConfig::from_difficulty(Some("high"), Arc::clone(&activity));
        assert_eq!(high.base_timeout, Duration::from_secs(40 * 60));

        // Case insensitive
        let high2 = TimeoutConfig::from_difficulty(Some("HIGH"), Arc::clone(&activity));
        assert_eq!(high2.base_timeout, Duration::from_secs(40 * 60));
    }

    #[test]
    fn test_timeout_extension_decreases() {
        // Verify the extension formula: 7min, 6min, 5min, ..., 0
        for i in 0u32..10 {
            let ext = INITIAL_EXTENSION_SECS.saturating_sub(i as u64 * EXTENSION_DECREMENT_SECS);
            let expected = match i {
                0 => 7 * 60,
                1 => 6 * 60,
                2 => 5 * 60,
                3 => 4 * 60,
                4 => 3 * 60,
                5 => 2 * 60,
                6 => 60,
                _ => 0,
            };
            assert_eq!(ext, expected, "Extension #{} should be {}s", i, expected);
        }
    }

    // --- Timeout integration tests (require unix process control) ---

    /// Helper: create a script that sleeps for 120s.
    fn create_sleep_script(dir: &std::path::Path) -> String {
        let script_path = dir.join("fake_claude.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 120\n").unwrap();
        std::fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        script_path.to_str().unwrap().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn test_timeout_kills_long_running_process() {
        use std::sync::Mutex;
        static ENV_MUTEX: Mutex<()> = Mutex::new(());

        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let script = create_sleep_script(tmp.path());
        unsafe { std::env::set_var("CLAUDE_BINARY", &script) };
        let activity = Arc::new(AtomicU64::new(0));
        let timeout = TimeoutConfig {
            base_timeout: Duration::from_secs(2),
            initial_extension: Duration::from_secs(INITIAL_EXTENSION_SECS),
            last_activity_epoch: activity,
        };

        let start = Instant::now();
        let result = crate::loop_engine::claude::spawn_claude(
            "ignored",
            &crate::loop_engine::config::PermissionMode::Dangerous,
            crate::loop_engine::claude::SpawnOpts {
                timeout: Some(timeout),
                ..Default::default()
            },
        );
        let elapsed = start.elapsed();

        unsafe { std::env::remove_var("CLAUDE_BINARY") };

        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(res.timed_out, "Process should be marked as timed out");
        assert_ne!(res.exit_code, 0);
        assert!(
            elapsed.as_secs() < 10,
            "Should timeout quickly, took {:?}",
            elapsed
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_timeout_extends_on_activity() {
        use std::sync::Mutex;
        use std::time::{SystemTime, UNIX_EPOCH};
        static ENV_MUTEX: Mutex<()> = Mutex::new(());

        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let script = create_sleep_script(tmp.path());
        unsafe { std::env::set_var("CLAUDE_BINARY", &script) };
        let activity = Arc::new(AtomicU64::new(0));
        let activity_clone = Arc::clone(&activity);
        let timeout = TimeoutConfig {
            base_timeout: Duration::from_secs(3),
            initial_extension: Duration::from_secs(INITIAL_EXTENSION_SECS),
            last_activity_epoch: activity,
        };

        // Simulate activity at 2s to extend the deadline
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            let epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            activity_clone.store(epoch, Ordering::Release);
        });

        let start = Instant::now();
        let result = crate::loop_engine::claude::spawn_claude(
            "ignored",
            &crate::loop_engine::config::PermissionMode::Dangerous,
            crate::loop_engine::claude::SpawnOpts {
                timeout: Some(timeout),
                ..Default::default()
            },
        );
        let elapsed = start.elapsed();

        unsafe { std::env::remove_var("CLAUDE_BINARY") };

        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(res.timed_out, "Should eventually time out");
        // With 3s base + activity at 2s extending by min(1+7*60, 3)=3s → deadline at ~5s
        assert!(
            elapsed.as_secs() >= 4,
            "Activity should have extended lifetime past 3s, took {:?}",
            elapsed
        );
    }

    /// Pre-arm `completion_epoch` to a time already past the grace window and
    /// verify the watchdog kills the child promptly without setting
    /// `timed_out` (grace kill = success, not timeout).
    #[cfg(unix)]
    #[test]
    fn test_watchdog_grace_kills_after_completion_armed() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{SystemTime, UNIX_EPOCH};

        let mut child = Command::new("sleep")
            .arg("120")
            .process_group(0) // own pgroup so kill_process_group's -pid targets it
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        let epoch = Arc::new(AtomicU64::new(0));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Store grace-already-expired: watchdog should fire on its first poll.
        epoch.store(
            now.saturating_sub(POST_COMPLETION_GRACE_SECS + 1),
            Ordering::Release,
        );

        let stop = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        let completion_killed = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let timed_out_clone = Arc::clone(&timed_out);
        let epoch_clone = Arc::clone(&epoch);
        let completion_killed_clone = Arc::clone(&completion_killed);
        let handle = std::thread::spawn(move || {
            watchdog_loop(
                pid,
                None,
                &stop_clone,
                None, // no base timeout — we're isolating the grace branch
                &timed_out_clone,
                Some(&epoch_clone),
                Some("T-GRACE-TEST"),
                Some(&completion_killed_clone),
            );
        });

        let wait_start = Instant::now();
        let status = child.wait().expect("child wait");
        let wait_elapsed = wait_start.elapsed();

        stop.store(true, Ordering::Release);
        handle.join().expect("watchdog thread join");

        assert!(!status.success(), "grace kill must terminate the child");
        assert!(
            wait_elapsed < Duration::from_secs(5),
            "grace kill should fire promptly after the grace window elapses, took {:?}",
            wait_elapsed
        );
        assert!(
            !timed_out.load(Ordering::Acquire),
            "grace kill is a successful completion — timed_out must stay false",
        );
        assert!(
            completion_killed.load(Ordering::Acquire),
            "grace kill must set completion_killed so the engine can skip signal propagation",
        );
    }

    /// When `completion_epoch` is never armed, the grace branch must be inert
    /// — nothing should be killed on that basis, only via timeout/signal.
    #[cfg(unix)]
    #[test]
    fn test_watchdog_grace_not_armed_does_not_kill() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut child = Command::new("sleep")
            .arg("10")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        let epoch = Arc::new(AtomicU64::new(0)); // unarmed
        let stop = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        let completion_killed = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let timed_out_clone = Arc::clone(&timed_out);
        let epoch_clone = Arc::clone(&epoch);
        let completion_killed_clone = Arc::clone(&completion_killed);
        let handle = std::thread::spawn(move || {
            watchdog_loop(
                pid,
                None,
                &stop_clone,
                None,
                &timed_out_clone,
                Some(&epoch_clone),
                Some("T-UNARMED"),
                Some(&completion_killed_clone),
            );
        });

        // Let the watchdog poll a few times, confirm child still alive.
        std::thread::sleep(Duration::from_millis(800));
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;

        // Clean up: stop watchdog, kill child ourselves.
        stop.store(true, Ordering::Release);
        handle.join().expect("watchdog thread join");
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        let _ = child.wait();

        assert!(
            alive,
            "unarmed grace must not kill the child; kill(0) on pid should succeed"
        );
        assert!(
            !timed_out.load(Ordering::Acquire),
            "no timeout configured, no grace armed — timed_out must stay false"
        );
        assert!(
            !completion_killed.load(Ordering::Acquire),
            "no grace armed — completion_killed must stay false"
        );
    }
}
