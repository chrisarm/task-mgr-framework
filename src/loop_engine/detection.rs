//! Output detection engine for analyzing Claude subprocess results.
//!
//! Determines the `IterationOutcome` by inspecting the Claude process's
//! stdout output and exit code. Checks for completion signals, blockers,
//! reorder requests, rate-limit errors, crashes, and empty output.
//!
//! Priority order (highest to lowest):
//! Completed > Blocked > Reorder > RateLimit > Crash > NoEligibleTasks > Empty
use std::path::Path;

use crate::loop_engine::config::{CrashType, IterationOutcome, KeyDecision, KeyDecisionOption};

// --- Exit Code Classification ---
// Maps raw i32 exit codes to typed CrashType variants.
// Only `categorize_crash` lives here; it is called exclusively by `analyze_output` below.
// Extraction to a separate module was considered but not warranted: the function is 6 lines
// and has a single caller in this file. Section markers provide adequate separation.

/// Categorize a crash by its exit code.
fn categorize_crash(exit_code: i32) -> CrashType {
    match exit_code {
        137 => CrashType::OomOrKilled,
        139 => CrashType::Segfault,
        _ => CrashType::RuntimeError,
    }
}

// --- Output String Analysis ---
// All functions below inspect Claude's stdout text to classify the iteration outcome.

/// Analyze Claude subprocess output and exit code to determine iteration outcome.
///
/// Checks output patterns in priority order:
/// 1. `<promise>COMPLETE</promise>` in last 20 lines -> Completed
/// 2. `<promise>BLOCKED</promise>` in last 20 lines -> Blocked
/// 3. `<reorder>TASK-ID</reorder>` anywhere in output -> Reorder(task_id)
/// 4. Rate-limit patterns (429, usage limit) -> RateLimit
/// 5. Non-zero exit code -> Crash (categorized by exit code)
/// 6. Empty output with exit 0 -> Empty
///
/// The `dir` parameter is reserved for future DB-based verification
/// (secondary check: query remaining tasks).
pub fn analyze_output(output: &str, exit_code: i32, _dir: &Path) -> IterationOutcome {
    // Step 1: Check last 20 lines for completion/blocked signals
    let last_20: Vec<&str> = output.lines().rev().take(20).collect();

    let has_complete = last_20
        .iter()
        .any(|line| line.contains("<promise>COMPLETE</promise>"));
    let has_blocked = last_20
        .iter()
        .any(|line| line.contains("<promise>BLOCKED</promise>"));

    if has_complete {
        return IterationOutcome::Completed;
    }
    if has_blocked {
        return IterationOutcome::Blocked;
    }

    // Step 2: Check for reorder tag anywhere in output
    if let Some(task_id) = extract_reorder_task_id(output) {
        return IterationOutcome::Reorder(task_id);
    }

    // Step 3: Check for rate-limit patterns
    if is_rate_limited(output) {
        return IterationOutcome::RateLimit;
    }

    // Step 3.5: Detect "Prompt is too long" (Claude CLI context-window overflow)
    // before generic crash classification. The CLI emits this on stdout when
    // the running conversation exceeds the model context window; handled
    // separately so the engine can downgrade effort and reset the task.
    if is_prompt_too_long(output) {
        return IterationOutcome::Crash(CrashType::PromptTooLong);
    }

    // Step 4: Check exit code for crashes
    if exit_code != 0 {
        return IterationOutcome::Crash(categorize_crash(exit_code));
    }

    // Step 5: Check for empty output
    if output.trim().is_empty() {
        return IterationOutcome::Empty;
    }

    // Default: no signal detected, treat as no-eligible-tasks (no progress)
    IterationOutcome::NoEligibleTasks
}

/// Extract task ID from `<reorder>TASK-ID</reorder>` tag in output.
///
/// Returns `None` if no valid reorder tag found. Requires both opening
/// and closing tags with a non-empty task ID between them.
fn extract_reorder_task_id(output: &str) -> Option<String> {
    // Simple string-based extraction (no regex dependency needed for this)
    let start_tag = "<reorder>";
    let end_tag = "</reorder>";

    let start_pos = output.find(start_tag)?;
    let content_start = start_pos + start_tag.len();
    let end_pos = output[content_start..].find(end_tag)?;
    let task_id = output[content_start..content_start + end_pos].trim();

    if task_id.is_empty() {
        return None;
    }

    Some(task_id.to_string())
}

/// Check if output contains the Claude CLI "Prompt is too long" error.
///
/// Claude emits this exact string on stdout when the assembled conversation
/// exceeds the model's context window. Match case-insensitively so minor CLI
/// wording variants still classify correctly.
pub(crate) fn is_prompt_too_long(output: &str) -> bool {
    output.to_lowercase().contains("prompt is too long")
}

/// Check if output contains rate-limit error patterns.
pub(crate) fn is_rate_limited(output: &str) -> bool {
    let output_lower = output.to_lowercase();
    output_lower.contains("rate_limit_error")
        || output_lower.contains("429")
            && (output_lower.contains("rate") || output_lower.contains("limit"))
        || output_lower.contains("usage")
            && output_lower.contains("limit")
            && output_lower.contains("reached")
        || output_lower.contains("hit your limit")
}

// --- Key Decision Extraction ---

/// Extract all `<key-decision>` blocks from Claude output.
///
/// Returns a `Vec<KeyDecision>` with one entry per valid block. Blocks are
/// skipped if they are malformed (missing closing tag, empty title or
/// description, or zero valid `<option>` tags).
pub fn extract_key_decisions(output: &str) -> Vec<KeyDecision> {
    let open_tag = "<key-decision>";
    let close_tag = "</key-decision>";
    let mut results = Vec::new();
    let mut remaining = output;

    while let Some(start) = remaining.find(open_tag) {
        let after_open = &remaining[start + open_tag.len()..];
        match after_open.find(close_tag) {
            None => break, // malformed: no closing tag — skip rest
            Some(end) => {
                let block = &after_open[..end];
                if let Some(kd) = parse_key_decision_block(block) {
                    results.push(kd);
                }
                remaining = &after_open[end + close_tag.len()..];
            }
        }
    }

    results
}

