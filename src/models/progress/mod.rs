//! Progress export models for task-mgr.
//!
//! This module defines the structs used for exporting progress data
//! to JSON format, including runs, learnings, and aggregate statistics.

mod learnings;
mod runs;
mod statistics;

#[cfg(test)]
mod tests;

pub use learnings::{LearningExport, LearningSummary};
pub use runs::{RunExport, RunTaskExport};
pub use statistics::{LearningsByOutcome, ProgressStatistics, TaskErrorSummary};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Root structure for progress.json export.
///
/// Contains all the information needed to capture and restore
/// the state of a task-mgr session, including runs, learnings,
/// and aggregate statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressExport {
    /// When this export was created
    pub exported_at: DateTime<Utc>,

    /// Version of the export format (for forward compatibility)
    pub export_version: String,

    /// Path to the source database
    pub source_db: String,

    /// Current global iteration counter at time of export
    pub global_iteration: i32,

    /// All runs included in this export
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunExport>,

    /// All learnings included in this export
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub learnings: Vec<LearningExport>,

    /// Aggregate statistics about progress
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistics: Option<ProgressStatistics>,
}

impl ProgressExport {
    /// Creates a new ProgressExport with the given metadata.
    #[must_use]
    pub fn new(source_db: impl Into<String>, global_iteration: i32) -> Self {
        ProgressExport {
            exported_at: Utc::now(),
            export_version: "1.0".to_string(),
            source_db: source_db.into(),
            global_iteration,
            runs: Vec::new(),
            learnings: Vec::new(),
            statistics: None,
        }
    }
}
