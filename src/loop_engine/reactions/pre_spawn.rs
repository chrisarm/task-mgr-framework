//! Pre-spawn per-task recovery resolution (CONTRACT-001 scaffold).
//!
//! Folds the two pre-dispatch per-task recovery reactions that must run at the
//! top of every iteration, BEFORE `resolve_effective_runner`:
//!
//! 1. `recovery::check_override_invalidation` — the operator escape valve
//!    (clears stale auto-recovery overrides when `tasks.model` was edited
//!    out-of-band). Load-bearing order: it MUST run before crash escalation so
//!    a fresh resolve sees cleared channels.
//! 2. `recovery::check_crash_escalation` — returns the escalated model when the
//!    previous iteration on this task crashed.
//!
//! Wired into both paths by FEAT-002/FEAT-008 (sequential: `iteration.rs`
//! ~L367/L419; wave: per-slot, folded in `wave_scheduler.rs` ~L983).

use std::collections::HashMap;

use rusqlite::Connection;

use crate::loop_engine::engine::IterationContext;
use crate::loop_engine::recovery::{check_crash_escalation, check_override_invalidation};

/// Inputs to [`resolve_task_execution`]. Destructured exhaustively (no `..`)
/// so a new field forces a deliberate update here and in every caller.
#[allow(dead_code)] // constructed by FEAT-002/FEAT-008 wiring; scaffold under CONTRACT-001
pub(crate) struct ResolveTaskExecutionParams<'a> {
    pub ctx: &'a mut IterationContext,
    pub conn: &'a Connection,
    pub task_id: &'a str,
    pub crashed_last_iteration: &'a HashMap<String, bool>,
    pub resolved_model: Option<&'a str>,
}

/// Pre-spawn recovery coordinator: invalidate stale overrides, then return any
/// crash-escalated model. Returns `None` when no escalation applies.
#[allow(dead_code)] // wired into both paths by FEAT-002/FEAT-008
pub(crate) fn resolve_task_execution(params: ResolveTaskExecutionParams<'_>) -> Option<String> {
    let ResolveTaskExecutionParams {
        ctx,
        conn,
        task_id,
        crashed_last_iteration,
        resolved_model,
    } = params;

    check_override_invalidation(ctx, conn, task_id);
    check_crash_escalation(crashed_last_iteration, task_id, resolved_model)
}
