//! Curate subcommand implementations.
//!
//! Provides `curate retire` and `curate unretire` commands for managing
//! the institutional memory quality via soft-archiving stale learnings.

pub mod output;
pub mod types;

pub use output::{format_retire_text, format_unretire_text};
pub use types::{RetireParams, RetireResult, RetirementCandidate, UnretireResult};

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Identifies retirement candidates and optionally soft-archives them.
///
/// Three criteria (OR'd together):
/// 1. age >= min_age_days AND confidence = 'low' AND times_applied = 0
/// 2. times_shown >= min_shows AND times_applied = 0
/// 3. times_shown >= min_shows*2 AND (times_applied/times_shown) < max_rate
///
/// Already-retired learnings (retired_at IS NOT NULL) are excluded.
pub fn curate_retire(conn: &Connection, params: RetireParams) -> TaskMgrResult<RetireResult> {
    let _ = conn;
    let _ = params;
    todo!("FEAT-004: implement retire candidate identification and soft-archive")
}

/// Restores soft-archived learnings by setting retired_at = NULL.
///
/// Returns an error entry for each ID that does not exist or is not retired.
pub fn curate_unretire(conn: &Connection, learning_ids: Vec<i64>) -> TaskMgrResult<UnretireResult> {
    let _ = conn;
    let _ = learning_ids;
    todo!("FEAT-005: implement curate unretire")
}

#[cfg(test)]
mod tests;
