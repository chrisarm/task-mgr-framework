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

/// CLI-error provenance for [`analyze_output`].
///
/// One struct so callers do not pass a same-typed bool pair. `task_id` and
/// `run_id` are warn-event correlation only; they do not change the outcome.
#[derive(Debug, Clone)]
pub struct OutputSignals {
    /// `result.is_error` or `StreamEvent::Error`. Wins over `completion_killed`.
    pub cli_error: bool,
    /// Post-completion grace kill. Suppresses only the bare `exit_code != 0` arm.
    pub completion_killed: bool,
    /// Retained CLI error string. Scanned instead of `output` when `cli_error`.
    pub error_text: Option<String>,
    /// Claimed task, when the call site has one.
    pub task_id: Option<String>,
    /// Loop run id, when the call site has one. Omitted from the warn when `None`.
    pub run_id: Option<String>,
}

/// Text-classifier hit, in the historical step 3 / 3.5 / 3.6 order.
///
/// The warn event and the outcome share this order so a suppressed match
/// names the classifier that would have been returned.
#[derive(Clone, Copy)]
enum GatedClassifier {
    RateLimit,
    PromptTooLong,
    TransientBackend,
}

impl GatedClassifier {
    fn name(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::PromptTooLong => "prompt_too_long",
            Self::TransientBackend => "transient_backend",
        }
    }

    fn outcome(self, text: &str) -> IterationOutcome {
        match self {
            Self::RateLimit => IterationOutcome::RateLimit,
            Self::PromptTooLong => IterationOutcome::Crash(CrashType::PromptTooLong),
            Self::TransientBackend => IterationOutcome::TransientBackend {
                retry_after_secs: parse_retry_after_secs(text),
            },
        }
    }
}

fn first_gated_classifier(text: &str) -> Option<GatedClassifier> {
    // Step 3: rate-limit patterns (429, usage limit).
    if is_rate_limited(text) {
        return Some(GatedClassifier::RateLimit);
    }
    // Step 3.5: "Prompt is too long" before generic crash classification.
    // The CLI emits this when the conversation exceeds the context window;
    // the engine downgrades effort and resets the task.
    if is_prompt_too_long(text) {
        return Some(GatedClassifier::PromptTooLong);
    }
    // Step 3.6: transient backend failures (HTTP 502/503/504, Bad Gateway,
    // Service Unavailable, overloaded_error / HTTP 529) before the generic
    // non-zero-exit crash branch. A 5xx is "try again later", not a task
    // failure — `reactions::account::react_to_transient` backs off.
    if is_transient_backend(text) {
        return Some(GatedClassifier::TransientBackend);
    }
    None
}

/// FR-005. Fields are provenance only — tracing has no redactor, so the
/// event must not carry output or error text.
fn warn_text_signal_without_cli_error(
    classifier: &'static str,
    exit_code: i32,
    signals: &OutputSignals,
) {
    let task_id = signals.task_id.as_deref().unwrap_or("");
    let completion_killed = signals.completion_killed;
    if let Some(run_id) = signals.run_id.as_deref() {
        tracing::warn!(
            target: "task_mgr::detection",
            event = "text_signal_without_cli_error",
            classifier,
            task_id,
            exit_code,
            completion_killed,
            run_id,
            "text_signal_without_cli_error"
        );
    } else {
        tracing::warn!(
            target: "task_mgr::detection",
            event = "text_signal_without_cli_error",
            classifier,
            task_id,
            exit_code,
            completion_killed,
            "text_signal_without_cli_error"
        );
    }
}

