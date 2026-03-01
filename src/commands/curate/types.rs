//! Types for the `curate` subcommands.

use serde::{Deserialize, Serialize};

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
