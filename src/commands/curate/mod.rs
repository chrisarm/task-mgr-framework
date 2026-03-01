//! Curate subcommand implementations.
//!
//! Provides `curate retire` and `curate unretire` commands for managing
//! the institutional memory quality via soft-archiving stale learnings.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::TaskMgrResult;

/// A learning identified as a retirement candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetirementCandidate {
    /// Learning ID
    pub id: i64,
    /// Learning title
    pub title: String,
    /// Human-readable reason why this learning is a candidate
    pub reason: String,
}

/// Result of the `curate retire` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetireResult {
    /// Whether this was a dry run (no DB changes made)
    pub dry_run: bool,
    /// Number of candidates identified
    pub candidates_found: usize,
    /// Number of learnings actually retired (0 when dry_run=true)
    pub learnings_retired: usize,
    /// The candidate learnings
    pub candidates: Vec<RetirementCandidate>,
}

/// Result of the `curate unretire` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnretireResult {
    /// IDs successfully restored to active status
    pub restored: Vec<i64>,
    /// Per-ID error messages for IDs that could not be unretired
    pub errors: Vec<String>,
}

/// Parameters for the `curate retire` command.
#[derive(Debug, Clone)]
pub struct RetireParams {
    /// If true, identify candidates but do not set retired_at
    pub dry_run: bool,
    /// Minimum age in days for criterion 1 (default: 90)
    pub min_age_days: u32,
    /// Minimum times_shown for criteria 2 and 3 (default: 10)
    pub min_shows: u32,
    /// Maximum application rate for criterion 3 (default: 0.05)
    pub max_rate: f64,
}

impl Default for RetireParams {
    fn default() -> Self {
        Self {
            dry_run: false,
            min_age_days: 90,
            min_shows: 10,
            max_rate: 0.05,
        }
    }
}

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