/// Parse a single `<key-decision>` block (content between open/close tags).
///
/// Returns `None` if the block is missing required fields or has no valid options.
fn parse_key_decision_block(block: &str) -> Option<KeyDecision> {
    let title = extract_inner(block, "<title>", "</title>")?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }

    let description = extract_inner(block, "<description>", "</description>")?
        .trim()
        .to_string();
    if description.is_empty() {
        return None;
    }

    let options = extract_options(block);
    if options.is_empty() {
        return None;
    }

    Some(KeyDecision {
        title,
        description,
        options,
    })
}

/// Extract the trimmed inner text between `open` and `close` tags (first occurrence).
///
/// Returns `None` if either tag is absent.
fn extract_inner<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = start + text[start..].find(close)?;
    Some(&text[start..end])
}

/// Extract all `<option label="...">description</option>` entries from a block.
///
/// Options with a missing or empty label attribute are skipped.
fn extract_options(block: &str) -> Vec<KeyDecisionOption> {
    let open_prefix = "<option label=\"";
    let mut options = Vec::new();
    let mut remaining = block;

    while let Some(attr_start) = remaining.find(open_prefix) {
        let after_attr = &remaining[attr_start + open_prefix.len()..];
        // Find closing quote of label attribute
        let Some(label_end) = after_attr.find('"') else {
            break;
        };
        let label = after_attr[..label_end].trim().to_string();

        // Advance past `label="...">`
        let after_label_quote = &after_attr[label_end + 1..];
        let Some(tag_close) = after_label_quote.find('>') else {
            break;
        };
        let content_start = &after_label_quote[tag_close + 1..];

        // Find the closing </option>
        let Some(content_end) = content_start.find("</option>") else {
            break;
        };
        let description = content_start[..content_end].trim().to_string();

        if !label.is_empty() {
            options.push(KeyDecisionOption { label, description });
        }

        remaining = &content_start[content_end + "</option>".len()..];
    }

    options
}

// --- `<task-status>` Side-Band Tag Extraction ---

/// Status change requested by a `<task-status>TASK-ID:status</task-status>` tag.
///
/// Mirrors the subset of task state transitions the loop engine can apply by
/// dispatching through the existing command handlers (`complete`, `fail`,
/// `skip`, `irrelevant`, `unblock`, `reset_tasks`). Unknown statuses cause the
/// tag to be skipped entirely rather than producing a sentinel variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatusChange {
    Done,
    Failed,
    Skipped,
    Irrelevant,
    Unblock,
    Reset,
}

/// One parsed `<task-status>TASK-ID:status</task-status>` tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStatusUpdate {
    pub task_id: String,
    pub status: TaskStatusChange,
}

/// Extract all `<task-status>TASK-ID:status</task-status>` tags from Claude output.
///
/// Multiple tags in the same output are each returned in document order. Tags
/// with empty task IDs, missing colons, empty statuses, or unknown statuses
/// are silently skipped — the whole-output scan continues past the malformed
/// block so one bad tag does not suppress later valid ones.
///
/// Status parsing is case-insensitive: `done`, `Done`, and `DONE` all map to
/// [`TaskStatusChange::Done`].
pub fn extract_status_updates(output: &str) -> Vec<TaskStatusUpdate> {
    let open_tag = "<task-status>";
    let close_tag = "</task-status>";
    let mut results = Vec::new();
    let mut remaining = output;

    while let Some(start) = remaining.find(open_tag) {
        let after_open = &remaining[start + open_tag.len()..];
        let Some(end) = after_open.find(close_tag) else {
            break;
        };
        let body = &after_open[..end];
        if let Some(update) = parse_status_tag_body(body) {
            results.push(update);
        }
        remaining = &after_open[end + close_tag.len()..];
    }

    results
}

/// Parse the body between `<task-status>` and `</task-status>`.
///
/// Expected form: `TASK-ID:status` (with optional whitespace around either
/// side of the colon). Returns `None` when the shape is wrong or either side
/// does not resolve to a real value.
fn parse_status_tag_body(body: &str) -> Option<TaskStatusUpdate> {
    let (id_part, status_part) = body.split_once(':')?;
    let task_id = id_part.trim();
    let status_raw = status_part.trim();
    if task_id.is_empty() || status_raw.is_empty() {
        return None;
    }
    let status = parse_status_keyword(status_raw)?;
    Some(TaskStatusUpdate {
        task_id: task_id.to_string(),
        status,
    })
}

/// Map a (case-insensitive) status keyword to [`TaskStatusChange`].
///
/// Returns `None` for anything outside the known dispatch surface so unknown
/// keywords never silently match a catch-all.
fn parse_status_keyword(raw: &str) -> Option<TaskStatusChange> {
    match raw.to_ascii_lowercase().as_str() {
        "done" | "completed" | "complete" => Some(TaskStatusChange::Done),
        "failed" | "fail" | "blocked" => Some(TaskStatusChange::Failed),
        "skipped" | "skip" => Some(TaskStatusChange::Skipped),
        "irrelevant" => Some(TaskStatusChange::Irrelevant),
        "unblock" | "unblocked" => Some(TaskStatusChange::Unblock),
        "reset" | "todo" => Some(TaskStatusChange::Reset),
        _ => None,
    }
}

