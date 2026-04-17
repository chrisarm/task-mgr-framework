/// Signal handling for the autonomous agent loop.
///
/// Supports two signal mechanisms:
/// 1. **File-based signals**: `.stop` and `.pause` files in the tasks directory
/// 2. **UNIX signals**: SIGINT (Ctrl+C) and SIGTERM via `Arc<AtomicBool>`
///
/// Session guidance accumulation lives in [`super::guidance`].
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::guidance::SessionGuidance;
use super::{DEADLINE_FILE_PREFIX, PAUSE_FILE, STOP_FILE};

/// Check if a stop signal exists for the given session.
///
/// When `prefix` is `Some(p)`, checks `.stop-{p}` first (fast path), then falls back
/// to the global `.stop` file. When `prefix` is `None`, checks only `.stop`.
pub fn check_stop_signal(tasks_dir: &Path, prefix: Option<&str>) -> bool {
    if let Some(p) = prefix
        && tasks_dir.join(format!("{STOP_FILE}-{p}")).exists()
    {
        return true;
    }
    tasks_dir.join(STOP_FILE).exists()
}

/// Check if a pause signal exists for the given session.
///
/// When `prefix` is `Some(p)`, checks `.pause-{p}` first (fast path), then falls back
/// to the global `.pause` file. When `prefix` is `None`, checks only `.pause`.
pub fn check_pause_signal(tasks_dir: &Path, prefix: Option<&str>) -> bool {
    if let Some(p) = prefix
        && tasks_dir.join(format!("{PAUSE_FILE}-{p}")).exists()
    {
        return true;
    }
    tasks_dir.join(PAUSE_FILE).exists()
}

/// Clean up signal files for a specific session prefix.
///
/// When `prefix` is `Some(p)`: removes `.stop-{p}` and `.pause-{p}`, and also
/// removes the global `.stop`/`.pause` if present (since the engine's
/// `check_stop_signal` falls back to global files, they must be cleaned up
/// too — otherwise the stop signal persists across subsequent runs).
/// When `prefix` is `None`: removes global `.stop` and `.pause`.
pub fn cleanup_signal_files_for_prefix(tasks_dir: &Path, prefix: Option<&str>) {
    let mut files_to_remove = vec![tasks_dir.join(STOP_FILE), tasks_dir.join(PAUSE_FILE)];
    if let Some(p) = prefix {
        files_to_remove.push(tasks_dir.join(format!("{STOP_FILE}-{p}")));
        files_to_remove.push(tasks_dir.join(format!("{PAUSE_FILE}-{p}")));
    }
    for path in &files_to_remove {
        if path.exists()
            && let Err(e) = fs::remove_file(path)
        {
            eprintln!("Warning: could not remove {}: {}", path.display(), e);
        }
    }
}

/// Handle a pause signal: display banner, read multi-line stdin, accumulate guidance.
///
/// Reads lines from stdin until an empty line is entered. The collected text
/// is added to `session_guidance` with the current iteration tag. The `.pause`
/// file is deleted after the interaction.
///
/// Returns `true` if guidance was provided, `false` if user just resumed.
pub fn handle_pause(
    tasks_dir: &Path,
    iteration: u32,
    session_guidance: &mut SessionGuidance,
    prefix: Option<&str>,
) -> bool {
    eprintln!("\n╔══════════════════════════════════════════╗");
    eprintln!("║          PAUSED (iteration {:<4})         ║", iteration);
    eprintln!("╠══════════════════════════════════════════╣");
    eprintln!("║  Enter guidance (empty line to resume):  ║");
    eprintln!("╚══════════════════════════════════════════╝\n");

    let lines = read_lines_with_timeout(io::BufReader::new(io::stdin()), None);
    let _ = fs::remove_file(pause_file_path(tasks_dir, prefix));

    let text = lines.join("\n");
    let has_guidance = !text.trim().is_empty();

    if has_guidance {
        eprintln!("Guidance recorded. Resuming...\n");
        session_guidance.add(iteration, text);
    } else {
        eprintln!("Resuming without guidance...\n");
    }

    has_guidance
}

