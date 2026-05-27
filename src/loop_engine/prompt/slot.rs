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
pub use crate::loop_engine::prompt::assembler::CRITICAL_OVERFLOW_SENTINEL;
use crate::loop_engine::prompt::assembler::{
    PromptContext, Rendered, SectionKind, SectionSpec, assemble_criticals, fit_trimmable,
};
use crate::loop_engine::prompt::core;
use crate::loop_engine::prompt_sections::dependencies::dependencies_spec;
use crate::loop_engine::prompt_sections::task_ops::task_ops_spec;
use crate::loop_engine::prompt_sections::truncate_to_budget;
use crate::models::Task;

/// The slot path's ordered section roster (CONTRACT-001).
///
/// A set-subset of [`super::sequential::sequential_roster`] but independently
/// ordered in slot display order: the slot emits the task envelope FIRST,
/// whereas sequential places it mid-list. The task/completion/base_prompt
/// envelopes use slot-specific render fns (untruncated `format_task_json`, the
/// `core::completion_instruction` variant, the no-trailing-newline base-prompt
/// reader) that differ byte-for-byte from sequential's. `task_ops`,
/// `dependencies`, and (since FEAT-005) `source` / `steering` /
/// `session_guidance` / `tool_awareness` / `key_decision` are shared specs
/// reached through their `*_spec` constructors.
///
/// After FEAT-005 every section the slot path emits is in this roster: the slot
/// has NO sequential-only sections (synergy / siblings / reorder / escalation
/// are dropped for disjoint wave slots by design). As in the sequential path,
/// [`build_prompt`] does not yet call [`assemble`] once over this whole roster —
/// the critical subset is gated together while each migrated trimmable is
/// assembled solo at its own fit position. It derives a criticals-only
/// sub-roster from this single source and reaches each trimmable spec via its
/// `*_spec` constructor.
pub fn slot_roster() -> Vec<SectionSpec> {
    vec![
        SectionSpec {
            name: "task",
            kind: SectionKind::Critical,
            render: render_task_section,
        },
        task_ops_spec(),
        learnings_spec(),
        core::source_spec(),
        dependencies_spec(),
        core::steering_spec(),
        core::session_guidance_spec(),
        core::tool_awareness_spec(),
        core::key_decision_spec(),
        SectionSpec {
            name: "completion",
            kind: SectionKind::Critical,
            render: render_completion_section,
        },
        SectionSpec {
            name: "base_prompt",
            kind: SectionKind::Critical,
            render: render_base_prompt_section,
        },
    ]
}

/// Build the slot learnings [`SectionSpec`] (trimmable, capped at
/// [`LEARNINGS_BUDGET`]).
///
/// Distinct from the sequential learnings spec: the slot variant recalls its
/// own learnings from the DB via [`core::build_learnings_block`] (the
/// `recall-limit + 4 KB truncate` path), whereas the sequential render reads
/// the learnings `next::next` already selected. Both rosters place this section
/// at their own legacy display position; neither shares a render fn here.
fn learnings_spec() -> SectionSpec {
    SectionSpec {
        name: "learnings",
        kind: SectionKind::Trimmable {
            budget: LEARNINGS_BUDGET,
        },
        render: render_learnings_section,
    }
}

/// Render the slot learnings section via [`core::build_learnings_block`],
/// surfacing the recalled learning IDs as the section's side output so
/// [`assemble`] owns the clear-on-drop invariant (the IDs are dropped from
/// `Assembled.shown_learning_ids` iff the section doesn't fit). The per-section
/// budget rides on [`SectionKind::Trimmable`]; a `Critical` kind is unreachable
/// for this spec but is mapped to "no cap" defensively.
fn render_learnings_section(ctx: &PromptContext<'_>, kind: SectionKind) -> Rendered {
    let (text, shown_learning_ids) =
        core::build_learnings_block(ctx.conn, ctx.task, kind.budget_or_max());
    Rendered {
        text,
        shown_learning_ids,
    }
}

/// Render the slot task envelope (`## Current Task` JSON block) from the
/// `&Task` + preloaded `task_files` carried in [`PromptContext`]. Distinct from
/// the sequential envelope: the slot variant uses `core::format_task_json` and
/// does NOT truncate.
fn render_task_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: build_task_section(ctx.task, ctx.task_files),
        ..Default::default()
    }
}

/// Build the slot `## Current Task` section from a `&Task` + its files. Single
/// legacy site for the slot envelope bytes — [`render_task_section`] and the
/// parity test both wrap it.
pub fn build_task_section(task: &Task, files: &[String]) -> String {
    let task_json = core::format_task_json(task, files);
    format!("## Current Task\n\n```json\n{task_json}\n```\n\n")
}

/// Render the slot completion-instruction section via
/// [`core::completion_instruction`] (the slot variant — no non-code note,
/// title in the commit message).
fn render_completion_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: core::completion_instruction(&ctx.task.id, &ctx.task.title),
        ..Default::default()
    }
}

/// Render the slot base-prompt template section from
/// [`PromptContext::base_prompt_path`] via [`load_base_prompt`] (no
/// trailing-newline fixup, unlike the sequential reader).
fn render_base_prompt_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: load_base_prompt(ctx.base_prompt_path),
        ..Default::default()
    }
}

