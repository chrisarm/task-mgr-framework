//! Progress event logging for the autonomous agent loop.
//!
//! Appends structured progress entries to a progress.txt file after each iteration.
//! This provides a persistent, human-readable log across loop sessions.

use std::fs::OpenOptions;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::config::CrashType;
    use std::fs;
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
}
