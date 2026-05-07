//! Bedrock prompt section helpers shared by sequential and slot prompt builders.
//!
//! These are placeholders (TDD scaffolding). The real implementations are
//! lifted out of `prompt.rs` / `prompt_sections/*` in FEAT-001. Each helper
//! returns a trivial empty value so callers compile against the real
//! signatures while `tests/prompt_core.rs` (TEST-INIT-001) drives the
//! contract.
//!
//! Invariants the implementations MUST honor (validated by the test suite):
//! - `format_task_json` produces JSON that round-trips via
//!   `serde_json::from_str` and includes `id`, `title`, and `files`.
//! - `completion_instruction` mentions both the task ID and the title.
//! - `build_learnings_block` returns `("", vec![])` on retrieval failure
//!   (e.g. missing FTS5 table on a partially migrated DB) — no panics.
//! - `build_source_context_block` returns `""` when `project_root` does not
//!   exist (graceful degradation, not an error).
//! - `build_tool_awareness_block` and `build_key_decisions_block` produce
//!   non-empty content for valid inputs; the empty stub must fail tests.

use std::path::Path;

use rusqlite::Connection;

use crate::loop_engine::config::PermissionMode;
use crate::models::Task;

/// Format a task as a JSON string suitable for the prompt's task block.
///
/// Stub: returns an empty string. Real implementation must include
/// `id`, `title`, `files`, and optional fields when present.
pub fn format_task_json(_task: &Task, _files: &[String]) -> String {
    String::new()
}

/// Build the completion-instruction section that tells the agent how to
/// signal task completion (commit message + `<completed>` tag).
///
/// Stub: returns an empty string. Real implementation must reference
/// both the task ID and title.
pub fn completion_instruction(_task_id: &str, _title: &str) -> String {
    String::new()
}

/// Build the learnings block by recalling task-relevant learnings and
/// formatting them. Returns the rendered section plus the IDs of the
/// learnings that were shown (so the caller can record bandit feedback).
///
/// Stub: returns `("", vec![])`. Real implementation MUST return
/// `("", vec![])` on retrieval errors (e.g. missing `learnings_fts`)
/// — never panic.
pub fn build_learnings_block(
    _conn: &Connection,
    _task: &Task,
    _budget: usize,
) -> (String, Vec<i64>) {
    (String::new(), Vec::new())
}

/// Build the source-context block by scanning `touches_files` rooted at
/// `project_root`. Returns `""` when `project_root` does not exist.
///
/// Stub: returns an empty string.
pub fn build_source_context_block(
    _touches_files: &[String],
    _budget: usize,
    _project_root: &Path,
) -> String {
    String::new()
}

/// Build the tool-awareness block describing the tools the agent has
/// access to under `permission_mode`.
///
/// Stub: returns an empty string.
pub fn build_tool_awareness_block(_permission_mode: &PermissionMode) -> String {
    String::new()
}

/// Build the key-decision-points instruction block. For tasks whose ID
/// contains "REVIEW" or "VERIFY", the implementation must add review-flavored
/// emphasis.
///
/// Stub: returns an empty string.
pub fn build_key_decisions_block(_task: &Task) -> String {
    String::new()
}
