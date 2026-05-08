//! Slot-mode prompt builder: composes `prompt::core` helpers into a Send-safe
//! bundle that wave workers consume after being spawned on a separate thread.
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

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;

use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::prompt::core;
use crate::loop_engine::prompt_sections::dependencies::build_dependency_section;
use crate::loop_engine::prompt_sections::task_ops::task_ops_section;
use crate::loop_engine::prompt_sections::truncate_to_budget;
use crate::models::Task;

/// Byte budget for the source-context section in slot prompts.
const SOURCE_CONTEXT_BUDGET: usize = 2000;

/// Byte budget for the learnings section in slot prompts.
const LEARNINGS_BUDGET: usize = 4000;

/// Byte budget for the base prompt template in slot prompts.
const BASE_PROMPT_BUDGET: usize = 16_000;

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
    /// Task difficulty at bundle-build time. The slot worker derives effort
    /// (`model::effort_for_difficulty`) and watchdog timeout
    /// (`watchdog::TimeoutConfig::from_difficulty`) from this without needing
    /// the original `Task` reference. `None` when the task has no difficulty
    /// set; downstream callers fall back to defaults.
    pub difficulty: Option<String>,
}

/// Load file paths for a task from the `task_files` join table.
///
/// Returns an empty vec on any DB error (graceful degradation — a missing
/// source-context section is better than aborting prompt assembly).
fn load_task_files(conn: &Connection, task_id: &str) -> Vec<String> {
    let mut stmt = match conn
        .prepare("SELECT file_path FROM task_files WHERE task_id = ?1 ORDER BY file_path")
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: failed to prepare task_files query: {e}");
            return Vec::new();
        }
    };

    stmt.query_map([task_id], |row| row.get(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_else(|e| {
            eprintln!("Warning: failed to query task_files for {task_id}: {e}");
            Vec::new()
        })
}

/// Read and truncate the base prompt template. Returns `""` on IO failure.
fn load_base_prompt(base_prompt_path: &std::path::Path) -> String {
    match fs::read_to_string(base_prompt_path) {
        Ok(content) => truncate_to_budget(&content, BASE_PROMPT_BUDGET),
        Err(e) => {
            eprintln!(
                "Warning: failed to read base prompt {}: {e}",
                base_prompt_path.display()
            );
            String::new()
        }
    }
}

/// Build a slot-mode prompt bundle. Runs on the main thread because it reads
/// the DB; the resulting `SlotPromptBundle` is `Send` and is moved into the
/// worker thread.
///
/// Composes `prompt::core` helpers: learnings, source context, tool awareness,
/// key decisions, and completed dependency summaries. Drops synergy cluster
/// escalation, reorder instruction, and sibling-PRD context — wave slots are
/// disjoint by design.
pub fn build_prompt(conn: &Connection, task: &Task, params: &SlotPromptParams) -> SlotPromptBundle {
    let task_files = load_task_files(conn, &task.id);

    // Task JSON header — the agent must see the task's description,
    // acceptance criteria, and notes to do anything useful. Mirrors the
    // sequential builder's `## Current Task` section so parallel slots have
    // parity with the canonical single-task path.
    let task_json = core::format_task_json(task, &task_files);
    let task_section = format!("## Current Task\n\n```json\n{task_json}\n```\n\n");

    let task_ops = task_ops_section();

    let (learnings_section, shown_learning_ids) =
        core::build_learnings_block(conn, task, LEARNINGS_BUDGET);

    let source_section =
        core::build_source_context_block(&task_files, SOURCE_CONTEXT_BUDGET, &params.project_root);

    let tool_section = core::build_tool_awareness_block(&params.permission_mode);

    let key_decisions_section = core::build_key_decisions_block(task);

    let dep_section = build_dependency_section(conn, &task.id);

    let completion_section = core::completion_instruction(&task.id, &task.title);

    let base_prompt = load_base_prompt(&params.base_prompt_path);

    let prompt = format!(
        "{task_section}{task_ops}{learnings_section}{source_section}{dep_section}{tool_section}{key_decisions_section}{completion_section}{base_prompt}"
    );

    let resolved_model = task
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .map(str::to_owned);

    SlotPromptBundle {
        prompt,
        task_id: task.id.clone(),
        task_files,
        shown_learning_ids,
        resolved_model,
        difficulty: task.difficulty.clone(),
    }
}
