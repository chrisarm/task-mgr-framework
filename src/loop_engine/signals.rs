/// Signal handling for the autonomous agent loop.
///
/// Supports two signal mechanisms:
/// 1. **File-based signals**: `.stop` and `.pause` files in the tasks directory
/// 2. **UNIX signals**: SIGINT (Ctrl+C) and SIGTERM via `Arc<AtomicBool>`
///
/// The `.pause` signal enters an interactive mode where the user can provide
/// multi-line session guidance that accumulates across iterations.
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{DEADLINE_FILE_PREFIX, PAUSE_FILE, STOP_FILE};

/// Accumulated session guidance from `.pause` interactions.
///
/// Each pause interaction appends guidance with an iteration tag,
/// building up context across multiple pause/resume cycles.
#[derive(Debug, Default)]
pub struct SessionGuidance {
    entries: Vec<GuidanceEntry>,
}

/// A single guidance entry from one pause interaction.
#[derive(Debug)]
struct GuidanceEntry {
    iteration: u32,
    text: String,
}

impl SessionGuidance {
    /// Create a new empty SessionGuidance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add guidance from a pause interaction at the given iteration.
    pub fn add(&mut self, iteration: u32, text: String) {
        if !text.trim().is_empty() {
            self.entries.push(GuidanceEntry { iteration, text });
        }
    }

    /// Format all accumulated guidance for inclusion in the prompt.
    ///
    /// Returns empty string if no guidance has been recorded.
    pub fn format_for_prompt(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut result = String::new();
        for entry in &self.entries {
            result.push_str(&format!(
                "[Iteration {}] {}\n",
                entry.iteration,
                entry.text.trim()
            ));
        }
        result
    }

    /// Whether any guidance has been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Format all accumulated guidance for recording to progress.txt.
    ///
    /// Produces a structured progress entry with a "Session Guidance" header
    /// and all entries listed with their iteration numbers.
    /// Returns empty string if no guidance has been recorded.
    pub fn format_for_recording(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let mut result = format!("\n## {} - Session Guidance\n", timestamp);

        for entry in &self.entries {
            result.push_str(&format!(
                "- [Iteration {}] {}\n",
                entry.iteration,
                entry.text.trim()
            ));
        }
        result.push_str("---\n");
        result
    }
}

/// Check if a stop signal exists for the given session.
///
/// When `prefix` is `Some(p)`, checks `.stop-{p}` first (fast path), then falls back
/// to the global `.stop` file. When `prefix` is `None`, checks only `.stop`.
pub fn check_stop_signal(tasks_dir: &Path, prefix: Option<&str>) -> bool {
    if let Some(p) = prefix {
        if tasks_dir.join(format!("{STOP_FILE}-{p}")).exists() {
            return true;
        }
    }
    tasks_dir.join(STOP_FILE).exists()
}

/// Check if a pause signal exists for the given session.
///
/// When `prefix` is `Some(p)`, checks `.pause-{p}` first (fast path), then falls back
/// to the global `.pause` file. When `prefix` is `None`, checks only `.pause`.
pub fn check_pause_signal(tasks_dir: &Path, prefix: Option<&str>) -> bool {
    if let Some(p) = prefix {
        if tasks_dir.join(format!("{PAUSE_FILE}-{p}")).exists() {
            return true;
        }
    }
    tasks_dir.join(PAUSE_FILE).exists()
}

