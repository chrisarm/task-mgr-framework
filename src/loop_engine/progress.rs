//! Progress event logging for the autonomous agent loop.
//!
//! Appends structured progress entries to a progress.txt file after each iteration.
//! This provides a persistent, human-readable log across loop sessions.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use chrono::Utc;

use crate::loop_engine::config::IterationOutcome;

/// Log an iteration result to the progress file.
///
/// Appends a structured entry in the format:
/// ```text
/// ## [timestamp] - Iteration N
/// - Task: TASK-ID
/// - Outcome: Completed|Blocked|Crash|etc.
/// - Files: file1.rs, file2.rs
/// ---
/// ```
///
/// Errors are logged to stderr but don't propagate — progress logging
/// should never crash the loop.
pub fn log_iteration(
    progress_path: &Path,
    iteration: u32,
    task_id: Option<&str>,
    outcome: &IterationOutcome,
    files: &[String],
    model: Option<&str>,
) {
    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let task = task_id.unwrap_or("(none)");
    let outcome_str = format_outcome(outcome);
    let files_str = if files.is_empty() {
        "(none)".to_string()
    } else {
        files.join(", ")
    };
    let model_display = model.unwrap_or("(default)");

    let entry = format!(
        "\n## {} - Iteration {}\n- Task: {}\n- Model: {}\n- Outcome: {}\n- Files: {}\n---\n",
        timestamp, iteration, task, model_display, outcome_str, files_str
    );

    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(progress_path)
    {
        Ok(mut file) => {
            if let Err(e) = file.write_all(entry.as_bytes()) {
                eprintln!(
                    "Warning: could not write to progress file {}: {}",
                    progress_path.display(),
                    e
                );
            }
        }
        Err(e) => {
            eprintln!(
                "Warning: could not open progress file {}: {}",
                progress_path.display(),
                e
            );
        }
    }
}

/// Format an IterationOutcome for human-readable display.
pub fn format_outcome(outcome: &IterationOutcome) -> String {
    match outcome {
        IterationOutcome::Completed => "Completed".to_string(),
        IterationOutcome::Blocked => "Blocked".to_string(),
        IterationOutcome::Crash(crash_type) => format!("Crash ({:?})", crash_type),
        IterationOutcome::RateLimit => "RateLimit".to_string(),
        IterationOutcome::Reorder(task_id) => format!("Reorder ({})", task_id),
        IterationOutcome::Stale => "Stale".to_string(),
        IterationOutcome::Empty => "Empty".to_string(),
        IterationOutcome::PromptOverflow => "PromptOverflow".to_string(),
    }
}

/// Maximum number of progress entries to keep after rotation.
const MAX_PROGRESS_ENTRIES: usize = 20;