/// Check if Claude's output reports a specific task as already complete.
///
/// Catches the case where a task was completed in a prior run but the DB
/// was never updated. Claude recognizes the work is done and reports it
/// (e.g., "This task is already complete"), but makes no commit — so
/// neither the git check nor the bracket-pattern scan can detect it.
///
/// Returns true when both conditions are met:
/// 1. The output contains the task ID (full or prefix-stripped)
/// 2. The output contains an "already complete" indicator phrase
pub fn is_task_reported_already_complete(
    output: &str,
    task_id: &str,
    _task_prefix: Option<&str>,
) -> bool {
    // Condition 1: output mentions this task (full ID only — no base ID fallback)
    if !output.contains(task_id) {
        return false;
    }

    // Condition 2: output signals "already done"
    let output_lower = output.to_lowercase();
    let already_complete_signals = [
        "already complete",
        "already completed",
        "already done",
        "already been completed",
        "was completed in a previous",
        "no further work is needed",
        "no further work needed",
    ];
    already_complete_signals
        .iter()
        .any(|signal| output_lower.contains(signal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_dir() -> PathBuf {
        PathBuf::from("/tmp/test-detection")
    }

    // --- AC 1: COMPLETE detection in last 20 lines ---

    #[test]
    fn test_detects_complete_in_last_20_lines() {
        let mut output = String::new();
        // Add some lines of normal output
        for i in 0..10 {
            output.push_str(&format!("Working on task {}...\n", i));
        }
        output.push_str("<promise>COMPLETE</promise>\n");

        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Completed,
            "Should detect COMPLETE in last 20 lines"
        );
    }

    #[test]
    fn test_complete_on_last_line() {
        let output = "Some work done\n<promise>COMPLETE</promise>";
        let result = analyze_output(output, 0, &test_dir());
        assert_eq!(result, IterationOutcome::Completed);
    }

    #[test]
    fn test_complete_exactly_at_line_20_from_end() {
        // Build output where COMPLETE is exactly the 20th line from the end
        let mut lines: Vec<String> = Vec::new();
        lines.push("<promise>COMPLETE</promise>".to_string());
        for i in 0..19 {
            lines.push(format!("trailing line {}", i));
        }
        let output = lines.join("\n");

        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Completed,
            "COMPLETE on exactly line 20 from end should be detected"
        );
    }

    #[test]
    fn test_complete_outside_last_20_lines_not_detected() {
        // Build output where COMPLETE is line 21 from end (outside window)
        let mut lines: Vec<String> = Vec::new();
        lines.push("<promise>COMPLETE</promise>".to_string());
        for i in 0..20 {
            lines.push(format!("trailing line {}", i));
        }
        let output = lines.join("\n");

        let result = analyze_output(&output, 0, &test_dir());
        assert_ne!(
            result,
            IterationOutcome::Completed,
            "COMPLETE on line 21 from end should NOT be detected"
        );
    }

    // --- AC 2: BLOCKED detection in last 20 lines ---

    #[test]
    fn test_detects_blocked_in_last_20_lines() {
        let output = "Missing dependency\n<promise>BLOCKED</promise>\n";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Blocked,
            "Should detect BLOCKED in last 20 lines"
        );
    }

    #[test]
    fn test_blocked_on_last_line() {
        let output = "Cannot proceed\n<promise>BLOCKED</promise>";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(result, IterationOutcome::Blocked);
    }

    // --- AC 3: Reorder detection ---

    #[test]
    fn test_detects_reorder_with_task_id() {
        let output = "I think LOOP-005 would be better.\n<reorder>LOOP-005</reorder>\nDone.";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Reorder("LOOP-005".to_string()),
            "Should extract task ID from <reorder> tag"
        );
    }

    #[test]
    fn test_detects_reorder_with_different_task_id_format() {
        let output = "Suggest switching to FEAT-024.\n<reorder>FEAT-024</reorder>";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(result, IterationOutcome::Reorder("FEAT-024".to_string()),);
    }

    #[test]
    fn test_reorder_with_whitespace_around_task_id() {
        let output = "<reorder>  LOOP-005  </reorder>";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Reorder("LOOP-005".to_string()),
            "Should trim whitespace from task ID"
        );
    }

    #[test]
    fn test_reorder_empty_tag_not_detected() {
        let output = "Some output\n<reorder></reorder>\nMore output";
        let result = analyze_output(&output, 0, &test_dir());
        assert_ne!(
            result,
            IterationOutcome::Reorder(String::new()),
            "Empty reorder tag should not produce a Reorder outcome"
        );
    }

    #[test]
    fn test_reorder_missing_closing_tag_not_detected() {
        let output = "Some output\n<reorder>LOOP-005\nMore output";
        let result = analyze_output(&output, 0, &test_dir());
        // Should NOT be Reorder since no closing tag
        if let IterationOutcome::Reorder(_) = result {
            panic!("Should not detect reorder without closing tag");
        }
    }

    // --- AC 4: Rate-limit detection ---

    #[test]
    fn test_detects_rate_limit_error_pattern() {
        let output = "Error: rate_limit_error - too many requests\n";
        let result = analyze_output(&output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Should detect rate_limit_error pattern"
        );
    }

    #[test]
    fn test_detects_429_rate_pattern() {
        let output = "HTTP 429 rate limit exceeded\n";
        let result = analyze_output(&output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Should detect 429 rate pattern"
        );
    }

    #[test]
    fn test_detects_usage_limit_reached_pattern() {
        let output = "Usage limit reached. Please wait.\n";
        let result = analyze_output(&output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Should detect usage limit reached pattern"
        );
    }

    // --- AC 5: Exit code crash categorization ---

    #[test]
    fn test_exit_137_returns_oom_or_killed() {
        let output = "Some work was done\n";
        let result = analyze_output(output, 137, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Crash(CrashType::OomOrKilled),
            "Exit 137 should map to OomOrKilled"
        );
    }

    #[test]
    fn test_exit_139_returns_segfault() {
        let output = "Some work was done\n";
        let result = analyze_output(output, 139, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Crash(CrashType::Segfault),
            "Exit 139 should map to Segfault"
        );
    }

    #[test]
    fn test_exit_1_returns_runtime_error() {
        let output = "Error: something went wrong\n";
        let result = analyze_output(output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Crash(CrashType::RuntimeError),
            "Exit 1 should map to RuntimeError"
        );
    }

    #[test]
    fn test_exit_2_returns_runtime_error() {
        let output = "Error: command not found\n";
        let result = analyze_output(output, 2, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Crash(CrashType::RuntimeError),
            "Exit 2 should map to RuntimeError"
        );
    }

    // --- AC 6: Empty output detection ---

    #[test]
    fn test_empty_output_with_exit_0_returns_empty() {
        let result = analyze_output("", 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Empty,
            "Empty string with exit 0 should return Empty"
        );
    }

    #[test]
    fn test_whitespace_only_output_with_exit_0_returns_empty() {
        let result = analyze_output("   \n  \n  ", 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Empty,
            "Whitespace-only output with exit 0 should return Empty"
        );
    }

    #[test]
    fn test_nonempty_output_with_exit_0_returns_no_eligible_tasks() {
        let output = "Did some work but no completion signal\n";
        let result = analyze_output(output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::NoEligibleTasks,
            "Non-empty output with exit 0 and no signal should return NoEligibleTasks"
        );
    }

    // --- AC 7: COMPLETE takes priority over BLOCKED ---

    #[test]
    fn test_complete_takes_priority_over_blocked() {
        let output =
            "Working...\n<promise>BLOCKED</promise>\nFixed it!\n<promise>COMPLETE</promise>\n";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Completed,
            "COMPLETE should take priority over BLOCKED when both present in last 20 lines"
        );
    }

    #[test]
    fn test_complete_takes_priority_even_when_blocked_appears_later() {
        // Both in last 20 lines, BLOCKED after COMPLETE
        let output =
            "Start\n<promise>COMPLETE</promise>\nMore work\n<promise>BLOCKED</promise>\nEnd";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Completed,
            "COMPLETE should take priority regardless of order"
        );
    }

    // --- Additional edge cases for robust contract definition ---

    #[test]
    fn test_rate_limit_takes_priority_over_crash_exit_code() {
        // Output contains rate limit pattern AND has non-zero exit code
        let output = "Error: rate_limit_error\n";
        let result = analyze_output(output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Rate limit in output should take priority over crash exit code"
        );
    }

    #[test]
    fn test_hit_your_limit_detected_as_rate_limit() {
        // Exact message from Claude CLI when session limit is reached
        let output = "You've hit your limit · resets 4pm (America/Los_Angeles)\nYou've hit your limit · resets 4pm (America/Los_Angeles)\n";
        let result = analyze_output(output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Session limit message should be detected as RateLimit, not Crash"
        );
    }

    #[test]
    fn test_complete_takes_priority_over_rate_limit() {
        let output = "rate_limit_error earlier\nRecovered\n<promise>COMPLETE</promise>\n";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Completed,
            "COMPLETE should take priority over rate limit pattern"
        );
    }

    #[test]
    fn test_blocked_takes_priority_over_reorder() {
        let output = "<reorder>FEAT-005</reorder>\n<promise>BLOCKED</promise>\n";
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Blocked,
            "BLOCKED should take priority over Reorder"
        );
    }

    // --- Helper function unit tests ---

    #[test]
    fn test_extract_reorder_task_id_valid() {
        let output = "text <reorder>FEAT-001</reorder> more text";
        assert_eq!(
            extract_reorder_task_id(output),
            Some("FEAT-001".to_string())
        );
    }

    #[test]
    fn test_extract_reorder_task_id_empty() {
        let output = "text <reorder></reorder> more text";
        assert_eq!(extract_reorder_task_id(output), None);
    }

    #[test]
    fn test_extract_reorder_task_id_missing_close() {
        let output = "text <reorder>FEAT-001 more text";
        assert_eq!(extract_reorder_task_id(output), None);
    }

    #[test]
    fn test_extract_reorder_task_id_no_tag() {
        let output = "no reorder tags here";
        assert_eq!(extract_reorder_task_id(output), None);
    }

    #[test]
    fn test_extract_reorder_task_id_whitespace_trimmed() {
        let output = "<reorder>  TASK-ID  </reorder>";
        assert_eq!(extract_reorder_task_id(output), Some("TASK-ID".to_string()));
    }

    #[test]
    fn test_categorize_crash_137() {
        assert_eq!(categorize_crash(137), CrashType::OomOrKilled);
    }

    #[test]
    fn test_categorize_crash_139() {
        assert_eq!(categorize_crash(139), CrashType::Segfault);
    }

    #[test]
    fn test_categorize_crash_other() {
        assert_eq!(categorize_crash(1), CrashType::RuntimeError);
        assert_eq!(categorize_crash(2), CrashType::RuntimeError);
        assert_eq!(categorize_crash(127), CrashType::RuntimeError);
        assert_eq!(categorize_crash(255), CrashType::RuntimeError);
    }

    #[test]
    fn test_is_rate_limited_positive() {
        assert!(is_rate_limited("rate_limit_error"));
        assert!(is_rate_limited("HTTP 429 rate limit"));
        assert!(is_rate_limited("Usage limit reached"));
        assert!(is_rate_limited(
            "You've hit your limit · resets 4pm (America/Los_Angeles)"
        ));
        assert!(is_rate_limited("You've hit your limit"));
    }

    #[test]
    fn test_is_rate_limited_negative() {
        assert!(!is_rate_limited("normal output"));
        assert!(!is_rate_limited("task completed successfully"));
        assert!(!is_rate_limited(""));
    }

    // ======================================================================
    // Comprehensive edge case tests (TEST-001)
    // ======================================================================

    // --- AC 1: COMPLETE on line 20 IS detected (boundary verification) ---

    #[test]
    fn test_complete_boundary_line_20_detected() {
        // 1 COMPLETE line + 19 trailing lines = COMPLETE is 20th from end
        let mut lines = vec!["<promise>COMPLETE</promise>".to_string()];
        for i in 0..19 {
            lines.push(format!("trailing {}", i));
        }
        let output = lines.join("\n");
        assert_eq!(
            analyze_output(&output, 0, &test_dir()),
            IterationOutcome::Completed
        );
    }

    // --- AC 2: COMPLETE on line 21 (just outside window) NOT detected ---

    #[test]
    fn test_complete_boundary_line_21_not_detected() {
        // 1 COMPLETE line + 20 trailing lines = COMPLETE is 21st from end
        let mut lines = vec!["<promise>COMPLETE</promise>".to_string()];
        for i in 0..20 {
            lines.push(format!("trailing {}", i));
        }
        let output = lines.join("\n");
        assert_eq!(
            analyze_output(&output, 0, &test_dir()),
            IterationOutcome::NoEligibleTasks,
            "COMPLETE on line 21 from end should NOT be detected"
        );
    }

    // --- AC 3: Malformed reorder tags ---

    #[test]
    fn test_reorder_whitespace_only_task_id_not_detected() {
        let output = "output\n<reorder>   </reorder>\nmore output";
        let result = analyze_output(output, 0, &test_dir());
        // Whitespace-only should NOT yield Reorder (trim leaves empty string)
        if let IterationOutcome::Reorder(_) = result {
            panic!("Whitespace-only reorder tag should not produce Reorder outcome");
        }
    }

    #[test]
    fn test_reorder_no_opening_tag() {
        let output = "FEAT-001</reorder>";
        assert_eq!(extract_reorder_task_id(output), None);
    }

    #[test]
    fn test_reorder_nested_tags_extracts_first() {
        let output = "<reorder><reorder>FEAT-001</reorder></reorder>";
        // The inner tag content is "<reorder>FEAT-001", which ends at first "</reorder>"
        let result = extract_reorder_task_id(output);
        assert!(
            result.is_some(),
            "Should extract something from nested tags"
        );
    }

    #[test]
    fn test_reorder_multiple_tags_returns_first() {
        let output = "text <reorder>FEAT-001</reorder> middle <reorder>FEAT-002</reorder> end";
        assert_eq!(
            extract_reorder_task_id(output),
            Some("FEAT-001".to_string()),
            "Should extract first reorder tag"
        );
    }

    #[test]
    fn test_reorder_tag_case_sensitive() {
        // Tags should be case-sensitive (uppercase should NOT match)
        let output = "<REORDER>FEAT-001</REORDER>";
        assert_eq!(
            extract_reorder_task_id(output),
            None,
            "Reorder tag should be case-sensitive"
        );
    }

    #[test]
    fn test_reorder_tag_with_newline_in_id() {
        let output = "<reorder>FEAT\n001</reorder>";
        let result = extract_reorder_task_id(output);
        // The task ID will include the newline; trim only strips leading/trailing whitespace
        // but \n in the middle stays
        assert!(result.is_some(), "Newline in task ID should still extract");
    }

    // --- AC 4: Both COMPLETE and BLOCKED returns Completed ---

    #[test]
    fn test_complete_and_blocked_interleaved() {
        let output = "<promise>BLOCKED</promise>\n\
                      work\n\
                      <promise>COMPLETE</promise>\n\
                      <promise>BLOCKED</promise>";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
            "COMPLETE takes priority even when BLOCKED appears after it"
        );
    }

    // --- AC 5: Rate limit pattern in middle of line ---

    #[test]
    fn test_rate_limit_in_middle_of_line() {
        let output = "Error occurred: rate_limit_error was encountered during processing\n";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::RateLimit,
            "Rate limit pattern in middle of line should be detected"
        );
    }

    #[test]
    fn test_rate_limit_429_in_middle_of_line() {
        let output = "The server responded with 429 rate limiting error";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::RateLimit,
        );
    }

    #[test]
    fn test_rate_limit_case_insensitive() {
        assert!(is_rate_limited("RATE_LIMIT_ERROR"));
        assert!(is_rate_limited("Rate_Limit_Error"));
        assert!(is_rate_limited("Usage Limit Reached"));
    }

    #[test]
    fn test_429_without_rate_context_is_not_rate_limit() {
        // "429" alone without "rate" or "limit" should not trigger
        let output = "There were 429 items in the list\n";
        // Check: "429" is present, but need "rate" or "limit" too
        // The condition is: "429" AND ("rate" OR "limit")
        // "items" and "list" contain "limit"? No. "list" != "limit"
        assert!(
            !is_rate_limited(output),
            "429 without rate/limit context should not trigger"
        );
    }

    #[test]
    fn test_usage_limit_partial_match_not_triggered() {
        // Need all three: "usage" + "limit" + "reached"
        let output = "Usage statistics show the limit was high";
        assert!(
            !is_rate_limited(output),
            "Need all three words: usage, limit, reached"
        );
    }

    // --- AC 6: Very large output (10000+ lines) ---

    #[test]
    fn test_large_output_with_complete_at_end() {
        let mut lines: Vec<String> = (0..10000)
            .map(|i| format!("Working on item {}...", i))
            .collect();
        lines.push("<promise>COMPLETE</promise>".to_string());
        let output = lines.join("\n");

        assert_eq!(
            analyze_output(&output, 0, &test_dir()),
            IterationOutcome::Completed,
            "Should detect COMPLETE in large output"
        );
    }

    #[test]
    fn test_large_output_with_complete_far_from_end() {
        let mut lines: Vec<String> = Vec::new();
        lines.push("<promise>COMPLETE</promise>".to_string());
        for i in 0..10000 {
            lines.push(format!("Working on item {}...", i));
        }
        let output = lines.join("\n");

        assert_eq!(
            analyze_output(&output, 0, &test_dir()),
            IterationOutcome::NoEligibleTasks,
            "COMPLETE far from end of large output should not be detected"
        );
    }

    #[test]
    fn test_large_output_no_signals() {
        let lines: Vec<String> = (0..10000)
            .map(|i| format!("Working on item {}...", i))
            .collect();
        let output = lines.join("\n");

        assert_eq!(
            analyze_output(&output, 0, &test_dir()),
            IterationOutcome::NoEligibleTasks,
            "Large output with no signals should be NoEligibleTasks"
        );
    }

    #[test]
    fn test_large_output_with_rate_limit_early() {
        // Rate limit search is full-output, not last-20
        let mut lines = vec!["rate_limit_error: too many requests".to_string()];
        for i in 0..10000 {
            lines.push(format!("line {}", i));
        }
        let output = lines.join("\n");

        assert_eq!(
            analyze_output(&output, 1, &test_dir()),
            IterationOutcome::RateLimit,
            "Rate limit anywhere in large output should be detected"
        );
    }

    // --- AC 7: Unicode characters don't crash ---

    #[test]
    fn test_unicode_output_no_crash() {
        let output = "日本語のテスト出力 🎉\n\
                      Ñoño café résumé naïve\n\
                      <promise>COMPLETE</promise>\n\
                      이것은 한국어입니다 🚀";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
            "Unicode output should not crash detection"
        );
    }

    #[test]
    fn test_unicode_in_reorder_tag() {
        let output = "<reorder>ТЕСТ-001</reorder>";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Reorder("ТЕСТ-001".to_string()),
            "Unicode task IDs should be extracted correctly"
        );
    }

    #[test]
    fn test_emoji_heavy_output_no_crash() {
        let output = "🔧 Working on task 🎯\n\
                      ✅ Step 1 done\n\
                      ✅ Step 2 done\n\
                      🏁 <promise>COMPLETE</promise>";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
        );
    }

    #[test]
    fn test_unicode_only_output_is_stale() {
        let output = "日本語のテスト 🎉";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::NoEligibleTasks,
        );
    }

    // --- AC 8: Empty output additional edge cases ---

    #[test]
    fn test_empty_output_with_nonzero_exit_is_crash() {
        let result = analyze_output("", 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Crash(CrashType::RuntimeError),
            "Empty output with non-zero exit should be Crash, not Empty"
        );
    }

    #[test]
    fn test_newlines_only_is_empty() {
        let result = analyze_output("\n\n\n\n", 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Empty,
            "Newlines-only output should be Empty"
        );
    }

    #[test]
    fn test_tabs_and_spaces_is_empty() {
        let result = analyze_output("\t  \t  \n\t  ", 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Empty,
            "Tabs and spaces only should be Empty"
        );
    }

    // --- Additional priority/interaction edge cases ---

    #[test]
    fn test_complete_overrides_rate_limit_and_crash() {
        // Complete in output, rate limit pattern, AND non-zero exit
        let output = "rate_limit_error\nrecovered\n<promise>COMPLETE</promise>\n";
        assert_eq!(
            analyze_output(output, 137, &test_dir()),
            IterationOutcome::Completed,
            "COMPLETE should override both rate limit and crash"
        );
    }

    #[test]
    fn test_blocked_overrides_rate_limit() {
        let output = "rate_limit_error happened\nbut then blocked\n<promise>BLOCKED</promise>\n";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Blocked,
            "BLOCKED should override rate limit"
        );
    }

    #[test]
    fn test_reorder_overrides_rate_limit() {
        // Reorder present, rate limit present, no complete/blocked
        let output = "rate_limit_error\n<reorder>FEAT-005</reorder>\n";
        // Wait - rate_limit_error IS in the output. But reorder is checked before rate limit.
        // Actually looking at the code: Complete > Blocked > Reorder > RateLimit
        // But rate_limit_error also present — need to check ordering
        // Step 1: no COMPLETE/BLOCKED in last 20
        // Step 2: reorder tag found → returns Reorder
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Reorder("FEAT-005".to_string()),
            "Reorder should override rate limit"
        );
    }

    #[test]
    fn test_stale_returned_for_normal_output() {
        let output =
            "Did some work, compiled things, ran tests.\nAll green.\nNo completion signal.";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::NoEligibleTasks,
        );
    }

    // --- Partial/malformed promise tags ---

    #[test]
    fn test_partial_complete_tag_not_detected() {
        let output = "<promise>COMPLET</promise>\n";
        assert_ne!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
            "Partial COMPLETE text should not be detected"
        );
    }

    #[test]
    fn test_promise_tag_without_closing_not_detected() {
        let output = "<promise>COMPLETE\nmore output\n";
        assert_ne!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
            "Unclosed promise tag should not be detected"
        );
    }

    #[test]
    fn test_complete_text_without_promise_tags_not_detected() {
        let output = "COMPLETE\n";
        assert_ne!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
            "COMPLETE without promise tags should not be detected"
        );
    }

    #[test]
    fn test_promise_complete_with_extra_whitespace_not_detected() {
        // Exact match required — no whitespace tolerance inside the tag
        let output = "<promise> COMPLETE </promise>\n";
        assert_ne!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
            "Whitespace inside <promise> tag should not match"
        );
    }

    // --- Exit code edge cases ---

    #[test]
    fn test_exit_code_negative_is_crash() {
        let output = "some output";
        let result = analyze_output(output, -1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Crash(CrashType::RuntimeError),
            "Negative exit code should be RuntimeError"
        );
    }

    #[test]
    fn test_exit_code_127_command_not_found() {
        let output = "command not found";
        let result = analyze_output(output, 127, &test_dir());
        assert_eq!(result, IterationOutcome::Crash(CrashType::RuntimeError),);
    }

    #[test]
    fn test_exit_code_143_sigterm() {
        let output = "terminated";
        let result = analyze_output(output, 143, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Crash(CrashType::RuntimeError),
            "Exit 143 (SIGTERM) maps to RuntimeError (not special-cased)"
        );
    }

    // --- is_task_reported_already_complete tests ---

    #[test]
    fn test_already_complete_with_full_task_id() {
        let output =
            "This task (`a3e1b7c9-TEST-003`) is already complete.\nNo further work needed.";
        assert!(is_task_reported_already_complete(
            output,
            "a3e1b7c9-TEST-003",
            Some("a3e1b7c9"),
        ));
    }

    #[test]
    fn test_already_complete_with_base_id_no_match() {
        // Base ID only (no full prefixed ID) should NOT match anymore
        let output = "This task (TEST-003) is already completed in a previous iteration.";
        assert!(
            !is_task_reported_already_complete(output, "a3e1b7c9-TEST-003", Some("a3e1b7c9")),
            "Should NOT match base ID without prefix"
        );
    }

    #[test]
    fn test_already_complete_no_task_id_in_output() {
        let output = "This task is already complete. No further work needed.";
        assert!(
            !is_task_reported_already_complete(output, "a3e1b7c9-TEST-003", Some("a3e1b7c9")),
            "Should not match when task ID is absent from output"
        );
    }

    #[test]
    fn test_already_complete_no_signal_phrase() {
        let output = "Working on a3e1b7c9-TEST-003... implemented the feature.";
        assert!(
            !is_task_reported_already_complete(output, "a3e1b7c9-TEST-003", Some("a3e1b7c9")),
            "Should not match when no 'already complete' signal is present"
        );
    }

    #[test]
    fn test_already_complete_case_insensitive_signal() {
        let output = "Task a3e1b7c9-TEST-003 was ALREADY COMPLETED in a prior run.";
        assert!(is_task_reported_already_complete(
            output,
            "a3e1b7c9-TEST-003",
            Some("a3e1b7c9"),
        ));
    }

    #[test]
    fn test_already_complete_no_prefix() {
        let output = "Task FEAT-001 is already done. Nothing to do.";
        assert!(is_task_reported_already_complete(output, "FEAT-001", None));
    }

    // ======================================================================
    // extract_key_decisions tests (KDP-FEAT-002)
    // ======================================================================

    fn make_kd(title: &str, description: &str, options: &[(&str, &str)]) -> String {
        let opts: String = options
            .iter()
            .map(|(l, d)| format!("<option label=\"{}\">{}</option>", l, d))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "<key-decision>\n<title>{}</title>\n<description>{}</description>\n{}\n</key-decision>",
            title, description, opts
        )
    }

    #[test]
    fn test_one_well_formed_key_decision() {
        let output = make_kd(
            "Auth Strategy",
            "Choose how users authenticate",
            &[
                ("A: JWT", "Stateless, scales well"),
                ("B: Session", "Simpler but stateful"),
            ],
        );
        let result = extract_key_decisions(&output);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Auth Strategy");
        assert_eq!(result[0].description, "Choose how users authenticate");
        assert_eq!(result[0].options.len(), 2);
        assert_eq!(result[0].options[0].label, "A: JWT");
        assert_eq!(result[0].options[0].description, "Stateless, scales well");
        assert_eq!(result[0].options[1].label, "B: Session");
    }

    #[test]
    fn test_two_key_decisions_returns_two() {
        let a = make_kd(
            "Decision A",
            "Desc A",
            &[("A: One", "opt1"), ("B: Two", "opt2")],
        );
        let b = make_kd("Decision B", "Desc B", &[("C: Three", "opt3")]);
        let output = format!("{}\n{}", a, b);
        let result = extract_key_decisions(&output);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].title, "Decision A");
        assert_eq!(result[1].title, "Decision B");
    }

    #[test]
    fn test_no_key_decision_tags_returns_empty() {
        let output = "Some normal Claude output with no key decision tags.";
        assert_eq!(extract_key_decisions(output), vec![]);
    }

    #[test]
    fn test_malformed_missing_close_tag_returns_empty() {
        let output = "<key-decision>\n<title>Something</title>\n<description>Desc</description>\n<option label=\"A: Foo\">bar</option>\n";
        // No </key-decision>
        assert_eq!(extract_key_decisions(output), vec![]);
    }

    #[test]
    fn test_empty_title_skipped() {
        let output = make_kd("", "Desc", &[("A: Foo", "bar")]);
        assert_eq!(extract_key_decisions(&output), vec![]);
    }

    #[test]
    fn test_zero_valid_options_skipped() {
        // Option has empty label — should be skipped, leaving zero valid options
        let output = "<key-decision>\n<title>T</title>\n<description>D</description>\n<option label=\"\">something</option>\n</key-decision>";
        assert_eq!(extract_key_decisions(output), vec![]);
    }

    #[test]
    fn test_option_label_and_description_extracted() {
        let output = make_kd(
            "Storage",
            "Pick storage engine",
            &[
                ("A: SQLite", "Simple embedded"),
                ("B: Postgres", "Full-featured"),
            ],
        );
        let result = extract_key_decisions(&output);
        assert_eq!(result[0].options[0].label, "A: SQLite");
        assert_eq!(result[0].options[0].description, "Simple embedded");
        assert_eq!(result[0].options[1].label, "B: Postgres");
        assert_eq!(result[0].options[1].description, "Full-featured");
    }

    #[test]
    fn test_whitespace_trimmed_from_fields() {
        let output = "<key-decision>\n<title>  Trimmed  </title>\n<description>  also trimmed  </description>\n<option label=\"  A: x  \">  trimmed desc  </option>\n</key-decision>";
        let result = extract_key_decisions(output);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Trimmed");
        assert_eq!(result[0].description, "also trimmed");
        // Label attribute is trimmed too
        assert_eq!(result[0].options[0].label, "A: x");
        assert_eq!(result[0].options[0].description, "trimmed desc");
    }

    // --- Prompt-too-long detection (context-window overflow) ---

    #[test]
    fn test_detects_prompt_too_long_exact_message() {
        let output = "some tool output\nPrompt is too long\n";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::Crash(CrashType::PromptTooLong),
        );
    }

    #[test]
    fn test_detects_prompt_too_long_regardless_of_exit_code() {
        // Fires on exit 0 too — Claude CLI may print the error and exit cleanly
        let output = "Prompt is too long";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Crash(CrashType::PromptTooLong),
        );
    }

    #[test]
    fn test_prompt_too_long_case_insensitive() {
        assert!(is_prompt_too_long("PROMPT IS TOO LONG"));
        assert!(is_prompt_too_long("Prompt Is Too Long"));
        assert!(is_prompt_too_long("the prompt is too long, aborting"));
    }

    #[test]
    fn test_prompt_too_long_negative() {
        assert!(!is_prompt_too_long(""));
        assert!(!is_prompt_too_long("normal output"));
        assert!(!is_prompt_too_long("prompt was long but fine"));
    }

    #[test]
    fn test_complete_beats_prompt_too_long() {
        let output = "Prompt is too long earlier\nrecovered\n<promise>COMPLETE</promise>\n";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
            "COMPLETE must win over PromptTooLong"
        );
    }

    #[test]
    fn test_blocked_beats_prompt_too_long() {
        let output = "Prompt is too long\n<promise>BLOCKED</promise>";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Blocked,
        );
    }

    #[test]
    fn test_reorder_beats_prompt_too_long() {
        let output = "Prompt is too long\n<reorder>FEAT-005</reorder>";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Reorder("FEAT-005".to_string()),
        );
    }

    #[test]
    fn test_rate_limit_beats_prompt_too_long() {
        // Rate-limit check runs before prompt-too-long
        let output = "rate_limit_error\nPrompt is too long\n";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::RateLimit,
        );
    }

    #[test]
    fn test_prompt_too_long_beats_generic_crash() {
        // Non-zero exit + prompt-too-long → PromptTooLong (not RuntimeError)
        let output = "Prompt is too long\n";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::Crash(CrashType::PromptTooLong),
        );
    }

    #[test]
    fn test_analyze_output_not_modified_by_key_decision_tag() {
        // A key-decision tag in output should NOT change IterationOutcome
        let output = make_kd("DB Choice", "Which DB?", &[("A: SQLite", "easy")]);
        let result = analyze_output(&output, 0, &test_dir());
        assert_eq!(result, IterationOutcome::NoEligibleTasks);
    }

    // ======================================================================
    // `<task-status>` side-band extraction tests (FEAT-003)
    // ======================================================================

    #[test]
    fn test_extract_status_updates_single() {
        let output = "<task-status>FEAT-001:done</task-status>";
        let updates = extract_status_updates(output);
        assert_eq!(
            updates,
            vec![TaskStatusUpdate {
                task_id: "FEAT-001".to_string(),
                status: TaskStatusChange::Done,
            }],
        );
    }

    #[test]
    fn test_extract_status_updates_multiple() {
        // Three tags in one output; must be returned in document order.
        let output = "noise <task-status>FEAT-001:done</task-status> \
                      and <task-status>FEAT-002:failed</task-status> \
                      plus <task-status>FEAT-003:skipped</task-status> trailing";
        let updates = extract_status_updates(output);
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].task_id, "FEAT-001");
        assert_eq!(updates[0].status, TaskStatusChange::Done);
        assert_eq!(updates[1].task_id, "FEAT-002");
        assert_eq!(updates[1].status, TaskStatusChange::Failed);
        assert_eq!(updates[2].task_id, "FEAT-003");
        assert_eq!(updates[2].status, TaskStatusChange::Skipped);
    }

    #[test]
    fn test_extract_status_updates_case_insensitive_status() {
        for keyword in ["done", "DONE", "Done", "DoNe"] {
            let output = format!("<task-status>FEAT-001:{keyword}</task-status>");
            let updates = extract_status_updates(&output);
            assert_eq!(
                updates,
                vec![TaskStatusUpdate {
                    task_id: "FEAT-001".to_string(),
                    status: TaskStatusChange::Done,
                }],
                "status '{}' should parse as Done",
                keyword,
            );
        }
    }

    #[test]
    fn test_extract_status_updates_malformed_skipped() {
        // No colon → malformed body → skipped; following well-formed tag is still parsed.
        let output = "<task-status>FEAT-001-NO-COLON</task-status> \
                      <task-status>FEAT-002:done</task-status>";
        let updates = extract_status_updates(output);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].task_id, "FEAT-002");
        assert_eq!(updates[0].status, TaskStatusChange::Done);
    }

    #[test]
    fn test_extract_status_updates_unknown_status_skipped() {
        let output = "<task-status>FEAT-001:exploded</task-status> \
                      <task-status>FEAT-002:done</task-status>";
        let updates = extract_status_updates(output);
        assert_eq!(updates.len(), 1, "unknown status must not dispatch");
        assert_eq!(updates[0].task_id, "FEAT-002");
    }

    #[test]
    fn test_extract_status_updates_empty_id_skipped() {
        let output = "<task-status>:done</task-status> \
                      <task-status>FEAT-002:done</task-status>";
        let updates = extract_status_updates(output);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].task_id, "FEAT-002");
    }

    #[test]
    fn test_extract_status_updates_whitespace_trimmed() {
        let output = "<task-status>  FEAT-001  :  done  </task-status>";
        let updates = extract_status_updates(output);
        assert_eq!(
            updates,
            vec![TaskStatusUpdate {
                task_id: "FEAT-001".to_string(),
                status: TaskStatusChange::Done,
            }],
        );
    }

    #[test]
    fn test_extract_status_updates_two_tags_not_greedy_matched() {
        // Learning [193]/known-bad: a naive find() that closes on the LAST
        // </task-status> would produce one giant task_id. The open+close slice
        // advance pattern must yield TWO separate updates.
        let output = "<task-status>A:done</task-status> noise <task-status>B:done</task-status>";
        let updates = extract_status_updates(output);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].task_id, "A");
        assert_eq!(updates[1].task_id, "B");
    }

    #[test]
    fn test_extract_status_updates_missing_close_tag_stops_cleanly() {
        // Second tag has no </task-status>; first valid tag is kept.
        let output = "<task-status>FEAT-001:done</task-status><task-status>FEAT-002:done";
        let updates = extract_status_updates(output);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].task_id, "FEAT-001");
    }

    #[test]
    fn test_status_tag_does_not_change_iteration_outcome() {
        // Output contains BOTH a <task-status> tag and <promise>COMPLETE</promise>.
        // analyze_output must return Completed (ignoring the status tag entirely),
        // and extract_status_updates must still pick up the task-status tag.
        let output = "<task-status>FEAT-001:done</task-status>\n<promise>COMPLETE</promise>";
        let outcome = analyze_output(output, 0, &test_dir());
        assert_eq!(outcome, IterationOutcome::Completed);
        let updates = extract_status_updates(output);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].task_id, "FEAT-001");
    }

    #[test]
    fn test_status_tag_does_not_change_outcome_without_promise() {
        // Without a promise, the status tag alone must NOT push the outcome to
        // Completed — side-band tags are parsed separately from analyze_output.
        let output = "<task-status>FEAT-001:done</task-status>";
        let outcome = analyze_output(output, 0, &test_dir());
        assert_ne!(outcome, IterationOutcome::Completed);
    }
}