/// Clean up signal files for a specific session prefix.
///
/// When `prefix` is `Some(p)`: removes only `.stop-{p}` and `.pause-{p}`.
/// When `prefix` is `None`: removes global `.stop` and `.pause`.
/// Never removes other sessions' signal files.
pub fn cleanup_signal_files_for_prefix(tasks_dir: &Path, prefix: Option<&str>) {
    let (stop, pause) = match prefix {
        Some(p) => (format!("{STOP_FILE}-{p}"), format!("{PAUSE_FILE}-{p}")),
        None => (STOP_FILE.to_string(), PAUSE_FILE.to_string()),
    };
    for filename in &[stop, pause] {
        let path = tasks_dir.join(filename);
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("Warning: could not remove {}: {}", path.display(), e);
            }
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

    let mut lines = Vec::new();
    let stdin = io::stdin();
    let reader = stdin.lock();

    for line_result in reader.lines() {
        match line_result {
            Ok(line) if line.trim().is_empty() => break,
            Ok(line) => lines.push(line),
            Err(_) => break, // EOF or error
        }
    }

    // Delete the session-specific or global .pause file
    let pause_filename = match prefix {
        Some(p) => format!("{PAUSE_FILE}-{p}"),
        None => PAUSE_FILE.to_string(),
    };
    let pause_path = tasks_dir.join(&pause_filename);
    if pause_path.exists() {
        let _ = fs::remove_file(&pause_path);
    }

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
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("Warning: could not remove {}: {}", path.display(), e);
            }
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
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with(DEADLINE_FILE_PREFIX) {
                if let Err(e) = fs::remove_file(entry.path()) {
                    eprintln!(
                        "Warning: could not remove deadline file {}: {}",
                        entry.path().display(),
                        e
                    );
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- SessionGuidance tests ---

    #[test]
    fn test_session_guidance_new_is_empty() {
        let guidance = SessionGuidance::new();
        assert!(guidance.is_empty());
        assert!(guidance.format_for_prompt().is_empty());
    }

    #[test]
    fn test_session_guidance_add_and_format() {
        let mut guidance = SessionGuidance::new();
        guidance.add(3, "Focus on error handling".to_string());

        assert!(!guidance.is_empty());
        let formatted = guidance.format_for_prompt();
        assert!(formatted.contains("[Iteration 3]"));
        assert!(formatted.contains("Focus on error handling"));
    }

    #[test]
    fn test_session_guidance_accumulates_multiple_entries() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "First guidance".to_string());
        guidance.add(5, "Second guidance".to_string());
        guidance.add(10, "Third guidance".to_string());

        let formatted = guidance.format_for_prompt();
        assert!(formatted.contains("[Iteration 1]"));
        assert!(formatted.contains("First guidance"));
        assert!(formatted.contains("[Iteration 5]"));
        assert!(formatted.contains("Second guidance"));
        assert!(formatted.contains("[Iteration 10]"));
        assert!(formatted.contains("Third guidance"));
    }

    #[test]
    fn test_session_guidance_ignores_empty_text() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "".to_string());
        guidance.add(2, "   \n  ".to_string());

        assert!(guidance.is_empty());
        assert!(guidance.format_for_prompt().is_empty());
    }

    #[test]
    fn test_session_guidance_trims_text() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "  padded text  ".to_string());

        let formatted = guidance.format_for_prompt();
        assert!(formatted.contains("padded text"));
    }

    // --- SessionGuidance format_for_recording tests ---

    #[test]
    fn test_format_for_recording_empty_returns_empty() {
        let guidance = SessionGuidance::new();
        assert!(guidance.format_for_recording().is_empty());
    }

    #[test]
    fn test_format_for_recording_single_entry() {
        let mut guidance = SessionGuidance::new();
        guidance.add(3, "Focus on error handling".to_string());

        let formatted = guidance.format_for_recording();
        assert!(formatted.contains("Session Guidance"));
        assert!(formatted.contains("[Iteration 3] Focus on error handling"));
        assert!(formatted.contains("---"));
    }

    #[test]
    fn test_format_for_recording_multiple_entries() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "First guidance".to_string());
        guidance.add(5, "Second guidance".to_string());
        guidance.add(10, "Third guidance".to_string());

        let formatted = guidance.format_for_recording();
        assert!(formatted.contains("Session Guidance"));
        assert!(formatted.contains("[Iteration 1] First guidance"));
        assert!(formatted.contains("[Iteration 5] Second guidance"));
        assert!(formatted.contains("[Iteration 10] Third guidance"));
        assert!(formatted.contains("---"));
    }

    #[test]
    fn test_format_for_recording_has_timestamp() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test".to_string());

        let formatted = guidance.format_for_recording();
        // Should contain a UTC timestamp in the header
        assert!(formatted.contains("UTC"));
        // Should have the ## header format for progress.txt
        assert!(formatted.contains("## "));
    }

    #[test]
    fn test_format_for_recording_trims_entry_text() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "  padded text  ".to_string());

        let formatted = guidance.format_for_recording();
        assert!(formatted.contains("[Iteration 1] padded text"));
        // Should not contain leading/trailing spaces in the entry
        assert!(!formatted.contains("[Iteration 1]   padded text"));
    }

    #[test]
    fn test_format_for_recording_starts_with_newline() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test".to_string());

        let formatted = guidance.format_for_recording();
        // Should start with newline for clean appending to progress.txt
        assert!(formatted.starts_with('\n'));
    }

    #[test]
    fn test_format_for_recording_ends_with_separator() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test".to_string());

        let formatted = guidance.format_for_recording();
        assert!(formatted.ends_with("---\n"));
    }

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
    fn test_cleanup_signal_files_prefix_removes_only_session_specific_files() {
        // cleanup with prefix "P1" removes .stop-P1 and .pause-P1, not global or P2 files
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
        // Other session and global files preserved
        assert!(temp_dir.path().join(".stop-P2").exists());
        assert!(temp_dir.path().join(STOP_FILE).exists());
        assert!(temp_dir.path().join(PAUSE_FILE).exists());
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
}
