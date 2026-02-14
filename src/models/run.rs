//! Run and RunTask models for tracking execution sessions.
//!
//! This module defines the Run struct for tracking agent execution sessions
//! and the RunTask struct for tracking individual task executions within a run.

use chrono::{DateTime, Utc};
use rusqlite::Row;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use super::{parse_datetime, parse_optional_datetime};
use crate::TaskMgrError;

/// Represents the status of a run (execution session).
///
/// Maps to the `status` column CHECK constraint in the runs table:
/// `CHECK(status IN ('active', 'completed', 'aborted'))`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Run is currently in progress
    Active,
    /// Run completed successfully
    Completed,
    /// Run was aborted before completion
    Aborted,
}

impl RunStatus {
    /// Returns the database string representation of this status.
    #[must_use]
    pub fn as_db_str(&self) -> &'static str {
        match self {
            RunStatus::Active => "active",
            RunStatus::Completed => "completed",
            RunStatus::Aborted => "aborted",
        }
    }

    /// Returns true if this run is still active (not finished).
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, RunStatus::Active)
    }

    /// Returns true if this run has finished (completed or aborted).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(self, RunStatus::Completed | RunStatus::Aborted)
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_db_str())
    }
}

impl FromStr for RunStatus {
    type Err = TaskMgrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(RunStatus::Active),
            "completed" => Ok(RunStatus::Completed),
            "aborted" => Ok(RunStatus::Aborted),
            _ => Err(TaskMgrError::invalid_state(
                "RunStatus",
                s,
                "active, completed, or aborted",
                s,
            )),
        }
    }
}

/// Represents an execution run (session) in the task management system.
///
/// Runs track execution sessions for auditing, recovery, and metrics.
/// A run can span multiple tasks and iterations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
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

    /// List of recently modified files (stored as JSON array in DB)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_files: Vec<String>,

    /// Number of iterations completed in this run
    pub iteration_count: i32,

    /// Additional notes about the run
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Run {
    /// Creates a new run with the given ID.
    #[must_use]
    pub fn new(run_id: impl Into<String>) -> Self {
        Run {
            run_id: run_id.into(),
            started_at: Utc::now(),
            ended_at: None,
            status: RunStatus::Active,
            last_commit: None,
            last_files: Vec::new(),
            iteration_count: 0,
            notes: None,
        }
    }

    /// Returns the duration of the run in seconds, or None if still active.
    #[must_use]
    pub fn duration_seconds(&self) -> Option<i64> {
        self.ended_at
            .map(|end| (end - self.started_at).num_seconds())
    }
}

impl TryFrom<&Row<'_>> for Run {
    type Error = TaskMgrError;

    fn try_from(row: &Row<'_>) -> Result<Self, Self::Error> {
        // Parse status from string
        let status_str: String = row.get("status")?;
        let status = RunStatus::from_str(&status_str)?;

        // Parse last_files from JSON array
        let last_files: Vec<String> = {
            let json_str: Option<String> = row.get("last_files")?;
            match json_str {
                Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
                _ => Vec::new(),
            }
        };

        // Parse timestamps
        let started_at_str: String = row.get("started_at")?;
        let ended_at_str: Option<String> = row.get("ended_at")?;

        Ok(Run {
            run_id: row.get("run_id")?,
            started_at: parse_datetime(&started_at_str)?,
            ended_at: parse_optional_datetime(ended_at_str)?,
            status,
            last_commit: row.get("last_commit")?,
            last_files,
            iteration_count: row.get("iteration_count")?,
            notes: row.get("notes")?,
        })
    }
}

/// Represents the status of a task within a run.
///
/// Maps to the `status` column CHECK constraint in the run_tasks table:
/// `CHECK(status IN ('started', 'completed', 'failed', 'skipped'))`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTaskStatus {
    /// Task has been started but not finished
    Started,
    /// Task was completed successfully
    Completed,
    /// Task failed with an error
    Failed,
    /// Task was skipped
    Skipped,
}

