//! Post-completion reactions (CONTRACT-001 scaffold).
//!
//! After a wave's slot results (or the sequential iteration's single result)
//! are processed, completion-driven reactions fire — chiefly triggering any
//! human-review CLARIFY tasks that the just-completed work unblocked. The leaf
//! today is `orchestrator::trigger_human_reviews` (a private fn called from
//! `orchestrator.rs` ~L1541). FEAT (013) relocates that body into this module
//! and wires both paths through this coordinator.

use rusqlite::Connection;

/// Inputs to [`react_to_completions`]. Destructured exhaustively (no `..`).
#[allow(dead_code)] // constructed by FEAT-013 wiring; scaffold under CONTRACT-001
pub(crate) struct ReactToCompletionsParams<'a> {
    pub conn: &'a mut Connection,
    pub run_id: &'a str,
}

/// Post-completion coordinator: fold completion-driven reactions (human-review
/// triggers) for the wave's N results or the sequential path's 1.
#[allow(dead_code)] // wired into both paths by FEAT-013
pub(crate) fn react_to_completions(params: ReactToCompletionsParams<'_>) {
    let ReactToCompletionsParams {
        conn: _conn,
        run_id: _run_id,
    } = params;
}