// The source-context byte budget now lives in `core::SOURCE_CONTEXT_BUDGET`
// (FEAT-005) — the single source of truth carried on the shared
// `core::source_spec` `SectionKind::Trimmable` budget, kept distinct from
// LEARNINGS_BUDGET below so the two caps never collapse into one field.

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
///
/// Single legacy site for the slot base-prompt bytes — [`render_base_prompt_section`]
/// and the parity test both wrap it.
pub fn load_base_prompt(base_prompt_path: &std::path::Path) -> String {
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
///   priority order. Since FEAT-005 every trimmable (`learnings`, `source`,
///   `dependencies`, `steering`, `session_guidance`, `tool_awareness`,
///   `key_decision`) is assembled solo via [`assemble`] at its fit position.
///   Drops are recorded in `dropped_sections`. When `"learnings"` is dropped,
///   [`assemble`] (the sole owner of this invariant) leaves `shown_learning_ids`
///   empty so the UCB bandit isn't credited with learnings the agent never saw.
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

    // The `&Connection` lives only in this main-thread `PromptContext`, which is
    // dropped before the bundle crosses the worker boundary — no `&Connection`
    // is ever stored in the `Send`-safe `SlotPromptBundle`. Reused for both the
    // Phase 1 critical assemble and the Phase 2 deps assemble.
    let ctx = PromptContext {
        conn,
        task,
        task_files: &task_files,
        project_root: &params.project_root,
        base_prompt_path: &params.base_prompt_path,
        permission_mode: &params.permission_mode,
        steering_path: params.steering_path,
        session_guidance: params.session_guidance,
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: None,
        resolved_model: None,
        next_task_output: None,
        recalled_learnings: None,
    };

    let roster = slot_roster();

    // ============================================================
    // Phase 1: critical sections — gated together against the budget.
    // ============================================================
    //
    // The migrated criticals (task, task_ops, completion, base_prompt) render
    // through `assemble` over the criticals-only sub-roster. The slot path has
    // NO inlined criticals, so the full `TOTAL_PROMPT_BUDGET` is the gate.
    // `assemble` signals overflow uniformly via
    // `dropped_sections == [CRITICAL_OVERFLOW_SENTINEL]`; unlike the sequential
    // caller (which maps it to `Err`), the slot caller keeps its
    // sentinel-in-bundle behavior so the wave scheduler skips the slot.
    let critical_assembled = assemble_criticals(&ctx, &roster, TOTAL_PROMPT_BUDGET);

    if critical_assembled
        .dropped_sections
        .iter()
        .any(|s| s == CRITICAL_OVERFLOW_SENTINEL)
    {
        let critical_total: usize = critical_assembled
            .section_sizes
            .iter()
            .map(|(_, n)| n)
            .sum();
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
            // `assemble` populates `section_sizes` on overflow (criticals in
            // roster order), the same breakdown the legacy sentinel reported.
            section_sizes: critical_assembled.section_sizes,
            dropped_sections: vec![CRITICAL_OVERFLOW_SENTINEL.to_string()],
        };
    }

    // Pull each migrated critical's bytes out for interleaved stitching below.
    let task_section = critical_assembled.section_text("task");
    let task_ops = critical_assembled.section_text("task_ops");
    let completion_section = critical_assembled.section_text("completion");
    let base_prompt = critical_assembled.section_text("base_prompt");

    // ============================================================
    // Phase 2: trimmable sections — fit into the remaining budget.
    // ============================================================

    let mut remaining = TOTAL_PROMPT_BUDGET - critical_assembled.prompt.len();
    let mut dropped_sections: Vec<String> = Vec::new();

    // Priority order (highest first):
    //   1. learnings, 2. source, 3. dependencies, 4. steering,
    //   5. session_guidance, 6. tool_awareness, 7. key_decision
    //
    // Learnings and dependencies are migrated trimmables; each is assembled
    // SOLO at its own fit position (they are separated in fit order by the
    // still-inlined `source`, so a single multi-spec assemble would reorder
    // them). Learnings sits first because its `shown_learning_ids` clear-on-drop
    // (now owned entirely by `assemble`) only matters when the section didn't
    // fit; keeping it first maximizes the chance the agent sees recalled context.

    // Learnings (trimmable, fit FIRST). `fit_trimmable` / `assemble` is the SOLE
    // owner of the shown_learning_ids clear-on-drop: the IDs survive iff the
    // section fit, so the bandit is never credited with learnings the agent never
    // saw — no per-builder `.clear()` here (FEAT-004 centralization).
    let learnings_rendered = fit_trimmable(
        &ctx,
        learnings_spec(),
        &mut remaining,
        &mut dropped_sections,
    );
    let learnings_section = learnings_rendered.text;
    let shown_learning_ids = learnings_rendered.shown_learning_ids;

    // Source context (trimmable, fit AFTER learnings). Its 2000-byte internal cap
    // rides on the spec's `SectionKind::Trimmable` budget, distinct from
    // LEARNINGS_BUDGET.
    let source_section = fit_trimmable(
        &ctx,
        core::source_spec(),
        &mut remaining,
        &mut dropped_sections,
    )
    .text;

    // Dependencies (trimmable, fit AFTER learnings + source).
    let dep_section = fit_trimmable(
        &ctx,
        dependencies_spec(),
        &mut remaining,
        &mut dropped_sections,
    )
    .text;

    // Steering (trimmable).
    let steering_section = fit_trimmable(
        &ctx,
        core::steering_spec(),
        &mut remaining,
        &mut dropped_sections,
    )
    .text;

    // Session guidance (trimmable).
    let guidance_section = fit_trimmable(
        &ctx,
        core::session_guidance_spec(),
        &mut remaining,
        &mut dropped_sections,
    )
    .text;

    // Tool awareness (trimmable).
    let tool_section = fit_trimmable(
        &ctx,
        core::tool_awareness_spec(),
        &mut remaining,
        &mut dropped_sections,
    )
    .text;

    // Key decision points (trimmable, fit LAST).
    let key_decisions_section = fit_trimmable(
        &ctx,
        core::key_decision_spec(),
        &mut remaining,
        &mut dropped_sections,
    )
    .text;

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