impl RunTaskStatus {
    /// Returns the database string representation of this status.
    #[must_use]
    pub fn as_db_str(&self) -> &'static str {
        match self {
            RunTaskStatus::Started => "started",
            RunTaskStatus::Completed => "completed",
            RunTaskStatus::Failed => "failed",
            RunTaskStatus::Skipped => "skipped",
        }
    }

    /// Returns true if this status represents a terminal state.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            RunTaskStatus::Completed | RunTaskStatus::Failed | RunTaskStatus::Skipped
        )
    }

    /// Returns true if this status represents successful completion.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, RunTaskStatus::Completed)
    }
}

impl fmt::Display for RunTaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_db_str())
    }
}

impl FromStr for RunTaskStatus {
    type Err = TaskMgrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "started" => Ok(RunTaskStatus::Started),
            "completed" => Ok(RunTaskStatus::Completed),
            "failed" => Ok(RunTaskStatus::Failed),
            "skipped" => Ok(RunTaskStatus::Skipped),
            _ => Err(TaskMgrError::invalid_state(
                "RunTaskStatus",
                s,
                "started, completed, failed, or skipped",
                s,
            )),
        }
    }
}

/// Represents a task execution within a run.
///
/// Tracks the execution of a specific task during a run,
/// including timing and status information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunTask {
    /// Database ID (auto-increment)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,

    /// ID of the parent run
    pub run_id: String,

    /// ID of the task being executed
    pub task_id: String,

    /// Current status of this task execution
    pub status: RunTaskStatus,

    /// Which iteration within the run (allows same task multiple times)
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

impl RunTask {
    /// Creates a new RunTask for a task execution.
    #[must_use]
    pub fn new(run_id: impl Into<String>, task_id: impl Into<String>, iteration: i32) -> Self {
        RunTask {
            id: None,
            run_id: run_id.into(),
            task_id: task_id.into(),
            status: RunTaskStatus::Started,
            iteration,
            started_at: Utc::now(),
            ended_at: None,
            duration_seconds: None,
            notes: None,
        }
    }

    /// Marks this task execution as completed and calculates duration.
    pub fn complete(&mut self) {
        self.status = RunTaskStatus::Completed;
        let now = Utc::now();
        self.duration_seconds = Some((now - self.started_at).num_seconds());
        self.ended_at = Some(now);
    }

    /// Marks this task execution as failed and calculates duration.
    pub fn fail(&mut self, error: Option<String>) {
        self.status = RunTaskStatus::Failed;
        let now = Utc::now();
        self.duration_seconds = Some((now - self.started_at).num_seconds());
        self.ended_at = Some(now);
        self.notes = error;
    }
}

