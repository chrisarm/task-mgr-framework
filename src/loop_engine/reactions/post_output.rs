//! Post-output reactions: rate-limit wait + overflow recovery (CONTRACT-001).
//!
//! Two post-Claude reactions live here because both key off the runner's
//! captured output:
//!
//! - [`react_to_outputs`] — the post-output rate-limit reaction: parse a reset
//!   timestamp from the output and wait once if the account hit its limit
//!   mid-run (`usage::{parse_reset_from_output, wait_for_usage_reset}`).
//!   **Typed scaffold** under CONTRACT-001; wired by FEAT-006/FEAT-010.
//! - [`handle_overflow`] — the "Prompt is too long" five-rung recovery ladder
//!   (`overflow::handle_prompt_too_long`). **Fully wired** by CONTRACT-001:
//!   both `iteration.rs` and `slot.rs` route through it, proving the
//!   `#[deprecated]` + `#![deny(deprecated)]` lock end-to-end.

use std::path::Path;

use rusqlite::Connection;

use crate::loop_engine::engine::IterationContext;
use crate::loop_engine::overflow::{self, RecoveryAction};
use crate::loop_engine::project_config::ProjectConfig;
use crate::loop_engine::prompt::PromptResult;
use crate::loop_engine::runner::RunnerKind;

/// Inputs to [`react_to_outputs`]. Destructured exhaustively (no `..`).
///
/// **Invariant for the wiring FEAT**: a rate-limit wait/early-return here must
/// NOT zero `ctx.consecutive_merge_fail_waves` — that field is the wave
/// cascade-halt defense and is orthogonal to usage limits.
#[allow(dead_code)] // constructed by FEAT-006/FEAT-010 wiring; scaffold under CONTRACT-001
pub(crate) struct ReactToOutputsParams<'a> {
    pub ctx: &'a mut IterationContext,
    pub output: &'a str,
    pub threshold: u8,
    pub tasks_dir: &'a Path,
    pub fallback_wait: u64,
}

/// Post-output rate-limit reaction. Fires the usage wait once per wave when the
/// captured output reports a rate/session limit.
#[allow(dead_code)] // wired into both paths by FEAT-006/FEAT-010
pub(crate) fn react_to_outputs(params: ReactToOutputsParams<'_>) {
    let ReactToOutputsParams {
        ctx: _ctx,
        output: _output,
        threshold: _threshold,
        tasks_dir: _tasks_dir,
        fallback_wait: _fallback_wait,
    } = params;
}

/// Inputs to [`handle_overflow`]. Destructured exhaustively (no `..`). Mirrors
/// the twelve arguments of `overflow::handle_prompt_too_long`; `slot_index` is
/// `Some(n)` for a wave slot and `None` for the sequential path.
pub(crate) struct HandleOverflowParams<'a> {
    pub ctx: &'a mut IterationContext,
    pub conn: &'a mut Connection,
    pub task_id: &'a str,
    pub effort: Option<&'a str>,
    pub effective_model: Option<&'a str>,
    pub prompt_result: &'a PromptResult,
    pub iteration: u32,
    pub run_id: Option<&'a str>,
    pub base_dir: &'a Path,
    pub slot_index: Option<usize>,
    pub effective_runner: RunnerKind,
    pub project_config: &'a ProjectConfig,
}

/// Overflow recovery coordinator: the single home both execution paths call
/// when a task hits "Prompt is too long". Sequential passes `slot_index: None`
/// and folds the one result; wave passes `slot_index: Some(n)` per slot.
pub(crate) fn handle_overflow(params: HandleOverflowParams<'_>) -> RecoveryAction {
    let HandleOverflowParams {
        ctx,
        conn,
        task_id,
        effort,
        effective_model,
        prompt_result,
        iteration,
        run_id,
        base_dir,
        slot_index,
        effective_runner,
        project_config,
    } = params;

    // CONTRACT-001 transition window: the leaf is `#[deprecated]` and the three
    // engine files carry `#![deny(deprecated)]`, so this coordinator is the only
    // legitimate caller. The `#[allow(deprecated)]` stays until the owning FEAT
    // physically relocates the leaf body into this module.
    #[allow(deprecated)]
    overflow::handle_prompt_too_long(
        ctx,
        conn,
        task_id,
        effort,
        effective_model,
        prompt_result,
        iteration,
        run_id,
        base_dir,
        slot_index,
        effective_runner,
        project_config,
    )
}