/// Analyze subprocess output and exit code to determine iteration outcome.
///
/// Checks patterns in priority order:
/// 1. `<promise>COMPLETE</promise>` in last 20 lines -> Completed
/// 2. `<promise>BLOCKED</promise>` in last 20 lines -> Blocked
/// 3. `<reorder>TASK-ID</reorder>` anywhere in output -> Reorder(task_id)
/// 4. Rate-limit / prompt-too-long / transient-backend, only when
///    `signals.cli_error || (exit_code != 0 && !signals.completion_killed)`.
///    A set `cli_error` scans `error_text`, not the agent summary.
/// 5. Non-zero exit code -> Crash (categorized by exit code). Exit 143 stays
///    `Crash(RuntimeError)`.
/// 6. Empty output with exit 0 -> Empty
///
/// The `dir` parameter is reserved for future DB-based verification
/// (secondary check: query remaining tasks).
pub fn analyze_output(
    output: &str,
    exit_code: i32,
    signals: &OutputSignals,
    _dir: &Path,
) -> IterationOutcome {
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

    // `cli_error` wins over a grace kill. `completion_killed` suppresses only
    // the bare non-zero-exit arm, so a grace-killed summary is not a CLI error
    // unless the CLI itself reported one.
    let has_cli_error = signals.cli_error || (exit_code != 0 && !signals.completion_killed);
    if has_cli_error {
        let scan = if signals.cli_error {
            signals.error_text.as_deref().unwrap_or("")
        } else {
            output
        };
        if let Some(class) = first_gated_classifier(scan) {
            return class.outcome(scan);
        }
    } else if let Some(class) = first_gated_classifier(output) {
        warn_text_signal_without_cli_error(class.name(), exit_code, signals);
    }

    // Step 4: Check exit code for crashes. Unchanged by the text gate:
    // exit 143 is still Crash(RuntimeError).
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
///
/// Recognizes both the classic API error (`rate_limit_error`, HTTP 429) and the
/// Claude CLI session/usage-limit messages. The CLI phrasing has drifted — in
/// addition to the original "You've hit your limit · resets ..." message, it
/// now emits variants like:
///
///   - "You've hit your org's monthly usage limit"
///   - "You've hit your org's weekly usage limit"
///   - "You've hit your session limit"
///
/// When the session limit is reached, the org's monthly/weekly limit is often
/// the cascading symptom; both must route through the usage-wait path (not
/// the crash-backoff retry path), otherwise the loop burns crash budget and
/// resets tasks instead of sleeping until the window reopens.
pub(crate) fn is_rate_limited(output: &str) -> bool {
    let output_lower = output.to_lowercase();
    output_lower.contains("rate_limit_error")
        || output_lower.contains("429")
            && (output_lower.contains("rate") || output_lower.contains("limit"))
        || output_lower.contains("usage")
            && output_lower.contains("limit")
            && output_lower.contains("reached")
        // Contiguous "usage limit" phrase — catches "monthly usage limit",
        // "weekly usage limit", "org's usage limit", etc. Won't fire on
        // "Usage statistics show the limit was high" (non-adjacent).
        || output_lower.contains("usage limit")
        // Broader "hit your ... limit" — catches the original "hit your limit"
        // and the newer "hit your org's ... limit" / "hit your session limit".
        || output_lower.contains("hit your") && output_lower.contains("limit")
        // Live CLI rung-scoped copy: "You've reached your Fable limit" (also
        // plain "You've reached your session limit"). Classification may widen
        // here; the narrow 3600 Wait override lives in
        // `reactions::account::is_rung_scoped_rate_limit_message`.
        || output_lower.contains("reached your") && output_lower.contains("limit")
}

/// Check if `text` reports a transient backend failure (FEAT-014).
///
/// Single source of truth for both execution paths: the Claude path scans
/// stdout (`analyze_output`), the Grok path scans the captured stderr
/// (`runner.rs`, where the `cli-chat-proxy.grok.com` 502 lands). A transient
/// backend error is a "retry later" signal (the API/gateway is briefly
/// unavailable or overloaded), distinct from a per-account rate limit
/// (`is_rate_limited`) — so it routes to the bounded backoff-retry reaction
/// rather than the crash-backoff path.
///
/// Recognized signals:
///
///   - Anthropic `overloaded_error` (JSON error type / HTTP 529 overloaded).
///   - The unambiguous gateway/availability phrases `bad gateway` and
///     `service unavailable` (covers the Cloudflare 502 page grok proxies).
///   - The numeric 5xx gateway/availability codes `502`/`503`/`504`/`529`,
///     gated on a backend-context word (`gateway`, `unavailable`,
///     `overloaded`, `upstream`, `cloudflare`) so a bare `503` elsewhere in
///     normal output does not trigger a false backoff.
pub(crate) fn is_transient_backend(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("overloaded_error")
        || lower.contains("bad gateway")
        || lower.contains("service unavailable")
        || (has_5xx_gateway_code(&lower) && has_backend_context(&lower))
}

/// True when `lower` (already lowercased) contains one of the transient 5xx
/// gateway/overloaded HTTP status codes.
fn has_5xx_gateway_code(lower: &str) -> bool {
    lower.contains("502") || lower.contains("503") || lower.contains("504") || lower.contains("529")
}

/// True when `lower` (already lowercased) carries a backend-error context word.
/// Pairing a 5xx code with one of these avoids firing on an arbitrary numeric
/// occurrence (e.g. "503 tests passed").
fn has_backend_context(lower: &str) -> bool {
    lower.contains("gateway")
        || lower.contains("unavailable")
        || lower.contains("overloaded")
        || lower.contains("upstream")
        || lower.contains("cloudflare")
}

/// Parse a `Retry-After` value (seconds) from `text`, if present (FEAT-014).
///
/// Recognizes the HTTP header form `Retry-After: 60` (case-insensitive, with
/// optional `:`/`=`/whitespace separators). Only the integer-seconds form is
/// parsed — an HTTP-date `Retry-After` returns `None`, so the reaction falls
/// back to its exponential backoff. Shared by the Claude (stdout) and Grok
/// (stderr) classification paths.
pub(crate) fn parse_retry_after_secs(text: &str) -> Option<u64> {
    let lower = text.to_lowercase();
    let idx = lower.find("retry-after")?;
    let after = &lower[idx + "retry-after".len()..];
    let after = after.trim_start_matches([':', '=', ' ', '\t']);
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().ok()
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
    use std::path::{Path, PathBuf};

    fn test_dir() -> PathBuf {
        PathBuf::from("/tmp/test-detection")
    }

    /// Pre-FEAT-002 call shape. `cli_error` and `completion_killed` stay false,
    /// so a non-zero exit still scans `output`. Exit 0 does not. Rewritten
    /// contract tests call [`super::analyze_output`] with explicit signals.
    fn analyze_output(output: &str, exit_code: i32, dir: &Path) -> IterationOutcome {
        super::analyze_output(
            output,
            exit_code,
            &OutputSignals {
                cli_error: false,
                completion_killed: false,
                error_text: None,
                task_id: None,
                run_id: None,
            },
            dir,
        )
    }

    fn bare_signals() -> OutputSignals {
        OutputSignals {
            cli_error: false,
            completion_killed: false,
            error_text: None,
            task_id: None,
            run_id: None,
        }
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
        let result = analyze_output(output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Blocked,
            "Should detect BLOCKED in last 20 lines"
        );
    }

    #[test]
    fn test_blocked_on_last_line() {
        let output = "Cannot proceed\n<promise>BLOCKED</promise>";
        let result = analyze_output(output, 0, &test_dir());
        assert_eq!(result, IterationOutcome::Blocked);
    }

    // --- AC 3: Reorder detection ---

    #[test]
    fn test_detects_reorder_with_task_id() {
        let output = "I think LOOP-005 would be better.\n<reorder>LOOP-005</reorder>\nDone.";
        let result = analyze_output(output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Reorder("LOOP-005".to_string()),
            "Should extract task ID from <reorder> tag"
        );
    }

    #[test]
    fn test_detects_reorder_with_different_task_id_format() {
        let output = "Suggest switching to FEAT-024.\n<reorder>FEAT-024</reorder>";
        let result = analyze_output(output, 0, &test_dir());
        assert_eq!(result, IterationOutcome::Reorder("FEAT-024".to_string()),);
    }

    #[test]
    fn test_reorder_with_whitespace_around_task_id() {
        let output = "<reorder>  LOOP-005  </reorder>";
        let result = analyze_output(output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Reorder("LOOP-005".to_string()),
            "Should trim whitespace from task ID"
        );
    }

    #[test]
    fn test_reorder_empty_tag_not_detected() {
        let output = "Some output\n<reorder></reorder>\nMore output";
        let result = analyze_output(output, 0, &test_dir());
        assert_ne!(
            result,
            IterationOutcome::Reorder(String::new()),
            "Empty reorder tag should not produce a Reorder outcome"
        );
    }

    #[test]
    fn test_reorder_missing_closing_tag_not_detected() {
        let output = "Some output\n<reorder>LOOP-005\nMore output";
        let result = analyze_output(output, 0, &test_dir());
        // Should NOT be Reorder since no closing tag
        if let IterationOutcome::Reorder(_) = result {
            panic!("Should not detect reorder without closing tag");
        }
    }

    // --- AC 4: Rate-limit detection ---

    #[test]
    fn test_detects_rate_limit_error_pattern() {
        let output = "Error: rate_limit_error - too many requests\n";
        let result = analyze_output(output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Should detect rate_limit_error pattern"
        );
    }

    #[test]
    fn test_detects_429_rate_pattern() {
        let output = "HTTP 429 rate limit exceeded\n";
        let result = analyze_output(output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Should detect 429 rate pattern"
        );
    }

    #[test]
    fn test_detects_usage_limit_reached_pattern() {
        let output = "Usage limit reached. Please wait.\n";
        let result = analyze_output(output, 1, &test_dir());
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
        let result = analyze_output(output, 0, &test_dir());
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
        let result = analyze_output(output, 0, &test_dir());
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
    fn test_monthly_usage_limit_detected_as_rate_limit() {
        // Exact cascading message observed when session limit is exhausted and
        // the org's monthly budget is also maxed. Previously mis-classified as
        // Crash(RuntimeError), which burned crash-tracker budget and reset tasks.
        let output = "You've hit your org's monthly usage limit\nYou've hit your org's monthly usage limit\n";
        let result = analyze_output(output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Monthly usage limit message must route to RateLimit (usage-wait), not Crash"
        );
    }

    #[test]
    fn test_weekly_usage_limit_detected_as_rate_limit() {
        let output = "You've hit your org's weekly usage limit · resets Monday\n";
        let result = analyze_output(output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Weekly usage limit message must route to RateLimit"
        );
    }

    #[test]
    fn test_session_limit_detected_as_rate_limit() {
        let output = "You've hit your session limit · resets 11pm\n";
        let result = analyze_output(output, 1, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::RateLimit,
            "Session limit message must route to RateLimit"
        );
    }

    #[test]
    fn test_complete_takes_priority_over_rate_limit() {
        let output = "rate_limit_error earlier\nRecovered\n<promise>COMPLETE</promise>\n";
        let result = analyze_output(output, 0, &test_dir());
        assert_eq!(
            result,
            IterationOutcome::Completed,
            "COMPLETE should take priority over rate limit pattern"
        );
    }

    #[test]
    fn test_blocked_takes_priority_over_reorder() {
        let output = "<reorder>FEAT-005</reorder>\n<promise>BLOCKED</promise>\n";
        let result = analyze_output(output, 0, &test_dir());
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
        // Live CLI copy when session/extra-usage is exhausted (no "resets" token).
        assert!(is_rate_limited(
            "You've hit your individual spend limit · run /usage-credits to raise it, or visit claude.ai/admin-settings/usage"
        ));
        // PR-1 / FR-002: live Fable / rung-scoped sentence (no "hit your", no
        // contiguous "usage limit") must classify as RateLimit, not Crash.
        assert!(is_rate_limited(
            "You've reached your Fable limit. To continue, switch models with /model."
        ));
        assert!(is_rate_limited("You've reached your Opus limit"));
        assert!(is_rate_limited("You've reached your session limit"));
    }

    #[test]
    fn test_is_rate_limited_negative() {
        assert!(!is_rate_limited("normal output"));
        assert!(!is_rate_limited("task completed successfully"));
        assert!(!is_rate_limited(""));
    }

    #[test]
    fn test_analyze_output_fable_limit_is_rate_limit_not_crash() {
        let output = "You've reached your Fable limit. To continue, switch models with /model.";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::RateLimit
        );
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
        // PRD tasks/prd-cli-error-gated-detection.md FR-002 (E1/E14): "429"
        // in the middle of a line is RateLimit only when has_cli_error.
        // Exit 0 agent prose is not RateLimit. A non-zero exit with
        // completion_killed false still scans that sentence.
        let output = "The server responded with 429 rate limiting error";
        assert!(is_rate_limited(output));
        let signals = bare_signals();
        assert_ne!(
            super::analyze_output(output, 0, &signals, &test_dir()),
            IterationOutcome::RateLimit,
        );
        assert_eq!(
            super::analyze_output(output, 1, &signals, &test_dir()),
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
        // Non-adjacent "usage" and "limit" with no "hit your" / "reached" / 429 /
        // rate_limit_error markers must not trigger. The broader "usage limit"
        // contiguous check requires the two words to be adjacent.
        let output = "Usage statistics show the limit was high";
        assert!(
            !is_rate_limited(output),
            "'usage' and 'limit' non-adjacent without other markers must not trigger"
        );
    }

    #[test]
    fn test_hit_your_without_limit_not_triggered() {
        // "hit your" alone (e.g. narrative text) must not trigger without "limit".
        let output = "You've hit your stride on this refactor";
        assert!(
            !is_rate_limited(output),
            "'hit your' without 'limit' must not trigger"
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
        // PRD tasks/prd-cli-error-gated-detection.md FR-002 / E7: exit 0
        // agent prose is not PromptTooLong. The previous assertion (exit 0
        // still classifies) was the false positive this PRD removes. A real
        // CLI overflow with is_error still classifies — see E8.
        let output = "Prompt is too long";
        assert!(is_prompt_too_long(output));
        let result = super::analyze_output(output, 0, &bare_signals(), &test_dir());
        assert_ne!(
            result,
            IterationOutcome::Crash(CrashType::PromptTooLong),
            "exit 0 prose must not classify as PromptTooLong"
        );
        assert_eq!(result, IterationOutcome::NoEligibleTasks);
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

    // ======================================================================
    // Transient backend detection (FEAT-014)
    // ======================================================================

    #[test]
    fn test_is_transient_backend_overloaded_error() {
        assert!(is_transient_backend(
            "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}"
        ));
    }

    #[test]
    fn test_is_transient_backend_bad_gateway() {
        // The Cloudflare 502 page grok's proxy returns on stderr.
        assert!(is_transient_backend(
            "cli-chat-proxy.grok.com returned 502 Bad Gateway (cloudflare)"
        ));
    }

    #[test]
    fn test_is_transient_backend_service_unavailable() {
        assert!(is_transient_backend("HTTP 503 Service Unavailable"));
    }

    #[test]
    fn test_is_transient_backend_numeric_code_needs_context() {
        // A bare 5xx code with no backend-context word must NOT trigger.
        assert!(
            !is_transient_backend("503 tests passed, 0 failed"),
            "a bare 503 without a gateway/overloaded context must not trigger"
        );
        // ...but the same code WITH context does.
        assert!(is_transient_backend("upstream returned 504 from gateway"));
        assert!(is_transient_backend(
            "HTTP 529: server overloaded, try later"
        ));
    }

    #[test]
    fn test_is_transient_backend_negative() {
        assert!(!is_transient_backend(""));
        assert!(!is_transient_backend("normal output, task completed"));
        assert!(!is_transient_backend("rate_limit_error")); // that's a rate limit, not transient
    }

    #[test]
    fn test_analyze_output_classifies_transient_backend() {
        let output = "Error: cli-chat-proxy.grok.com 502 Bad Gateway\n";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::TransientBackend {
                retry_after_secs: None
            },
            "a 502 Bad Gateway with non-zero exit must classify as TransientBackend, not Crash"
        );
    }

    #[test]
    fn test_analyze_output_transient_backend_carries_retry_after() {
        let output = "503 Service Unavailable\nRetry-After: 60\n";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::TransientBackend {
                retry_after_secs: Some(60)
            },
        );
    }

    #[test]
    fn test_rate_limit_beats_transient_backend() {
        // RateLimit is checked first (step 3) — a body with both signals routes
        // to the usage-wait path, never the transient backoff.
        let output = "rate_limit_error\n503 service unavailable\n";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::RateLimit,
            "rate limit must take priority over transient backend"
        );
    }

    #[test]
    fn test_prompt_too_long_beats_transient_backend() {
        // PromptTooLong (step 3.5) is more specific than the transient backend
        // sweep (step 3.6) and must win.
        let output = "Prompt is too long\n502 bad gateway\n";
        assert_eq!(
            analyze_output(output, 1, &test_dir()),
            IterationOutcome::Crash(CrashType::PromptTooLong),
        );
    }

    #[test]
    fn test_completed_beats_transient_backend() {
        let output = "502 bad gateway earlier\nrecovered\n<promise>COMPLETE</promise>\n";
        assert_eq!(
            analyze_output(output, 0, &test_dir()),
            IterationOutcome::Completed,
        );
    }

    #[test]
    fn test_parse_retry_after_secs() {
        assert_eq!(parse_retry_after_secs("Retry-After: 60"), Some(60));
        assert_eq!(parse_retry_after_secs("retry-after:120"), Some(120));
        assert_eq!(parse_retry_after_secs("RETRY-AFTER = 5"), Some(5));
        assert_eq!(
            parse_retry_after_secs("503\nRetry-After: 30\nmore text"),
            Some(30)
        );
    }

    #[test]
    fn test_parse_retry_after_secs_none() {
        assert_eq!(parse_retry_after_secs(""), None);
        assert_eq!(parse_retry_after_secs("no header here"), None);
        // HTTP-date form is not parsed (falls back to exponential backoff).
        assert_eq!(
            parse_retry_after_secs("Retry-After: Wed, 21 Oct 2025 07:28:00 GMT"),
            None
        );
    }

    // ======================================================================
    // CLI-error gate (PRD tasks/prd-cli-error-gated-detection.md FR-002/FR-005)
    // ======================================================================

    #[test]
    fn test_agent_summary_about_429_handling_is_not_rate_limit() {
        // E1 + E9. Incident summary (exit 0, no CLI error) is not RateLimit.
        // The warn names the classifier and the call-site ids, and carries
        // no output text.
        let output =
            "One warn line per 429 with operation, headers. See rate_limit_diagnostic_headers().";
        assert!(
            is_rate_limited(output),
            "fixture must still match the widened rate-limit patterns"
        );
        let task_id = "17a0ade1-FEAT-001";
        let run_id = "run-e1";
        let signals = OutputSignals {
            cli_error: false,
            completion_killed: false,
            error_text: None,
            task_id: Some(task_id.to_string()),
            run_id: Some(run_id.to_string()),
        };
        let capture = EventCapture::new();
        let outcome = tracing::subscriber::with_default(capture.clone(), || {
            tracing::callsite::rebuild_interest_cache();
            super::analyze_output(output, 0, &signals, &test_dir())
        });
        assert_ne!(outcome, IterationOutcome::RateLimit);
        assert_eq!(outcome, IterationOutcome::NoEligibleTasks);

        let events: Vec<CapturedEvent> = capture
            .snapshot()
            .into_iter()
            .filter(|ev| {
                ev.fields
                    .iter()
                    .any(|(n, v)| n == "event" && v == "text_signal_without_cli_error")
            })
            .collect();
        assert_eq!(
            events.len(),
            1,
            "expected exactly one gated-out warn, got {events:?}"
        );
        let ev = &events[0];
        assert_eq!(ev.target, "task_mgr::detection");
        assert_eq!(ev.level, tracing::Level::WARN);
        let allowed = [
            "event",
            "classifier",
            "task_id",
            "exit_code",
            "completion_killed",
            "run_id",
            "message",
        ];
        for (name, value) in &ev.fields {
            assert!(
                allowed.contains(&name.as_str()),
                "unexpected field {name}={value}"
            );
            assert!(
                !value.contains("rate_limit_diagnostic_headers"),
                "warn leaked output text in {name}"
            );
        }
        assert_eq!(field(ev, "classifier"), "rate_limit");
        assert_eq!(field(ev, "task_id"), task_id);
        assert_eq!(field(ev, "exit_code"), "0");
        assert_eq!(field(ev, "completion_killed"), "false");
        assert_eq!(field(ev, "run_id"), run_id);
        assert!(
            ev.fields
                .iter()
                .all(|(n, _)| n != "output" && n != "error_text")
        );
    }

    #[test]
    fn test_grace_kill_rate_limit_prose_is_crash_not_rate_limit() {
        // E2 classifier half. The done-row assertion is FEAT-006.
        // completion_killed suppresses only the bare exit != 0 arm.
        let output =
            "One warn line per 429 with operation, headers. See rate_limit_diagnostic_headers().";
        assert!(is_rate_limited(output));
        let grace = OutputSignals {
            cli_error: false,
            completion_killed: true,
            error_text: None,
            task_id: None,
            run_id: None,
        };
        assert_eq!(
            super::analyze_output(output, 143, &grace, &test_dir()),
            IterationOutcome::Crash(CrashType::RuntimeError),
        );

        // Known-bad: cli_error wins over completion_killed. The retained
        // error string is RateLimit even though the grace kill fired and
        // that sentence is not in output.
        let summary = "Post-completion grace expired, terminating the process group";
        assert!(!is_rate_limited(summary));
        let known_bad = OutputSignals {
            cli_error: true,
            completion_killed: true,
            error_text: Some("You've hit your session limit".to_string()),
            task_id: None,
            run_id: None,
        };
        assert_eq!(
            super::analyze_output(summary, 143, &known_bad, &test_dir()),
            IterationOutcome::RateLimit,
        );
    }

    #[test]
    fn test_is_error_session_limit_exit_1_is_rate_limit() {
        // E4: result is_error with the session-limit sentence, exit 1.
        let sentence = "You've hit your session limit · resets 4pm";
        let signals = OutputSignals {
            cli_error: true,
            completion_killed: false,
            error_text: Some(sentence.to_string()),
            task_id: None,
            run_id: None,
        };
        assert_eq!(
            super::analyze_output(sentence, 1, &signals, &test_dir()),
            IterationOutcome::RateLimit,
        );
    }

    #[test]
    fn test_rate_limit_from_retained_error_text_not_in_output() {
        // E5: the assistant error string is scanned. The same sentence is
        // absent from output. The inverse (prose matches, retained text does
        // not) must not classify — cli_error does not scan the summary.
        let output = "Finished the diagnostics change and committed.";
        assert!(!is_rate_limited(output));
        let signals = OutputSignals {
            cli_error: true,
            completion_killed: false,
            error_text: Some("You've hit your session limit · resets 4pm".to_string()),
            task_id: None,
            run_id: None,
        };
        assert_eq!(
            super::analyze_output(output, 1, &signals, &test_dir()),
            IterationOutcome::RateLimit,
        );

        let prose =
            "One warn line per 429 with operation, headers. See rate_limit_diagnostic_headers().";
        assert!(is_rate_limited(prose));
        let benign = OutputSignals {
            cli_error: true,
            completion_killed: false,
            error_text: Some("unrelated cli failure".to_string()),
            task_id: None,
            run_id: None,
        };
        assert_eq!(
            super::analyze_output(prose, 1, &benign, &test_dir()),
            IterationOutcome::Crash(CrashType::RuntimeError),
            "cli_error must scan error_text, not the agent summary"
        );
    }

    #[test]
    fn test_exit_0_bad_gateway_prose_is_not_transient_backend() {
        // E6.
        let output = "Bad Gateway from upstream 502";
        assert!(is_transient_backend(output));
        let result = super::analyze_output(output, 0, &bare_signals(), &test_dir());
        assert!(
            !matches!(result, IterationOutcome::TransientBackend { .. }),
            "exit 0 prose must not classify as TransientBackend, got {result:?}"
        );
        assert_eq!(result, IterationOutcome::NoEligibleTasks);
    }

    #[test]
    fn test_is_error_prompt_too_long_exit_0_scans_error_text() {
        // E8: exit 0 with is_error still classifies PromptTooLong from the
        // retained error text, not from the summary.
        let output = "migrated the caller and left a note";
        assert!(!is_prompt_too_long(output));
        let signals = OutputSignals {
            cli_error: true,
            completion_killed: false,
            error_text: Some("Prompt is too long".to_string()),
            task_id: None,
            run_id: None,
        };
        assert_eq!(
            super::analyze_output(output, 0, &signals, &test_dir()),
            IterationOutcome::Crash(CrashType::PromptTooLong),
        );
    }

    #[test]
    fn test_codex_turn_failed_rate_limit_absent_from_derive_output() {
        // E13: the rate-limit sentence lives only on the retained turn.failed
        // string. derive_output (RunnerResult.output) does not contain it.
        let output = "Codex finished the turn.";
        assert!(!is_rate_limited(output));
        let signals = OutputSignals {
            cli_error: true,
            completion_killed: false,
            error_text: Some("turn.failed: You've hit your session limit".to_string()),
            task_id: None,
            run_id: None,
        };
        assert_eq!(
            super::analyze_output(output, 1, &signals, &test_dir()),
            IterationOutcome::RateLimit,
        );
    }

    #[test]
    fn test_grok_exit_0_prose_mentioning_429_is_not_rate_limit() {
        // E14: Grok has no error events. Exit 0 prose that mentions 429 is
        // not RateLimit when cli_error is false.
        let output = "The grok proxy returned 429 because the upstream rate limit was exceeded.";
        assert!(is_rate_limited(output));
        let signals = OutputSignals {
            cli_error: false,
            completion_killed: false,
            error_text: None,
            task_id: None,
            run_id: None,
        };
        let result = super::analyze_output(output, 0, &signals, &test_dir());
        assert_ne!(result, IterationOutcome::RateLimit);
        assert_eq!(result, IterationOutcome::NoEligibleTasks);
    }

    fn field<'a>(ev: &'a CapturedEvent, name: &str) -> &'a str {
        ev.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("missing field {name} in {ev:?}"))
    }

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        target: String,
        level: tracing::Level,
        fields: Vec<(String, String)>,
    }

    #[derive(Clone)]
    struct EventCapture {
        events: std::sync::Arc<std::sync::Mutex<Vec<CapturedEvent>>>,
    }

    impl EventCapture {
        fn new() -> Self {
            Self {
                events: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn snapshot(&self) -> Vec<CapturedEvent> {
            self.events.lock().expect("event lock").clone()
        }
    }

    #[derive(Default)]
    struct FieldVisitor {
        fields: Vec<(String, String)>,
    }

    impl tracing::field::Visit for FieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }

    impl tracing::Subscriber for EventCapture {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            // Always enabled so registering this subscriber cannot stick
            // Interest::never on unrelated callsites.
            true
        }

        fn register_callsite(
            &self,
            _metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }

        fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if event.metadata().target() != "task_mgr::detection" {
                return;
            }
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            self.events.lock().expect("event lock").push(CapturedEvent {
                target: event.metadata().target().to_string(),
                level: *event.metadata().level(),
                fields: visitor.fields,
            });
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }
}