/// Shared signal flag for SIGINT/SIGTERM detection.
///
/// Use `setup_signal_handler()` to install the async handler, then check
/// `is_signaled()` at iteration boundaries.
#[derive(Clone)]
pub struct SignalFlag {
    flag: Arc<AtomicBool>,
}

impl SignalFlag {
    /// Create a new signal flag (initially false).
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Check if a signal has been received.
    pub fn is_signaled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// Set the signal flag (called by signal handler).
    pub fn set(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Get a clone of the inner Arc for use in async handlers.
    pub fn inner(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

impl Default for SignalFlag {
    fn default() -> Self {
        Self::new()
    }
}

/// Clean up signal files and deadline files from the tasks directory.
///
/// Removes: `.stop`, `.pause`, and any `.deadline-*` files.
/// Errors are logged but don't propagate — cleanup should never crash the loop.
pub fn cleanup_signal_files(tasks_dir: &Path) {
    // Remove specific signal files
    for filename in &[STOP_FILE, PAUSE_FILE] {
        let path = tasks_dir.join(filename);
        if path.exists()
            && let Err(e) = fs::remove_file(&path)
        {
            eprintln!("Warning: could not remove {}: {}", path.display(), e);
        }
    }

    // Remove .deadline-* files
    cleanup_deadline_files(tasks_dir);
}

/// Remove `.deadline-*` files from the tasks directory.
fn cleanup_deadline_files(tasks_dir: &Path) {
    let entries = match fs::read_dir(tasks_dir) {
        Ok(entries) => entries,
        Err(_) => return, // Can't read dir, skip
    };

    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && name.starts_with(DEADLINE_FILE_PREFIX)
            && let Err(e) = fs::remove_file(entry.path())
        {
            eprintln!(
                "Warning: could not remove deadline file {}: {}",
                entry.path().display(),
                e
            );
        }
    }
}

/// Get the path where a stop file should be created.
///
/// When `prefix` is `Some(p)`, returns the session-specific `.stop-{p}` path.
/// When `prefix` is `None`, returns the global `.stop` path.
pub fn stop_file_path(tasks_dir: &Path, prefix: Option<&str>) -> PathBuf {
    match prefix {
        Some(p) => tasks_dir.join(format!("{STOP_FILE}-{p}")),
        None => tasks_dir.join(STOP_FILE),
    }
}

/// Get the path where a pause file should be created.
///
/// When `prefix` is `Some(p)`, returns the session-specific `.pause-{p}` path.
/// When `prefix` is `None`, returns the global `.pause` path.
pub fn pause_file_path(tasks_dir: &Path, prefix: Option<&str>) -> PathBuf {
    match prefix {
        Some(p) => tasks_dir.join(format!("{PAUSE_FILE}-{p}")),
        None => tasks_dir.join(PAUSE_FILE),
    }
}

/// Handle a human review checkpoint after a `requires_human` task completes.
///
/// Displays a banner with `task_id`, `task_title`, and optional `task_notes`,
/// then reads multi-line input from `reader` until an empty line or EOF.
/// Guidance is tagged as `[Human Review for {task_id}] {input}` and added to
/// `session_guidance`.
///
/// Returns `true` if guidance was provided, `false` if the user skipped or
/// input was EOF.
///
/// `timeout_secs: None` or `Some(0)` means a blocking read (no timeout).
/// `timeout_secs: Some(n)` where `n > 0` means return `false` after `n` seconds
/// without input.
///
/// # Panics
/// Never panics. EOF or I/O errors are treated as "no guidance provided".
pub fn handle_human_review<R: io::BufRead + Send + 'static>(
    reader: R,
    task_id: &str,
    task_title: &str,
    task_notes: Option<&str>,
    iteration: u32,
    session_guidance: &mut SessionGuidance,
    timeout_secs: Option<u32>,
) -> bool {
    let banner = format_human_review_banner(task_id, task_title, task_notes);
    eprint!("{banner}");

    let lines = read_lines_with_timeout(reader, timeout_secs);
    let text = lines.join("\n");
    let has_guidance = !text.trim().is_empty();

    if has_guidance {
        let tagged = format!("[Human Review for {task_id}] {text}");
        eprintln!("Guidance recorded. Continuing...\n");
        session_guidance.add(iteration, tagged);
    } else {
        eprintln!("Skipping human review (no input).\n");
    }

    has_guidance
}

/// Format the human review banner string for display.
///
/// Returns a multi-line string containing the banner with task ID, title,
/// and notes (when present). The caller prints it to stderr.
pub fn format_human_review_banner(
    task_id: &str,
    task_title: &str,
    task_notes: Option<&str>,
) -> String {
    let sep = "═".repeat(44);
    let mut banner = format!("\n╔{sep}╗\n");
    banner.push_str("║           HUMAN REVIEW CHECKPOINT           ║\n");
    banner.push_str(&format!("╠{sep}╣\n"));
    banner.push_str(&format!("  Task:  {task_id}\n"));
    banner.push_str(&format!("  Title: {task_title}\n"));
    if let Some(notes) = task_notes {
        banner.push_str(&format!("  Notes: {notes}\n"));
    }
    banner.push_str(&format!("╠{sep}╣\n"));
    banner.push_str("  Enter feedback (empty line to skip):\n");
    banner.push_str(&format!("╚{sep}╝\n"));
    banner
}

/// Read lines from `reader` until an empty line or EOF, with optional timeout.
///
/// `timeout_secs: None` or `Some(0)` → blocking read.
/// `timeout_secs: Some(n > 0)` → spawn reader thread; collect lines until timeout fires.
pub(crate) fn read_lines_with_timeout<R: io::BufRead + Send + 'static>(
    reader: R,
    timeout_secs: Option<u32>,
) -> Vec<String> {
    match timeout_secs {
        None | Some(0) => {
            let mut lines = Vec::new();
            let mut saw_eof = true;
            for line_result in reader.lines() {
                saw_eof = false;
                match line_result {
                    Ok(line) if line.trim().is_empty() => break,
                    Ok(line) => lines.push(line),
                    Err(_) => break,
                }
            }
            if saw_eof && lines.is_empty() {
                eprintln!("Warning: EOF reached reading human review input. No guidance provided.");
            }
            lines
        }
        Some(n) => {
            use std::sync::mpsc;
            use std::time::Duration;

            let (tx, rx) = mpsc::channel::<Option<String>>();
            std::thread::spawn(move || {
                for line_result in reader.lines() {
                    match line_result {
                        Ok(line) => {
                            let is_empty = line.trim().is_empty();
                            let _ = tx.send(Some(line));
                            if is_empty {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(None);
                            break;
                        }
                    }
                }
            });

            let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(n));
            let mut lines = Vec::new();
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    if lines.is_empty() {
                        eprintln!("Human review timeout reached. No input provided.");
                    } else {
                        eprintln!("Human review timeout reached. Using partial input.");
                    }
                    break;
                }
                match rx.recv_timeout(remaining) {
                    Ok(Some(line)) if line.trim().is_empty() => break,
                    Ok(Some(line)) => lines.push(line),
                    Ok(None) => {
                        eprintln!(
                            "Warning: EOF reached reading human review input. No guidance provided."
                        );
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if lines.is_empty() {
                            eprintln!("Human review timeout reached. No input provided.");
                        } else {
                            eprintln!("Human review timeout reached. Using partial input.");
                        }
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            lines
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- File signal tests ---

    #[test]
    fn test_check_stop_signal_returns_false_when_no_file() {
        let temp_dir = TempDir::new().unwrap();
        assert!(!check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_returns_true_when_file_exists() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        assert!(check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_returns_true_with_content() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "reason: done for now").unwrap();
        assert!(check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_pause_signal_returns_false_when_no_file() {
        let temp_dir = TempDir::new().unwrap();
        assert!(!check_pause_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_pause_signal_returns_true_when_file_exists() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();
        assert!(check_pause_signal(temp_dir.path(), None));
    }

    // --- SignalFlag tests ---

    #[test]
    fn test_signal_flag_initially_false() {
        let flag = SignalFlag::new();
        assert!(!flag.is_signaled());
    }

    #[test]
    fn test_signal_flag_set() {
        let flag = SignalFlag::new();
        flag.set();
        assert!(flag.is_signaled());
    }

    #[test]
    fn test_signal_flag_clone_shares_state() {
        let flag1 = SignalFlag::new();
        let flag2 = flag1.clone();

        flag1.set();
        assert!(
            flag2.is_signaled(),
            "Cloned flag should see set from original"
        );
    }

    #[test]
    fn test_signal_flag_idempotent() {
        let flag = SignalFlag::new();
        flag.set();
        flag.set();
        flag.set();
        assert!(flag.is_signaled());
    }

    #[test]
    fn test_signal_flag_inner_arc() {
        let flag = SignalFlag::new();
        let inner = flag.inner();
        inner.store(true, Ordering::Relaxed);
        assert!(flag.is_signaled());
    }

    #[test]
    fn test_signal_flag_default() {
        let flag = SignalFlag::default();
        assert!(!flag.is_signaled());
    }

    // --- Cleanup tests ---

    #[test]
    fn test_cleanup_signal_files_removes_stop_and_pause() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();

        cleanup_signal_files(temp_dir.path());

        assert!(!temp_dir.path().join(STOP_FILE).exists());
        assert!(!temp_dir.path().join(PAUSE_FILE).exists());
    }

    #[test]
    fn test_cleanup_signal_files_removes_deadline_files() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join(".deadline-123"),
            "2024-01-01T00:00:00Z",
        )
        .unwrap();
        fs::write(
            temp_dir.path().join(".deadline-456"),
            "2024-01-01T12:00:00Z",
        )
        .unwrap();

        cleanup_signal_files(temp_dir.path());

        assert!(!temp_dir.path().join(".deadline-123").exists());
        assert!(!temp_dir.path().join(".deadline-456").exists());
    }

    #[test]
    fn test_cleanup_preserves_non_signal_files() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        fs::write(temp_dir.path().join("progress.txt"), "some progress").unwrap();
        fs::write(temp_dir.path().join("tasks.json"), "{}").unwrap();

        cleanup_signal_files(temp_dir.path());

        assert!(!temp_dir.path().join(STOP_FILE).exists());
        assert!(temp_dir.path().join("progress.txt").exists());
        assert!(temp_dir.path().join("tasks.json").exists());
    }

    #[test]
    fn test_cleanup_handles_nonexistent_files_gracefully() {
        let temp_dir = TempDir::new().unwrap();
        // No signal files exist — should not error
        cleanup_signal_files(temp_dir.path());
    }

    #[test]
    fn test_cleanup_handles_nonexistent_directory_gracefully() {
        let path = Path::new("/nonexistent/directory/path");
        // Should not panic
        cleanup_signal_files(path);
    }

    // --- Path helper tests ---

    #[test]
    fn test_stop_file_path() {
        let path = stop_file_path(Path::new("/project/tasks"), None);
        assert_eq!(path, PathBuf::from("/project/tasks/.stop"));
    }

    #[test]
    fn test_stop_file_path_with_prefix() {
        let path = stop_file_path(Path::new("/project/tasks"), Some("P1"));
        assert_eq!(path, PathBuf::from("/project/tasks/.stop-P1"));
    }

    #[test]
    fn test_pause_file_path() {
        let path = pause_file_path(Path::new("/project/tasks"), None);
        assert_eq!(path, PathBuf::from("/project/tasks/.pause"));
    }

    #[test]
    fn test_pause_file_path_with_prefix() {
        let path = pause_file_path(Path::new("/project/tasks"), Some("P1"));
        assert_eq!(path, PathBuf::from("/project/tasks/.pause-P1"));
    }

    // --- Per-session (prefix-scoped) signal file tests ---
    //
    // These tests define the expected behavior after prefix support is added to
    // check_stop_signal and check_pause_signal. They will fail to compile until
    // the functions accept `prefix: Option<&str>` as a second parameter.

    #[test]
    fn test_check_stop_signal_prefix_matches_session_specific_file() {
        // .stop-P1 exists → prefix "P1" triggers stop
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();

        assert!(check_stop_signal(temp_dir.path(), Some("P1")));
    }

    #[test]
    fn test_check_stop_signal_prefix_no_match_other_session_file() {
        // .stop-P1 exists → prefix "P2" must NOT trigger stop (known-bad discriminator)
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();

        assert!(!check_stop_signal(temp_dir.path(), Some("P2")));
    }

    #[test]
    fn test_check_stop_signal_global_fallback_triggers_for_prefixed_session() {
        // Global .stop exists → any prefixed session (P1, P2) must trigger stop
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        assert!(check_stop_signal(temp_dir.path(), Some("P1")));
        assert!(check_stop_signal(temp_dir.path(), Some("P2")));
    }

    #[test]
    fn test_check_stop_signal_global_fallback_triggers_for_no_prefix() {
        // Global .stop exists → session with no prefix must also trigger
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        assert!(check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_no_file_no_trigger_with_prefix() {
        // No signal files at all → must not trigger for any prefix
        let temp_dir = TempDir::new().unwrap();

        assert!(!check_stop_signal(temp_dir.path(), Some("P1")));
        assert!(!check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_session_specific_does_not_trigger_for_none_prefix() {
        // .stop-P1 exists but no global .stop → session with no prefix must NOT trigger
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();

        assert!(!check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_prefix_file_takes_priority_over_global() {
        // Both .stop-P1 and global .stop exist → P1 prefix still triggers (via session file)
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        assert!(check_stop_signal(temp_dir.path(), Some("P1")));
    }

    #[test]
    fn test_check_pause_signal_prefix_matches_session_specific_file() {
        // .pause-P1 exists → prefix "P1" triggers pause
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".pause-P1"), "").unwrap();

        assert!(check_pause_signal(temp_dir.path(), Some("P1")));
    }

    #[test]
    fn test_check_pause_signal_prefix_no_match_other_session_file() {
        // .pause-P1 exists → prefix "P2" must NOT trigger pause
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".pause-P1"), "").unwrap();

        assert!(!check_pause_signal(temp_dir.path(), Some("P2")));
    }

    #[test]
    fn test_check_pause_signal_global_fallback_triggers_for_prefixed_session() {
        // Global .pause exists → any prefixed session (P1, P2) must trigger pause
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();

        assert!(check_pause_signal(temp_dir.path(), Some("P1")));
        assert!(check_pause_signal(temp_dir.path(), Some("P2")));
    }

    #[test]
    fn test_check_pause_signal_global_fallback_triggers_for_no_prefix() {
        // Global .pause exists → session with no prefix must also trigger
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();

        assert!(check_pause_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_pause_signal_no_file_no_trigger() {
        // No signal files → must not trigger for any prefix
        let temp_dir = TempDir::new().unwrap();

        assert!(!check_pause_signal(temp_dir.path(), Some("P1")));
        assert!(!check_pause_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_pause_signal_session_specific_does_not_trigger_for_none_prefix() {
        // .pause-P1 exists but no global .pause → session with no prefix must NOT trigger
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".pause-P1"), "").unwrap();

        assert!(!check_pause_signal(temp_dir.path(), None));
    }

    // --- Prefix-scoped cleanup tests ---

    #[test]
    fn test_cleanup_signal_files_prefix_removes_session_and_global_files() {
        // cleanup with prefix "P1" removes .stop-P1, .pause-P1, AND global .stop/.pause
        // (because check_stop_signal falls back to global, so both must be cleaned)
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();
        fs::write(temp_dir.path().join(".pause-P1"), "").unwrap();
        fs::write(temp_dir.path().join(".stop-P2"), "").unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();

        cleanup_signal_files_for_prefix(temp_dir.path(), Some("P1"));

        // Session-specific P1 files removed
        assert!(!temp_dir.path().join(".stop-P1").exists());
        assert!(!temp_dir.path().join(".pause-P1").exists());
        // Global files also removed (engine falls back to global)
        assert!(!temp_dir.path().join(STOP_FILE).exists());
        assert!(!temp_dir.path().join(PAUSE_FILE).exists());
        // Other session files preserved
        assert!(temp_dir.path().join(".stop-P2").exists());
    }

    #[test]
    fn test_cleanup_signal_files_no_prefix_removes_global_files_only() {
        // cleanup with no prefix removes global .stop and .pause, not session-specific
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();

        cleanup_signal_files_for_prefix(temp_dir.path(), None);

        assert!(!temp_dir.path().join(STOP_FILE).exists());
        assert!(!temp_dir.path().join(PAUSE_FILE).exists());
        // Session-specific file preserved
        assert!(temp_dir.path().join(".stop-P1").exists());
    }

    #[test]
    fn test_cleanup_signal_files_prefix_handles_nonexistent_files_gracefully() {
        // cleanup with a prefix when no matching files exist must not panic
        let temp_dir = TempDir::new().unwrap();
        cleanup_signal_files_for_prefix(temp_dir.path(), Some("P1"));
    }

    // --- handle_human_review tests (require FEAT-004) ---

    #[test]
    fn test_handle_human_review_with_input_returns_true_and_records_guidance() {
        // Non-empty line followed by empty line terminates the read.
        let input = "my feedback\n\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result = handle_human_review(
            cursor,
            "TASK-123",
            "Some task title",
            None,
            1,
            &mut guidance,
            None,
        );

        assert!(result, "Should return true when guidance was provided");
        assert!(!guidance.is_empty(), "Guidance should be recorded");
    }

    #[test]
    fn test_handle_human_review_with_empty_input_returns_false_no_guidance() {
        // Just pressing Enter (empty line) — no guidance provided.
        let input = "\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result = handle_human_review(
            cursor,
            "TASK-123",
            "Some task title",
            None,
            1,
            &mut guidance,
            None,
        );

        assert!(!result, "Should return false when only empty input given");
        assert!(
            guidance.is_empty(),
            "Guidance must not be recorded for empty input"
        );
    }

    #[test]
    fn test_handle_human_review_guidance_tagged_with_task_id() {
        // Guidance must be stored as "[Human Review for TASK-ID] {input}".
        let input = "review feedback\n\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        handle_human_review(
            cursor,
            "FEAT-007",
            "Some feature",
            None,
            5,
            &mut guidance,
            None,
        );

        let formatted = guidance.format_for_prompt();
        assert!(
            formatted.contains("[Human Review for FEAT-007]"),
            "Guidance must be tagged with task ID; got: '{formatted}'"
        );
        assert!(
            formatted.contains("review feedback"),
            "Guidance must contain the input text; got: '{formatted}'"
        );
    }

    #[test]
    fn test_handle_human_review_stdin_eof_returns_false_no_panic() {
        // Empty reader simulates headless/piped stdin hitting EOF immediately.
        let input = "";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result = handle_human_review(
            cursor,
            "TASK-123",
            "Some task title",
            None,
            1,
            &mut guidance,
            None,
        );

        assert!(!result, "Should return false on EOF");
        assert!(guidance.is_empty(), "Must not record guidance on EOF");
    }

    #[test]
    fn test_handle_human_review_timeout_none_means_blocking_read() {
        // timeout=None: reads input normally (does not immediately return).
        let input = "blocking feedback\n\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result = handle_human_review(cursor, "TASK-123", "title", None, 1, &mut guidance, None);

        assert!(result, "timeout=None must read input and return true");
        assert!(!guidance.is_empty());
    }

    #[test]
    fn test_handle_human_review_timeout_zero_means_blocking_read() {
        // timeout=Some(0): treated as blocking (same as None), not an immediate return.
        let input = "zero timeout feedback\n\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result =
            handle_human_review(cursor, "TASK-123", "title", None, 1, &mut guidance, Some(0));

        assert!(
            result,
            "timeout=Some(0) must read input and return true (blocking)"
        );
        assert!(!guidance.is_empty());
    }

    // --- format_human_review_banner tests (require FEAT-004) ---

    #[test]
    fn test_human_review_banner_includes_task_id_title_and_notes() {
        let banner = format_human_review_banner(
            "TASK-456",
            "Deploy the feature",
            Some("Check that database migrations ran successfully"),
        );

        assert!(
            banner.contains("TASK-456"),
            "Banner must include task ID; got: '{banner}'"
        );
        assert!(
            banner.contains("Deploy the feature"),
            "Banner must include task title; got: '{banner}'"
        );
        assert!(
            banner.contains("Check that database migrations ran successfully"),
            "Banner must include notes; got: '{banner}'"
        );
    }

    #[test]
    fn test_human_review_banner_includes_task_id_and_title_without_notes() {
        let banner = format_human_review_banner("TASK-123", "Some task title", None);

        assert!(banner.contains("TASK-123"), "Banner must include task ID");
        assert!(
            banner.contains("Some task title"),
            "Banner must include task title"
        );
        // notes=None: must not panic and should not include a notes section with garbage
    }
}
