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
//!   Conversely, `shown_learning_ids` MUST be empty when "learnings" appears
//!   in `dropped_sections` — feeding the bandit with learnings the agent
//!   never saw skews UCB scoring.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::prompt::core;
use crate::loop_engine::prompt_sections::dependencies::build_dependency_section;
use crate::loop_engine::prompt_sections::task_ops::task_ops_section;
use crate::loop_engine::prompt_sections::{truncate_to_budget, try_fit_section};
use crate::models::Task;

/// Byte budget for the source-context section in slot prompts.
const SOURCE_CONTEXT_BUDGET: usize = 2000;

/// Byte budget for the learnings section in slot prompts.
const LEARNINGS_BUDGET: usize = 4000;

/// Byte budget for the base prompt template in slot prompts.
const BASE_PROMPT_BUDGET: usize = 16_000;

/// Total byte budget for the entire assembled slot prompt.
///
/// Mirrors `prompt::sequential::TOTAL_PROMPT_BUDGET` so wave slots and the
/// sequential path enforce the same aggregate cap. Without this cap, a slot
/// could hand Claude a prompt >80KB that immediately trips `PromptTooLong`
/// and consumes a wasted wave slot before the per-slot overflow ladder
/// rescues it.
const TOTAL_PROMPT_BUDGET: usize = 80_000;

/// Sentinel name pushed into `SlotPromptBundle.dropped_sections` when the
/// critical-section total alone exceeds `TOTAL_PROMPT_BUDGET`. Callers
/// should treat a bundle with this entry as too large to attempt and skip
/// the slot rather than dispatch a malformed prompt.
pub const CRITICAL_OVERFLOW_SENTINEL: &str = "CRITICAL";

