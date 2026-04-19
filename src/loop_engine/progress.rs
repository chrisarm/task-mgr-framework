//! Progress event logging for the autonomous agent loop.
//!
//! Appends structured progress entries to a progress.txt file after each iteration.
//! This provides a persistent, human-readable log across loop sessions.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use chrono::Utc;

use crate::loop_engine::config::{IterationOutcome, PermissionMode};
use crate::loop_engine::model::HAIKU_MODEL;

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
    effort: Option<&str>,
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
    let effort_display = effort.unwrap_or("(default)");

    let entry = format!(
        "\n## {} - Iteration {}\n- Task: {}\n- Model: {}\n- Effort: {}\n- Outcome: {}\n- Files: {}\n---\n",
        timestamp, iteration, task, model_display, effort_display, outcome_str, files_str
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
        IterationOutcome::NoEligibleTasks => "NoEligibleTasks".to_string(),
        IterationOutcome::Empty => "Empty".to_string(),
        IterationOutcome::PromptOverflow => "PromptOverflow".to_string(),
    }
}

/// Maximum number of regular iteration entries to keep after rotation.
/// Milestone summary entries are exempt and always preserved.
const MAX_PROGRESS_ENTRIES: usize = 7;

/// Marker substring identifying a milestone-summary entry. Used by both
/// `rotate_progress` (to preserve summaries past rotation) and
/// `summarize_milestone` (to find the cutoff for "since the last milestone").
const MILESTONE_SUMMARY_MARKER: &str = "Milestone Summary:";

fn is_milestone_summary(entry: &str) -> bool {
    entry.contains(MILESTONE_SUMMARY_MARKER)
}

/// Rotate the progress file to keep only the last `MAX_PROGRESS_ENTRIES` regular
/// iteration entries. **Milestone summary entries are always preserved** so that
/// the long-running narrative of a PRD survives rotation.
///
/// Reads the file, splits on `---` delimiters, keeps every milestone summary plus
/// the trailing N regular entries (in original order), and writes back. Errors are
/// logged to stderr but never crash the loop.
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

    let entries: Vec<&str> = content
        .split("\n---\n")
        .filter(|e| !e.trim().is_empty())
        .collect();

    let regular_count = entries.iter().filter(|e| !is_milestone_summary(e)).count();
    if regular_count <= MAX_PROGRESS_ENTRIES {
        return;
    }

    let drop_regular = regular_count - MAX_PROGRESS_ENTRIES;
    let mut dropped = 0usize;
    let kept: Vec<&str> = entries
        .into_iter()
        .filter(|e| {
            if is_milestone_summary(e) {
                true
            } else if dropped < drop_regular {
                dropped += 1;
                false
            } else {
                true
            }
        })
        .collect();

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

/// Parsed view of a single progress entry — only the fields needed for
/// milestone summarization. Unparseable lines are tolerated and produce
/// `None` fields rather than failures.
#[derive(Debug, Default, Clone)]
struct ParsedEntry {
    task_id: Option<String>,
    outcome: Option<String>,
    files: Vec<String>,
}

