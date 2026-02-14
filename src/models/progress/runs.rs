//! Run export models for progress export.
//!
//! Contains export formats for runs and run tasks.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::run::{RunStatus, RunTaskStatus};

/// Export format for a run (execution session).
///
/// Contains all run information plus nested task executions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunExport {
    /// Unique identifier for the run (UUID format)
    pub run_id: String,

    /// When the run started
    pub started_at: DateTime<Utc>,

    /// When the run ended (None if still active)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,

    /// Current status of the run
    pub status: RunStatus,

    /// Most recent git commit hash
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<String>,

    /// List of recently modified files
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_files: Vec<String>,

    /// Number of iterations completed in this run
    pub iteration_count: i32,

    /// Additional notes about the run
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    /// Task executions within this run
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<RunTaskExport>,

    /// Duration of the run in seconds (computed if ended)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
}

impl RunExport {
    /// Creates a new RunExport with the given run_id.
    #[must_use]
    pub fn new(run_id: impl Into<String>) -> Self {
        RunExport {
            run_id: run_id.into(),
            started_at: Utc::now(),
            ended_at: None,
            status: RunStatus::Active,
            last_commit: None,
            last_files: Vec::new(),
            iteration_count: 0,
            notes: None,
            tasks: Vec::new(),
            duration_seconds: None,
        }
    }
}

/// Export format for a task execution within a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTaskExport {
    /// ID of the task being executed
    pub task_id: String,

    /// Current status of this task execution
    pub status: RunTaskStatus,

    /// Which iteration within the run
    pub iteration: i32,

    /// When execution started
    pub started_at: DateTime<Utc>,

    /// When execution ended (None if still running)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,

    /// Duration in seconds (computed on completion)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,

    /// Additional notes about this execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl RunTaskExport {
    /// Creates a new RunTaskExport.
    #[must_use]
    pub fn new(task_id: impl Into<String>, iteration: i32) -> Self {
        RunTaskExport {
            task_id: task_id.into(),
            status: RunTaskStatus::Started,
            iteration,
            started_at: Utc::now(),
            ended_at: None,
            duration_seconds: None,
            notes: None,
        }
    }
}