/// Parameters required to assemble a slot-mode prompt on the main thread.
///
/// Everything in here is `Send` so the resulting `SlotPromptBundle` can cross
/// the worker thread boundary without holding a `&Connection` (rusqlite is
/// `!Send`, see learnings #1893 / #1852 / #1871). The borrowed fields
/// (`steering_path`, `session_guidance`) are read on the main thread before
/// `thread::spawn`; the rendered text is owned by `SlotPromptBundle.prompt`,
/// so the borrow lifetime never crosses the worker boundary.
#[derive(Clone, Debug)]
pub struct SlotPromptParams<'a> {
    /// Absolute path to the project root used to resolve `touches_files` for
    /// the source-context section.
    pub project_root: PathBuf,
    /// Path to the base prompt template (`prompt.md`) appended verbatim.
    pub base_prompt_path: PathBuf,
    /// Permission mode that determines which tool-awareness block to render.
    pub permission_mode: PermissionMode,
    /// Optional path to project-wide `steering.md`. `None` when the project
    /// has no steering file; the steering section is omitted in that case.
    pub steering_path: Option<&'a Path>,
    /// Operator pause feedback rendered as a `## Session Guidance` block.
    /// Empty string omits the section entirely (no header).
    pub session_guidance: &'a str,
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
    /// Empty whenever `dropped_sections` contains `"learnings"` so the
    /// bandit isn't credited with learnings that never reached the agent.
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
    /// Per-section byte sizes in prompt-assembly order. Mirrors
    /// `PromptResult::section_sizes` so overflow dumps include a meaningful
    /// section breakdown (instead of an empty `[]`) when a slot hits
    /// `PromptTooLong`. Static string names match the section identifiers
    /// used in the sequential builder.
    pub section_sizes: Vec<(&'static str, usize)>,
    /// Names of trimmable sections that didn't fit within
    /// `TOTAL_PROMPT_BUDGET`. Empty when every section fit. Contains the
    /// [`CRITICAL_OVERFLOW_SENTINEL`] when the critical sections alone
    /// exceed the budget; the bundle's `prompt` is empty in that case and
    /// the slot should be skipped.
    pub dropped_sections: Vec<String>,
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
/// Composes `prompt::core` helpers: steering, session guidance, learnings,
/// source context, tool awareness, key decisions, and completed dependency
/// summaries. Drops synergy cluster escalation, reorder instruction, and
/// sibling-PRD context — wave slots are disjoint by design. Steering and
/// session guidance are kept (vs the disjoint-task drops) because they are
/// project-wide and operator-driven respectively, both of which apply to
/// every slot in the wave.
///
/// Two-phase assembly mirrors `prompt::sequential::build_prompt`:
/// - **Phase 1**: assemble the critical sections (task JSON, task ops,
///   completion instruction, base prompt template). If they alone exceed
///   `TOTAL_PROMPT_BUDGET`, return a sentinel bundle with empty `prompt`
///   and [`CRITICAL_OVERFLOW_SENTINEL`] in `dropped_sections`.
/// - **Phase 2**: fill the remaining budget with trimmable sections in
///   priority order via [`try_fit_section`]; record any drops in
///   `dropped_sections`. When `"learnings"` is dropped, `shown_learning_ids`
///   is cleared so the UCB bandit isn't credited with learnings the agent
///   never saw.
pub fn build_prompt(
    conn: &Connection,
    task: &Task,
    params: &SlotPromptParams<'_>,
) -> SlotPromptBundle {
    let task_files = load_task_files(conn, &task.id);

    let resolved_model = task
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .map(str::to_owned);

    // ============================================================
    // Phase 1: critical sections — must always be present.
    // ============================================================

    let task_json = core::format_task_json(task, &task_files);
    let task_section = format!("## Current Task\n\n```json\n{task_json}\n```\n\n");

    let task_ops = task_ops_section().to_string();

    let completion_section = core::completion_instruction(&task.id, &task.title);

    let base_prompt = load_base_prompt(&params.base_prompt_path);

    let critical_total =
        task_section.len() + task_ops.len() + completion_section.len() + base_prompt.len();

    if critical_total > TOTAL_PROMPT_BUDGET {
        eprintln!(
            "Warning: slot prompt critical sections ({} bytes) exceed TOTAL_PROMPT_BUDGET ({}) \
             for task {} — slot should be skipped",
            critical_total, TOTAL_PROMPT_BUDGET, task.id,
        );
        return SlotPromptBundle {
            prompt: String::new(),
            task_id: task.id.clone(),
            task_files,
            shown_learning_ids: Vec::new(),
            resolved_model,
            difficulty: task.difficulty.clone(),
            section_sizes: vec![
                ("task", task_section.len()),
                ("task_ops", task_ops.len()),
                ("completion", completion_section.len()),
                ("base_prompt", base_prompt.len()),
            ],
            dropped_sections: vec![CRITICAL_OVERFLOW_SENTINEL.to_string()],
        };
    }

    // ============================================================
    // Phase 2: trimmable sections — fit into the remaining budget.
    // ============================================================

    let mut remaining = TOTAL_PROMPT_BUDGET - critical_total;
    let mut dropped_sections: Vec<String> = Vec::new();

    // Priority order (highest first):
    //   1. learnings, 2. source, 3. dependencies, 4. steering,
    //   5. session_guidance, 6. tool_awareness, 7. key_decision
    //
    // Learnings sit at the top because the bandit feedback gating
    // (`shown_learning_ids` cleared when "learnings" drops) only fires when
    // the section actually didn't fit; keeping it first maximizes the
    // chance the agent sees recalled context.

    let (learnings_section_raw, mut shown_learning_ids) =
        core::build_learnings_block(conn, task, LEARNINGS_BUDGET);
    let learnings_section = try_fit_section(
        learnings_section_raw,
        "learnings",
        &mut remaining,
        &mut dropped_sections,
    );
    if learnings_section.is_empty() && dropped_sections.last().is_some_and(|s| s == "learnings") {
        // Drop bandit feedback when the learnings block didn't fit — the
        // agent never saw these recall results, so crediting them would
        // skew UCB scoring. Empty-input case (no recall results) leaves
        // shown_learning_ids untouched (already empty) since try_fit_section
        // does not push the section name on empty input.
        shown_learning_ids.clear();
    }

    let source_section_raw =
        core::build_source_context_block(&task_files, SOURCE_CONTEXT_BUDGET, &params.project_root);
    let source_section = try_fit_section(
        source_section_raw,
        "source",
        &mut remaining,
        &mut dropped_sections,
    );

    let dep_section_raw = build_dependency_section(conn, &task.id);
    let dep_section = try_fit_section(
        dep_section_raw,
        "dependencies",
        &mut remaining,
        &mut dropped_sections,
    );

    let steering_section_raw = params
        .steering_path
        .map(core::build_steering_block)
        .unwrap_or_default();
    let steering_section = try_fit_section(
        steering_section_raw,
        "steering",
        &mut remaining,
        &mut dropped_sections,
    );

    let guidance_section_raw = core::build_session_guidance_block(params.session_guidance);
    let guidance_section = try_fit_section(
        guidance_section_raw,
        "session_guidance",
        &mut remaining,
        &mut dropped_sections,
    );

    let tool_section_raw = core::build_tool_awareness_block(&params.permission_mode);
    let tool_section = try_fit_section(
        tool_section_raw,
        "tool_awareness",
        &mut remaining,
        &mut dropped_sections,
    );

    let key_decisions_section_raw = core::build_key_decisions_block(&task.id);
    let key_decisions_section = try_fit_section(
        key_decisions_section_raw,
        "key_decision",
        &mut remaining,
        &mut dropped_sections,
    );

    // ============================================================
    // Assembly
    // ============================================================
    // Display order matches sequential.rs: steering → guidance precede
    // tool_awareness so project-wide guidance lands before per-task content.
    let prompt = format!(
        "{task_section}{task_ops}{learnings_section}{source_section}{dep_section}{steering_section}{guidance_section}{tool_section}{key_decisions_section}{completion_section}{base_prompt}"
    );

    let section_sizes: Vec<(&'static str, usize)> = vec![
        ("task", task_section.len()),
        ("task_ops", task_ops.len()),
        ("learnings", learnings_section.len()),
        ("source", source_section.len()),
        ("dependencies", dep_section.len()),
        ("steering", steering_section.len()),
        ("session_guidance", guidance_section.len()),
        ("tool_awareness", tool_section.len()),
        ("key_decision", key_decisions_section.len()),
        ("completion", completion_section.len()),
        ("base_prompt", base_prompt.len()),
    ];

    SlotPromptBundle {
        prompt,
        task_id: task.id.clone(),
        task_files,
        shown_learning_ids,
        resolved_model,
        difficulty: task.difficulty.clone(),
        section_sizes,
        dropped_sections,
    }
}