fn parse_entry(entry: &str) -> ParsedEntry {
    let mut parsed = ParsedEntry::default();
    for line in entry.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("- Task: ") {
            if rest != "(none)" {
                parsed.task_id = Some(rest.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("- Outcome: ") {
            parsed.outcome = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("- Files: ") {
            if rest != "(none)" {
                parsed.files = rest.split(", ").map(|s| s.to_string()).collect();
            }
        }
    }
    parsed
}

/// Heuristic recommendation for a cluster of crash/overflow entries on the
/// same task. Pure function — easy to unit-test against real outcome strings.
fn crash_recommendation(task_id: &str, outcomes: &[String]) -> Option<String> {
    if outcomes.len() < 2 {
        return None;
    }
    let overflow_count = outcomes.iter().filter(|o| o.contains("PromptOverflow")).count();
    let crash_count = outcomes.iter().filter(|o| o.starts_with("Crash")).count();
    let timeout_count = outcomes
        .iter()
        .filter(|o| o.contains("Timeout") || o.contains("OomOrKilled"))
        .count();

    let is_review = task_id.contains("-REVIEW");

    if overflow_count >= 2 {
        if is_review {
            return Some(format!(
                "{}× PromptOverflow on REVIEW-type task — verify the task isn't already complete (Opus 4.7 may be re-doing work); consider auto-skip or routing to Sonnet 4.6",
                overflow_count
            ));
        }
        return Some(format!(
            "{}× PromptOverflow — split the task into smaller subtasks, or route to Sonnet 4.6 (lower per-tool-use token cost)",
            overflow_count
        ));
    }
    if timeout_count >= 2 {
        return Some(format!(
            "{}× Timeout/OOM — increase per-iteration timeout or lower task difficulty",
            timeout_count
        ));
    }
    if crash_count >= 2 {
        return Some(format!(
            "{}× Crash — investigate root cause; consider escalating model or marking task blocked for human review",
            crash_count
        ));
    }
    None
}

/// Build the Haiku prompt asking for a short narrative summary + crash
/// recommendations. Kept private so the format is owned by this module.
fn build_haiku_summary_prompt(milestone_task_id: &str, raw_entries: &str) -> String {
    format!(
        "You are summarizing the progress log of an autonomous coding loop at a \
         milestone boundary. The milestone that just completed is `{}`.\n\n\
         Below are the raw iteration entries since the last milestone (or the start \
         of the run). Each entry records one loop iteration: the task it ran, model, \
         effort, outcome, and files touched.\n\n\
         Produce a SHORT human-readable summary (≤180 words) with two parts:\n\
         1. **Narrative**: 2–3 sentences describing what the loop accomplished and \
            any notable patterns (clusters of failures, repeated tasks, model \
            changes).\n\
         2. **Recommendations**: For any task that crashed or hit `PromptOverflow` \
            ≥2 times, give ONE concrete actionable recommendation per task (e.g. \
            \"split task\", \"route to Sonnet 4.6\", \"verify already complete\", \
            \"increase timeout\"). Skip this section if the run was clean.\n\n\
         Output PLAIN TEXT only — no markdown headers, no fenced code blocks, no \
         `---` lines (those break our parser). Start directly with the narrative.\n\n\
         Raw entries:\n{}",
        milestone_task_id, raw_entries
    )
}

/// Try to generate the narrative + recommendations section via Haiku.
///
/// Returns `None` on any failure (binary missing, non-zero exit, empty output) —
/// callers fall back to the deterministic heuristic in that case.
///
/// Designed to be cheap: text-only permission mode (no tools), no timeout
/// override (Haiku is fast — short prompts, ~5–15s typical), `db_dir` threaded
/// through so any nested `task-mgr` invocation hits the canonical DB.
fn try_haiku_summary(
    milestone_task_id: &str,
    raw_entries: &str,
    db_dir: Option<&Path>,
) -> Option<String> {
    let prompt = build_haiku_summary_prompt(milestone_task_id, raw_entries);
    let result = match crate::loop_engine::claude::spawn_claude(
        &prompt,
        None,
        None,
        Some(HAIKU_MODEL),
        None,
        false,
        &PermissionMode::text_only(),
        None,
        None,
        db_dir,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "Warning: milestone summary Haiku spawn failed: {} — falling back to heuristic",
                e
            );
            return None;
        }
    };
    if result.exit_code != 0 {
        eprintln!(
            "Warning: milestone summary Haiku exited with code {} — falling back to heuristic",
            result.exit_code
        );
        return None;
    }
    let trimmed = result.output.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Sanitize: strip any literal `---` separator lines so the LLM can't
    // accidentally split the entry boundary our parser relies on.
    let cleaned: String = trimmed
        .lines()
        .filter(|l| l.trim() != "---")
        .collect::<Vec<_>>()
        .join("\n");
    Some(cleaned)
}

/// Compact the progress file by replacing every raw iteration entry since
/// the last milestone summary (or file start) with a single summary block.
///
/// Milestones are the compaction mechanism — after this runs, all raw entries
/// in the summarized window are dropped from the file and the summary takes
/// their place. Prior milestone summaries are preserved verbatim, so the
/// long-running narrative survives.
///
/// The summary records iterations covered, distinct tasks, completed tasks,
/// files touched, and (when `db_dir` is `Some`) a Haiku-generated narrative +
/// crash-avoidance recommendations. When `db_dir` is `None` or the Haiku call
/// fails, falls back to the deterministic `crash_recommendation` heuristic.
///
/// **Note on "Iterations covered":** the count reflects raw entries currently
/// present in the file since the last milestone — NOT the true number of
/// iterations elapsed. `rotate_progress` may have already trimmed older
/// entries before the milestone fired, so the count is a lower bound.
///
/// Best-effort: file I/O and LLM failures log to stderr and do not propagate.
pub fn summarize_milestone(
    progress_path: &Path,
    milestone_task_id: &str,
    db_dir: Option<&Path>,
) {
    let content = match fs::read_to_string(progress_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!(
                "Warning: could not read progress file for milestone summary {}: {}",
                progress_path.display(),
                e
            );
            return;
        }
    };

    let entries: Vec<&str> = content
        .split("\n---\n")
        .filter(|e| !e.trim().is_empty())
        .collect();

    let last_milestone_idx = entries.iter().rposition(|e| is_milestone_summary(e));
    let scope_start = last_milestone_idx.map(|i| i + 1).unwrap_or(0);
    let scope: Vec<ParsedEntry> = entries[scope_start..]
        .iter()
        .map(|e| parse_entry(e))
        .collect();

    if scope.is_empty() {
        // Nothing to summarize — last entry was already a milestone summary,
        // or the file is empty. Skip silently.
        return;
    }

    let mut by_task: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut completed: Vec<String> = Vec::new();
    let mut files_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for entry in &scope {
        if let Some(tid) = &entry.task_id
            && let Some(outcome) = &entry.outcome
        {
            by_task.entry(tid.clone()).or_default().push(outcome.clone());
            if outcome == "Completed" && !completed.contains(tid) {
                completed.push(tid.clone());
            }
        }
        for f in &entry.files {
            files_set.insert(f.clone());
        }
    }

    let mut recommendations: Vec<String> = Vec::new();
    for (tid, outcomes) in &by_task {
        if let Some(rec) = crash_recommendation(tid, outcomes) {
            recommendations.push(format!("  - {}: {}", tid, rec));
        }
    }

    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let mut summary = format!(
        "\n## {} - Milestone Summary: {}\n- Iterations covered: {}\n- Distinct tasks: {}\n- Tasks completed: {}\n- Files touched: {}\n",
        timestamp,
        milestone_task_id,
        scope.len(),
        by_task.len(),
        if completed.is_empty() {
            "(none)".to_string()
        } else {
            completed.join(", ")
        },
        if files_set.is_empty() {
            "(none)".to_string()
        } else {
            files_set.into_iter().collect::<Vec<_>>().join(", ")
        },
    );

    // Optional Haiku-generated narrative + recommendations. Skipped when
    // db_dir is None (test path) or when the spawn fails for any reason —
    // either way the deterministic heuristic block always provides a baseline.
    let llm_section = if db_dir.is_some() {
        let raw_entries = entries[scope_start..].join("\n---\n");
        try_haiku_summary(milestone_task_id, &raw_entries, db_dir)
    } else {
        None
    };

    if let Some(narrative) = llm_section {
        summary.push_str("- Narrative + recommendations (haiku):\n");
        for line in narrative.lines() {
            summary.push_str("    ");
            summary.push_str(line);
            summary.push('\n');
        }
    } else if recommendations.is_empty() {
        summary.push_str("- Crash recommendations: (none — clean run)\n");
    } else {
        summary.push_str("- Crash recommendations:\n");
        for rec in &recommendations {
            summary.push_str(rec);
            summary.push('\n');
        }
    }
    summary.push_str("---\n");

    // Rebuild the file: keep every entry up to and including the last existing
    // milestone summary, drop the raw iteration entries we just summarized,
    // then append the new summary block.
    let kept: Vec<&str> = entries[..scope_start].to_vec();
    let mut rebuilt = String::with_capacity(content.len() + summary.len());
    if !kept.is_empty() {
        rebuilt.push_str(&kept.join("\n---\n"));
        rebuilt.push_str("\n---\n");
    }
    rebuilt.push_str(&summary);

    if let Err(e) = fs::write(progress_path, rebuilt) {
        eprintln!(
            "Warning: could not write milestone summary to {}: {}",
            progress_path.display(),
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::config::CrashType;
    use crate::loop_engine::model::SONNET_MODEL;
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
            None,
        );
        log_iteration(
            &progress_path,
            2,
            Some("FEAT-002"),
            &IterationOutcome::Blocked,
            &[],
            None,
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

        log_iteration(
            &progress_path,
            1,
            None,
            &IterationOutcome::Empty,
            &[],
            None,
            None,
        );

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
            None,
        );

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("- Model: (default)"));
        assert!(content.contains("- Effort: (default)"));
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
            Some(SONNET_MODEL),
            Some("xhigh"),
        );

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains(&format!("- Model: {SONNET_MODEL}")));
        assert!(content.contains("- Effort: xhigh"));
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
    fn test_format_outcome_no_eligible_tasks() {
        assert_eq!(
            format_outcome(&IterationOutcome::NoEligibleTasks),
            "NoEligibleTasks"
        );
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
        let content = build_progress_entries(MAX_PROGRESS_ENTRIES - 1);
        fs::write(&progress_path, &content).unwrap();

        rotate_progress(&progress_path);

        let after = fs::read_to_string(&progress_path).unwrap();
        assert_eq!(after, content, "Under-limit file should not be modified");
    }

    #[test]
    fn test_rotate_progress_over_limit_keeps_last_n() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let total = MAX_PROGRESS_ENTRIES * 3;
        let content = build_progress_entries(total);
        fs::write(&progress_path, &content).unwrap();

        rotate_progress(&progress_path);

        let after = fs::read_to_string(&progress_path).unwrap();

        let first_kept = total - MAX_PROGRESS_ENTRIES + 1;
        assert!(
            !after.contains(&format!("TASK-{:03}", first_kept - 1)),
            "Entry just before the kept window should be rotated out"
        );
        assert!(
            after.contains(&format!("TASK-{:03}", first_kept)),
            "First entry inside the kept window should remain"
        );
        assert!(
            after.contains(&format!("TASK-{:03}", total)),
            "Latest entry should be kept"
        );

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
        let content = build_progress_entries(MAX_PROGRESS_ENTRIES);
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

    #[test]
    fn test_rotate_progress_preserves_milestone_summaries() {
        // File layout: 1 milestone summary then MAX*2 regular entries.
        // After rotation, the milestone block must survive even though the
        // regular entries before/after it are well past the cap.
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        let mut content = String::new();
        for i in 1..=MAX_PROGRESS_ENTRIES {
            content.push_str(&format!(
                "\n## 2026-01-01 - Iteration {}\n- Task: TASK-{:03}\n- Model: (default)\n- Outcome: Completed\n- Files: (none)\n---\n",
                i, i
            ));
        }
        content.push_str(
            "\n## 2026-01-02 - Milestone Summary: MILESTONE-1\n- Iterations covered: 7\n---\n",
        );
        for i in (MAX_PROGRESS_ENTRIES + 1)..=(MAX_PROGRESS_ENTRIES * 2) {
            content.push_str(&format!(
                "\n## 2026-01-03 - Iteration {}\n- Task: TASK-{:03}\n- Model: (default)\n- Outcome: Completed\n- Files: (none)\n---\n",
                i, i
            ));
        }
        fs::write(&progress_path, &content).unwrap();

        rotate_progress(&progress_path);

        let after = fs::read_to_string(&progress_path).unwrap();
        assert!(
            after.contains("Milestone Summary: MILESTONE-1"),
            "milestone summary must survive rotation"
        );
        // Pre-milestone regular entries should be the ones rotated out.
        assert!(
            !after.contains("TASK-001"),
            "earliest pre-milestone regular entry should be dropped"
        );
        // Latest regular entry should be kept.
        assert!(
            after.contains(&format!("TASK-{:03}", MAX_PROGRESS_ENTRIES * 2)),
            "newest regular entry should be kept"
        );
        // Total regular entries should not exceed the cap.
        let regular = after
            .split("\n---\n")
            .filter(|e| !e.trim().is_empty() && !e.contains("Milestone Summary"))
            .count();
        assert!(
            regular <= MAX_PROGRESS_ENTRIES,
            "regular-entry count {} exceeded cap {}",
            regular,
            MAX_PROGRESS_ENTRIES
        );
    }

    // --- crash_recommendation tests ---

    #[test]
    fn test_crash_recommendation_single_outcome_returns_none() {
        let recs = crash_recommendation("FEAT-001", &["PromptOverflow".to_string()]);
        assert!(recs.is_none(), "one entry isn't a pattern");
    }

    #[test]
    fn test_crash_recommendation_repeated_overflow() {
        let outcomes = vec!["PromptOverflow".to_string(), "PromptOverflow".to_string()];
        let rec = crash_recommendation("FEAT-001", &outcomes).expect("must produce recommendation");
        assert!(rec.contains("PromptOverflow"), "must name the symptom");
        assert!(
            rec.contains("Sonnet") || rec.contains("split"),
            "must propose a concrete remedy: {}",
            rec
        );
    }

    #[test]
    fn test_crash_recommendation_review_task_overflow_suggests_already_complete_check() {
        let outcomes = vec!["PromptOverflow".to_string(), "PromptOverflow".to_string()];
        let rec = crash_recommendation("REFACTOR-REVIEW-2", &outcomes)
            .expect("must produce recommendation");
        assert!(
            rec.contains("already complete") || rec.contains("auto-skip"),
            "REVIEW-type tasks should trigger the loop-on-completed-work warning: {}",
            rec
        );
    }

    #[test]
    fn test_crash_recommendation_repeated_timeout() {
        let outcomes = vec![
            "Crash (Timeout)".to_string(),
            "Crash (Timeout)".to_string(),
        ];
        let rec = crash_recommendation("FEAT-002", &outcomes).expect("must produce recommendation");
        assert!(
            rec.contains("timeout") || rec.contains("Timeout") || rec.contains("difficulty"),
            "timeout cluster should suggest timeout/difficulty fix: {}",
            rec
        );
    }

    // --- summarize_milestone tests ---

    fn write_iteration_entry(buf: &mut String, iteration: u32, task_id: &str, outcome: &str) {
        buf.push_str(&format!(
            "\n## 2026-01-01 - Iteration {}\n- Task: {}\n- Model: (default)\n- Effort: medium\n- Outcome: {}\n- Files: (none)\n---\n",
            iteration, task_id, outcome
        ));
    }

    #[test]
    fn test_summarize_milestone_replaces_raw_entries() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        let mut content = String::new();
        write_iteration_entry(&mut content, 1, "FEAT-001", "Completed");
        write_iteration_entry(&mut content, 2, "FEAT-002", "Completed");
        write_iteration_entry(&mut content, 3, "MILESTONE-1", "Completed");
        fs::write(&progress_path, &content).unwrap();

        summarize_milestone(&progress_path, "MILESTONE-1", None);

        let after = fs::read_to_string(&progress_path).unwrap();
        // Raw FEAT-001 / FEAT-002 entries should be GONE — replaced by summary.
        assert!(
            !after.contains("Iteration 1"),
            "raw iteration entries must be dropped after summarization"
        );
        assert!(
            after.contains("Milestone Summary: MILESTONE-1"),
            "summary block must be present"
        );
        assert!(
            after.contains("Iterations covered: 3"),
            "summary must count the entries it replaced"
        );
        assert!(
            after.contains("FEAT-001") && after.contains("FEAT-002"),
            "completed task list must be preserved in the summary"
        );
    }

    #[test]
    fn test_summarize_milestone_preserves_prior_summaries() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        let mut content = String::new();
        write_iteration_entry(&mut content, 1, "FEAT-001", "Completed");
        content.push_str(
            "\n## 2026-01-01 - Milestone Summary: MILESTONE-1\n- Iterations covered: 1\n- Crash recommendations: (none — clean run)\n---\n",
        );
        write_iteration_entry(&mut content, 2, "FEAT-002", "Completed");
        write_iteration_entry(&mut content, 3, "FEAT-003", "Completed");
        fs::write(&progress_path, &content).unwrap();

        summarize_milestone(&progress_path, "MILESTONE-2", None);

        let after = fs::read_to_string(&progress_path).unwrap();
        assert!(
            after.contains("Milestone Summary: MILESTONE-1"),
            "prior milestone summary must remain"
        );
        assert!(
            after.contains("Milestone Summary: MILESTONE-2"),
            "new milestone summary must be appended"
        );
        // Only entries since MILESTONE-1 should be summarized — count = 2.
        assert!(
            after.contains("Iterations covered: 2"),
            "new summary must only count entries since the prior milestone, got: {}",
            after
        );
        assert!(
            !after.contains("Iteration 2") && !after.contains("Iteration 3"),
            "raw entries since prior milestone must be dropped"
        );
    }

    #[test]
    fn test_summarize_milestone_includes_crash_recommendation() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        let mut content = String::new();
        write_iteration_entry(&mut content, 1, "REFACTOR-REVIEW-2", "PromptOverflow");
        write_iteration_entry(&mut content, 2, "REFACTOR-REVIEW-2", "PromptOverflow");
        write_iteration_entry(&mut content, 3, "REFACTOR-REVIEW-2", "PromptOverflow");
        fs::write(&progress_path, &content).unwrap();

        summarize_milestone(&progress_path, "MILESTONE-2", None);

        let after = fs::read_to_string(&progress_path).unwrap();
        assert!(
            after.contains("Crash recommendations:"),
            "summary must include recommendations section"
        );
        assert!(
            after.contains("REFACTOR-REVIEW-2"),
            "summary must name the offending task"
        );
        assert!(
            after.contains("PromptOverflow") && (after.contains("Sonnet") || after.contains("auto-skip") || after.contains("already complete")),
            "summary must include both the symptom count and a remedy: {}",
            after
        );
    }

    #[test]
    fn test_summarize_milestone_no_entries_is_noop() {
        // Already-summarized state: file ends with a milestone block. Calling
        // summarize again with no new entries between should leave the file
        // unchanged.
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let content =
            "\n## 2026-01-01 - Milestone Summary: MILESTONE-1\n- Iterations covered: 0\n---\n";
        fs::write(&progress_path, content).unwrap();

        summarize_milestone(&progress_path, "MILESTONE-2", None);

        let after = fs::read_to_string(&progress_path).unwrap();
        assert_eq!(after, content, "no-op when nothing to summarize");
    }

    #[test]
    fn test_summarize_milestone_missing_file_is_noop() {
        let temp_dir = TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("missing.txt");
        summarize_milestone(&progress_path, "MILESTONE-1", None);
        assert!(
            !progress_path.exists(),
            "must not create a progress file from nothing"
        );
    }
}
