//! Shared post-Claude pipeline used by both the sequential `run_iteration`
//! path and the parallel-slot `process_slot_result` path.
//!
//! TDD scaffolding for FEAT-003 (the pipeline unification). This module is a
//! deliberate stub: [`process_iteration_output`] returns
//! [`ProcessingOutcome::default()`] so callers compile while
//! `tests/iteration_pipeline.rs` (TEST-INIT-003) drives the contract.
//!
//! Invariants the implementation MUST honor (validated by the test suite):
//! - Calls `learnings::ingestion::extract_learnings_from_output` (governed by
//!   the `TASK_MGR_NO_EXTRACT_LEARNINGS=1` opt-out).
//! - Calls `feedback::record_iteration_feedback` for `shown_learning_ids`.
//! - Honors `skip_git_completion_detection` for both wave (true) and
//!   sequential (false) paths.
//! - The "already complete" fallback fires in BOTH skip-git modes (this is
//!   the wave-mode parity fix called out in the PRD).
//! - `ProcessingOutcome.tasks_completed` dedups across the multiple
//!   completion branches in a single call (matches today's
//!   `process_slot_result` HashSet semantics).
//! - On retroactive completion, mutates `params.outcome` to
//!   `IterationOutcome::Completed` (matches sequential at engine.rs:3280,
//!   3307, 3341, 3400, 3454).
//! - NEVER invokes merge / external-git / wrapper-commit operations —
//!   those stay at `run_loop` / `run_wave_iteration` call sites.

use std::path::Path;

use rusqlite::Connection;

use crate::loop_engine::config::IterationOutcome;
use crate::loop_engine::engine::IterationContext;
use crate::loop_engine::signals::SignalFlag;

/// Aggregated results from one pass through the pipeline.
///
/// Mirrors the per-slot bookkeeping `process_slot_result` keeps today and
/// the per-iteration counters that `run_loop` accumulates in its sequential
/// post-Claude block.
#[derive(Debug, Default)]
pub struct ProcessingOutcome {
    /// Number of distinct task IDs the pipeline marked done in this pass.
    /// Deduped across `<task-status>:done`, `<completed>`, git-detection,
    /// output-scan, and the already-complete fallback branches.
    pub tasks_completed: u32,
    /// Every distinct task ID that the pipeline marked done in this pass.
    /// Includes the originally-claimed task AND any cross-task
    /// `<completed>Y</completed>` IDs the slot/iteration emitted.
    pub completed_task_ids: Vec<String>,
    /// Number of `<key-decision>` tags successfully extracted and stored.
    pub key_decisions_count: u32,
    /// Number of `<task-status>` tags successfully applied.
    pub status_updates_applied: u32,
    /// Number of new learnings extracted from output. Always 0 when the
    /// `TASK_MGR_NO_EXTRACT_LEARNINGS` env opt-out is in effect.
    pub learnings_extracted: usize,
}

/// Inputs to [`process_iteration_output`]. Carries every reference the
/// pipeline needs across both the sequential and wave call sites.
///
/// Lifetime `'a` ties every borrow together; the struct must always be moved
/// (consumed) into the function call. Holding `&mut Connection`,
/// `&mut IterationOutcome`, and `&mut IterationContext` simultaneously is
/// permitted because the caller hands those out once and never aliases them
/// during the call.
pub struct ProcessingParams<'a> {
    /// Database connection. `&mut` because `apply_status_updates` and
    /// `mark_task_done` take `&mut Connection`.
    pub conn: &'a mut Connection,
    /// Run ID for telemetry / completion provenance.
    pub run_id: &'a str,
    /// 1-based iteration number (used by progress logging and key-decisions
    /// insertion).
    pub iteration: u32,
    /// Task ID the iteration was claimed against, if any. `None` matches the
    /// "no claimed task" early return in the sequential path.
    pub task_id: Option<&'a str>,
    /// Raw stdout from Claude. Source for `<completed>` / `<task-status>` /
    /// `<key-decision>` parsing AND for the already-complete fallback.
    pub output: &'a str,
    /// Optional structured stream-json conversation (preferred input for
    /// learning extraction when present, falls back to `output` otherwise).
    pub conversation: Option<&'a str>,
    /// Learnings shown to Claude this iteration; threaded back from
    /// `PromptResult.shown_learning_ids` (sequential) or
    /// `SlotPromptBundle.shown_learning_ids` (wave).
    pub shown_learning_ids: &'a [i64],
    /// Mutable iteration outcome. The pipeline MAY upgrade this to
    /// `Completed` when retroactive completion is detected (see invariants).
    pub outcome: &'a mut IterationOutcome,
    /// Working directory used for git-commit detection. In wave mode this is
    /// the slot's ephemeral worktree (which has the commit but on a branch
    /// not yet merged — hence the skip flag).
    pub working_root: &'a Path,
    /// Number of `git log` entries to scan for the `-completed` suffix.
    pub git_scan_depth: usize,
    /// Wave mode passes `true` so the pipeline never inspects git history
    /// during the per-slot pass — git-commit detection runs once at the
    /// `run_wave_iteration` boundary after merges complete.
    /// Sequential mode passes `false`.
    ///
    /// Critical: the already-complete fallback MUST fire in both modes
    /// (this is the wave-mode parity fix the PRD calls out).
    pub skip_git_completion_detection: bool,
    /// Path to the PRD JSON for `passes: true` reconciliation via
    /// `update_prd_task_passes` and `mark_task_done`.
    pub prd_path: &'a Path,
    /// PRD task prefix (e.g. "5d1118de") for ID normalization.
    pub task_prefix: Option<&'a str>,
    /// Path to the per-PRD progress log so the pipeline can attribute
    /// status-tag dispatch.
    pub progress_path: &'a Path,
    /// `--dir` (DB directory) for embedding scheduling on extracted
    /// learnings via `LearningWriter`.
    pub db_dir: &'a Path,
    /// Signal flag, threaded through to `extract_learnings_from_output` so
    /// Ctrl-C aborts the extraction subprocess.
    pub signal_flag: &'a SignalFlag,
    /// Iteration context. The pipeline updates `crash_tracker` and
    /// `last_files` (matching the sequential post-Claude block).
    pub ctx: &'a mut IterationContext,
}

/// Stub: returns an empty [`ProcessingOutcome`]. Real implementation lands
/// in FEAT-003 and will:
///
/// 1. Extract `<key-decision>` tags and persist them.
/// 2. Apply `<task-status>` tags (dedup across status / completed paths).
/// 3. Mark `<completed>` task IDs done.
/// 4. (Sequential only — `skip_git_completion_detection == false`) attempt
///    git-commit detection for the claimed task.
/// 5. Fall back to output-scan for completed task IDs.
/// 6. Always run the "already complete" fallback when no path marked done.
/// 7. Mutate `params.outcome` to `Completed` if any path retroactively
///    marked the claimed task done.
/// 8. Extract learnings from `conversation` (preferred) or `output`.
/// 9. Record bandit feedback for `shown_learning_ids`.
///
/// See `tests/iteration_pipeline.rs` for the contract.
pub fn process_iteration_output(_params: ProcessingParams<'_>) -> ProcessingOutcome {
    ProcessingOutcome::default()
}