/// Rotate the progress file to keep only the last `MAX_PROGRESS_ENTRIES` entries.
///
/// Reads the file, splits on `---` delimiters, keeps the last N entries, and writes back.
/// Errors are logged to stderr but never crash the loop.
pub fn rotate_progress(progress_path: &Path) {
    let content = match fs::read_to_string(progress_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!(
                "Warning: could not read progress file for rotation {}: {}",
                progress_path.display(),
                e
            );
            return;
        }
    };

    if content.trim().is_empty() {
        return;
    }

    // Split on "---" delimiter lines. Each entry ends with "---\n".
    // We split on "\n---\n" to separate entries, filtering out empty trailing parts.
    let entries: Vec<&str> = content
        .split("\n---\n")
        .filter(|e| !e.trim().is_empty())
        .collect();

    if entries.len() <= MAX_PROGRESS_ENTRIES {
        return;
    }

    // Keep last MAX_PROGRESS_ENTRIES entries, rejoin with the delimiter
    let start = entries.len() - MAX_PROGRESS_ENTRIES;
    let kept: Vec<&str> = entries[start..].to_vec();
    let mut rotated = kept.join("\n---\n");
    rotated.push_str("\n---\n");

    if let Err(e) = fs::write(progress_path, rotated) {
        eprintln!(
            "Warning: could not write rotated progress file {}: {}",
            progress_path.display(),
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::config::CrashType;
    use tempfile::TempDir;

    // --- log_iteration tests ---

    #[test]
    fn test_log_iteration_creates_file_and_writes_entry() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        log_iteration(
            &progress_path,
            1,
            Some("FEAT-001"),
            &IterationOutcome::Completed,
            &["src/lib.rs".to_string()],
            None,
        );

        assert!(progress_path.exists());
        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("Iteration 1"));
        assert!(content.contains("FEAT-001"));
        assert!(content.contains("Completed"));
        assert!(content.contains("src/lib.rs"));
        assert!(content.contains("---"));
    }

    #[test]
    fn test_log_iteration_appends_to_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        // Write initial content
        fs::write(&progress_path, "# Progress\n").unwrap();

        log_iteration(
            &progress_path,
            1,
            Some("FEAT-001"),
            &IterationOutcome::Completed,
            &[],
            None,
        );
        log_iteration(
            &progress_path,
            2,
            Some("FEAT-002"),
            &IterationOutcome::Blocked,
            &[],
            None,
        );

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.starts_with("# Progress\n"));
        assert!(content.contains("Iteration 1"));
        assert!(content.contains("Iteration 2"));
    }

    #[test]
    fn test_log_iteration_no_task_id() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        log_iteration(&progress_path, 1, None, &IterationOutcome::Empty, &[], None);

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("(none)"));
    }

    #[test]
    fn test_log_iteration_no_files() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        log_iteration(
            &progress_path,
            1,
            Some("FEAT-001"),
            &IterationOutcome::Completed,
            &[],
            None,
        );

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("Files: (none)"));
    }

    #[test]
    fn test_log_iteration_multiple_files() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        log_iteration(
            &progress_path,
            1,
            Some("FEAT-001"),
            &IterationOutcome::Completed,
            &["src/a.rs".to_string(), "src/b.rs".to_string()],
            None,
        );

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("src/a.rs, src/b.rs"));
    }

    #[test]
    fn test_log_iteration_invalid_path_does_not_panic() {
        // Write to a nonexistent directory — should not panic
        log_iteration(
            Path::new("/nonexistent/dir/progress.txt"),
            1,
            Some("FEAT-001"),
            &IterationOutcome::Completed,
            &[],
            None,
        );
    }

    #[test]
    fn test_log_iteration_with_none_model_shows_default() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        log_iteration(
            &progress_path,
            1,
            Some("FEAT-001"),
            &IterationOutcome::Completed,
            &[],
            None,
        );

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("- Model: (default)"));
    }

    #[test]
    fn test_log_iteration_with_some_model_shows_model_name() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        log_iteration(
            &progress_path,
            1,
            Some("FEAT-001"),
            &IterationOutcome::Completed,
            &[],
            Some("claude-sonnet-4-6"),
        );

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("- Model: claude-sonnet-4-6"));
    }

    // --- format_outcome tests ---

    #[test]
    fn test_format_outcome_completed() {
        assert_eq!(format_outcome(&IterationOutcome::Completed), "Completed");
    }

    #[test]
    fn test_format_outcome_blocked() {
        assert_eq!(format_outcome(&IterationOutcome::Blocked), "Blocked");
    }

    #[test]
    fn test_format_outcome_crash() {
        let result = format_outcome(&IterationOutcome::Crash(CrashType::OomOrKilled));
        assert!(result.contains("Crash"));
        assert!(result.contains("OomOrKilled"));
    }

    #[test]
    fn test_format_outcome_rate_limit() {
        assert_eq!(format_outcome(&IterationOutcome::RateLimit), "RateLimit");
    }

    #[test]
    fn test_format_outcome_reorder() {
        let result = format_outcome(&IterationOutcome::Reorder("FEAT-005".to_string()));
        assert!(result.contains("Reorder"));
        assert!(result.contains("FEAT-005"));
    }

    #[test]
    fn test_format_outcome_stale() {
        assert_eq!(format_outcome(&IterationOutcome::Stale), "Stale");
    }

    #[test]
    fn test_format_outcome_empty() {
        assert_eq!(format_outcome(&IterationOutcome::Empty), "Empty");
    }

    #[test]
    fn test_format_outcome_prompt_overflow() {
        assert_eq!(
            format_outcome(&IterationOutcome::PromptOverflow),
            "PromptOverflow"
        );
    }

    // --- rotate_progress tests ---

    /// Helper to build a progress file with N entries separated by `---` delimiters.
    fn build_progress_entries(n: usize) -> String {
        let mut content = String::new();
        for i in 1..=n {
            content.push_str(&format!(
                "\n## 2026-01-01 00:00:00 UTC - Iteration {}\n- Task: TASK-{:03}\n- Model: (default)\n- Outcome: Completed\n- Files: (none)\n---\n",
                i, i
            ));
        }
        content
    }

    #[test]
    fn test_rotate_progress_no_file_does_not_panic() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("nonexistent_progress.txt");

        // Should not panic when file doesn't exist
        rotate_progress(&progress_path);
        assert!(!progress_path.exists());
    }

    #[test]
    fn test_rotate_progress_under_limit_no_change() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let content = build_progress_entries(10);
        fs::write(&progress_path, &content).unwrap();

        rotate_progress(&progress_path);

        let after = fs::read_to_string(&progress_path).unwrap();
        assert_eq!(after, content, "Under-limit file should not be modified");
    }

    #[test]
    fn test_rotate_progress_over_limit_keeps_last_n() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let content = build_progress_entries(30);
        fs::write(&progress_path, &content).unwrap();

        rotate_progress(&progress_path);

        let after = fs::read_to_string(&progress_path).unwrap();

        // Should keep last 20 entries (iterations 11-30)
        assert!(
            !after.contains("TASK-001"),
            "Oldest entry should be rotated out"
        );
        assert!(
            !after.contains("TASK-010"),
            "Entry 10 should be rotated out"
        );
        assert!(
            after.contains("TASK-011") || after.contains("TASK-012"),
            "Entries around the boundary should be kept"
        );
        assert!(after.contains("TASK-030"), "Latest entry should be kept");

        // Count entries by counting "---" delimiters
        let entry_count = after.matches("\n---\n").count();
        assert!(
            entry_count <= MAX_PROGRESS_ENTRIES,
            "Should have at most {} entries, found {}",
            MAX_PROGRESS_ENTRIES,
            entry_count
        );
    }

    #[test]
    fn test_rotate_progress_exact_limit_no_change() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let content = build_progress_entries(20);
        fs::write(&progress_path, &content).unwrap();

        rotate_progress(&progress_path);

        let after = fs::read_to_string(&progress_path).unwrap();
        assert_eq!(after, content, "Exact-limit file should not be modified");
    }

    #[test]
    fn test_rotate_progress_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        fs::write(&progress_path, "").unwrap();

        rotate_progress(&progress_path);

        let after = fs::read_to_string(&progress_path).unwrap();
        assert_eq!(after, "", "Empty file should remain empty");
    }
}
