//! Slot-mode prompt builder: composes `prompt::core` helpers into a Send-safe
//! bundle that wave workers consume after being spawned on a separate thread.
//!
//! These are placeholders (TDD scaffolding). The real implementations land in
//! FEAT-001 alongside `prompt::core`. Each helper returns a trivial empty
//! bundle so callers compile against the real signatures while
//! `tests/prompt_slot.rs` (TEST-INIT-002) drives the contract.
//!
//! Invariants the implementation MUST honor (validated by the test suite):
//! - `SlotPromptBundle: Send` — verified at compile time via
//!   `static_assertions::assert_impl_all!` in the integration tests. Adding any
//!   `Rc`, `RefCell`, or `MutexGuard` field is a contract break.
//! - `bundle.task_id == task.id` — orphan-reset accounting depends on the
//!   bundle being the source-of-truth for a slot's task id once the worker
//!   thread has been spawned.
//! - `bundle.prompt` MUST contain the `## Relevant Learnings` header when
//!   matching learnings exist in the DB; the source-context block when
//!   `touches_files` is non-empty and files exist; the tool-awareness block;
//!   and the key-decisions block.
//! - `bundle.shown_learning_ids` is non-empty whenever the learnings block
//!   was rendered (so `record_shown_learnings` gets fed by the wave path).

use std::path::PathBuf;

use rusqlite::Connection;

use crate::loop_engine::config::PermissionMode;
use crate::models::Task;

/// Parameters required to assemble a slot-mode prompt on the main thread.
///
/// Everything in here is `Send` so the resulting `SlotPromptBundle` can cross
/// the worker thread boundary without holding a `&Connection` (rusqlite is
/// `!Send`, see learnings #1893 / #1852 / #1871).
#[derive(Clone, Debug)]
pub struct SlotPromptParams {
    /// Absolute path to the project root used to resolve `touches_files` for
    /// the source-context section.
    pub project_root: PathBuf,
    /// Path to the base prompt template (`prompt.md`) appended verbatim.
    pub base_prompt_path: PathBuf,
    /// Permission mode that determines which tool-awareness block to render.
    pub permission_mode: PermissionMode,
}

/// Send-safe bundle of everything a slot worker needs to invoke Claude and
/// thread feedback back to the main thread.
///
/// Constructed on the main thread via [`build_prompt`], then moved into the
/// worker thread inside `SlotContext`. After the worker returns,
/// `shown_learning_ids` is the canonical list for `record_shown_learnings`.
#[derive(Clone, Debug)]
pub struct SlotPromptBundle {
    /// Fully assembled prompt string passed to `claude -p`.
    pub prompt: String,
    /// Task id this bundle was built for. The orphan-reset / failure
    /// accounting in `slot_failure_result` MUST use this field instead of
    /// rederiving it from a `&Task` that no longer crosses thread boundaries.
    pub task_id: String,
    /// Files the task touches, propagated from `task_files` table at build
    /// time so workers don't need a `&Connection`.
    pub task_files: Vec<String>,
    /// Learning ids surfaced in the prompt's learnings block. Threaded back
    /// to the main thread so `record_shown_learnings` can update the bandit.
    pub shown_learning_ids: Vec<i64>,
    /// Resolved model for the slot (mirrors `PromptResult::resolved_model`).
    /// `None` means "use CLI default"; `Some("")` is normalized to `None`.
    pub resolved_model: Option<String>,
}

/// Build a slot-mode prompt bundle. Runs on the main thread because it reads
/// the DB; the resulting `SlotPromptBundle` is `Send` and is moved into the
/// worker thread.
///
/// Stub: returns an empty bundle with `task_id` populated. The empty `prompt`
/// is the discriminator that the TEST-INIT-002 content assertions catch — a
/// real implementation must compose `prompt::core::build_learnings_block` +
/// `build_source_context_block` + `build_tool_awareness_block` +
/// `build_key_decisions_block` and append the base prompt.
pub fn build_prompt(
    _conn: &Connection,
    task: &Task,
    _params: &SlotPromptParams,
) -> SlotPromptBundle {
    SlotPromptBundle {
        prompt: String::new(),
        task_id: task.id.clone(),
        task_files: Vec::new(),
        shown_learning_ids: Vec::new(),
        resolved_model: None,
    }
}