impl TryFrom<&Row<'_>> for RunTask {
    type Error = TaskMgrError;

    fn try_from(row: &Row<'_>) -> Result<Self, Self::Error> {
        // Parse status from string
        let status_str: String = row.get("status")?;
        let status = RunTaskStatus::from_str(&status_str)?;

        // Parse timestamps
        let started_at_str: String = row.get("started_at")?;
        let ended_at_str: Option<String> = row.get("ended_at")?;

        Ok(RunTask {
            id: row.get("id")?,
            run_id: row.get("run_id")?,
            task_id: row.get("task_id")?,
            status,
            iteration: row.get("iteration")?,
            started_at: parse_datetime(&started_at_str)?,
            ended_at: parse_optional_datetime(ended_at_str)?,
            duration_seconds: row.get("duration_seconds")?,
            notes: row.get("notes")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ RunStatus tests ============

    #[test]
    fn test_run_status_display() {
        assert_eq!(RunStatus::Active.to_string(), "active");
        assert_eq!(RunStatus::Completed.to_string(), "completed");
        assert_eq!(RunStatus::Aborted.to_string(), "aborted");
    }

    #[test]
    fn test_run_status_from_str() {
        assert_eq!(RunStatus::from_str("active").unwrap(), RunStatus::Active);
        assert_eq!(
            RunStatus::from_str("completed").unwrap(),
            RunStatus::Completed
        );
        assert_eq!(RunStatus::from_str("aborted").unwrap(), RunStatus::Aborted);
    }

    #[test]
    fn test_run_status_from_str_invalid() {
        let result = RunStatus::from_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_run_status_roundtrip() {
        let statuses = [RunStatus::Active, RunStatus::Completed, RunStatus::Aborted];

        for status in statuses {
            let s = status.to_string();
            let parsed = RunStatus::from_str(&s).unwrap();
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn test_run_status_is_active() {
        assert!(RunStatus::Active.is_active());
        assert!(!RunStatus::Completed.is_active());
        assert!(!RunStatus::Aborted.is_active());
    }

    #[test]
    fn test_run_status_is_finished() {
        assert!(!RunStatus::Active.is_finished());
        assert!(RunStatus::Completed.is_finished());
        assert!(RunStatus::Aborted.is_finished());
    }

    #[test]
    fn test_run_status_serialization() {
        let status = RunStatus::Completed;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""completed""#);

        let deserialized: RunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, status);
    }

    // ============ Run tests ============

    #[test]
    fn test_run_new() {
        let run = Run::new("run-001");
        assert_eq!(run.run_id, "run-001");
        assert_eq!(run.status, RunStatus::Active);
        assert!(run.ended_at.is_none());
        assert!(run.last_commit.is_none());
        assert!(run.last_files.is_empty());
        assert_eq!(run.iteration_count, 0);
    }

    #[test]
    fn test_run_duration_seconds_active() {
        let run = Run::new("run-001");
        assert!(run.duration_seconds().is_none());
    }

    #[test]
    fn test_run_duration_seconds_completed() {
        let mut run = Run::new("run-001");
        // Set ended_at to 60 seconds after started_at
        run.ended_at = Some(run.started_at + chrono::Duration::seconds(60));
        assert_eq!(run.duration_seconds(), Some(60));
    }

    #[test]
    fn test_run_serialization() {
        let run = Run::new("run-001");
        let json = serde_json::to_string(&run).unwrap();
        assert!(json.contains("\"run_id\":\"run-001\""));
        assert!(json.contains("\"status\":\"active\""));
        // ended_at should be omitted when None
        assert!(!json.contains("\"ended_at\""));
    }

    #[test]
    fn test_run_deserialization() {
        let json = r#"{
            "run_id": "run-001",
            "started_at": "2026-01-18T12:00:00Z",
            "status": "completed",
            "last_commit": "abc123",
            "last_files": ["src/main.rs", "src/lib.rs"],
            "iteration_count": 5
        }"#;

        let run: Run = serde_json::from_str(json).unwrap();
        assert_eq!(run.run_id, "run-001");
        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(run.last_commit, Some("abc123".to_string()));
        assert_eq!(run.last_files, vec!["src/main.rs", "src/lib.rs"]);
        assert_eq!(run.iteration_count, 5);
    }

    // ============ RunTaskStatus tests ============

    #[test]
    fn test_run_task_status_display() {
        assert_eq!(RunTaskStatus::Started.to_string(), "started");
        assert_eq!(RunTaskStatus::Completed.to_string(), "completed");
        assert_eq!(RunTaskStatus::Failed.to_string(), "failed");
        assert_eq!(RunTaskStatus::Skipped.to_string(), "skipped");
    }

    #[test]
    fn test_run_task_status_from_str() {
        assert_eq!(
            RunTaskStatus::from_str("started").unwrap(),
            RunTaskStatus::Started
        );
        assert_eq!(
            RunTaskStatus::from_str("completed").unwrap(),
            RunTaskStatus::Completed
        );
        assert_eq!(
            RunTaskStatus::from_str("failed").unwrap(),
            RunTaskStatus::Failed
        );
        assert_eq!(
            RunTaskStatus::from_str("skipped").unwrap(),
            RunTaskStatus::Skipped
        );
    }

    #[test]
    fn test_run_task_status_from_str_invalid() {
        let result = RunTaskStatus::from_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_run_task_status_roundtrip() {
        let statuses = [
            RunTaskStatus::Started,
            RunTaskStatus::Completed,
            RunTaskStatus::Failed,
            RunTaskStatus::Skipped,
        ];

        for status in statuses {
            let s = status.to_string();
            let parsed = RunTaskStatus::from_str(&s).unwrap();
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn test_run_task_status_is_finished() {
        assert!(!RunTaskStatus::Started.is_finished());
        assert!(RunTaskStatus::Completed.is_finished());
        assert!(RunTaskStatus::Failed.is_finished());
        assert!(RunTaskStatus::Skipped.is_finished());
    }

    #[test]
    fn test_run_task_status_is_success() {
        assert!(!RunTaskStatus::Started.is_success());
        assert!(RunTaskStatus::Completed.is_success());
        assert!(!RunTaskStatus::Failed.is_success());
        assert!(!RunTaskStatus::Skipped.is_success());
    }

    #[test]
    fn test_run_task_status_serialization() {
        let status = RunTaskStatus::Failed;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""failed""#);

        let deserialized: RunTaskStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, status);
    }

    // ============ RunTask tests ============

    #[test]
    fn test_run_task_new() {
        let run_task = RunTask::new("run-001", "US-001", 1);
        assert_eq!(run_task.run_id, "run-001");
        assert_eq!(run_task.task_id, "US-001");
        assert_eq!(run_task.iteration, 1);
        assert_eq!(run_task.status, RunTaskStatus::Started);
        assert!(run_task.id.is_none());
        assert!(run_task.ended_at.is_none());
        assert!(run_task.duration_seconds.is_none());
    }

    #[test]
    fn test_run_task_complete() {
        let mut run_task = RunTask::new("run-001", "US-001", 1);
        // Sleep a tiny bit to ensure duration > 0 (not reliable but illustrative)
        std::thread::sleep(std::time::Duration::from_millis(10));
        run_task.complete();

        assert_eq!(run_task.status, RunTaskStatus::Completed);
        assert!(run_task.ended_at.is_some());
        assert!(run_task.duration_seconds.is_some());
    }

    #[test]
    fn test_run_task_fail() {
        let mut run_task = RunTask::new("run-001", "US-001", 1);
        run_task.fail(Some("Test error".to_string()));

        assert_eq!(run_task.status, RunTaskStatus::Failed);
        assert!(run_task.ended_at.is_some());
        assert_eq!(run_task.notes, Some("Test error".to_string()));
    }

    #[test]
    fn test_run_task_serialization() {
        let run_task = RunTask::new("run-001", "US-001", 1);
        let json = serde_json::to_string(&run_task).unwrap();
        assert!(json.contains("\"run_id\":\"run-001\""));
        assert!(json.contains("\"task_id\":\"US-001\""));
        assert!(json.contains("\"status\":\"started\""));
        // id should be omitted when None
        assert!(!json.contains("\"id\""));
    }

    #[test]
    fn test_run_task_deserialization() {
        let json = r#"{
            "id": 42,
            "run_id": "run-001",
            "task_id": "US-001",
            "status": "completed",
            "iteration": 3,
            "started_at": "2026-01-18T12:00:00Z",
            "ended_at": "2026-01-18T12:05:00Z",
            "duration_seconds": 300
        }"#;

        let run_task: RunTask = serde_json::from_str(json).unwrap();
        assert_eq!(run_task.id, Some(42));
        assert_eq!(run_task.run_id, "run-001");
        assert_eq!(run_task.task_id, "US-001");
        assert_eq!(run_task.status, RunTaskStatus::Completed);
        assert_eq!(run_task.iteration, 3);
        assert_eq!(run_task.duration_seconds, Some(300));
    }

    // Datetime parsing tests are in models/datetime.rs
}
