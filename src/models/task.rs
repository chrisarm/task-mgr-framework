//! Task model and TaskStatus enum for task-mgr.
//!
//! This module defines the core Task struct that represents a task/user story
//! in the PRD, along with the TaskStatus enum for tracking task state.

use chrono::{DateTime, Utc};
use rusqlite::Row;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use super::{parse_datetime, parse_optional_datetime};
use crate::TaskMgrError;

/// Represents the status of a task in the task management system.
///
/// Maps to the `status` column CHECK constraint in the tasks table:
/// `CHECK(status IN ('todo', 'in_progress', 'done', 'blocked', 'skipped', 'irrelevant'))`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task has not been started
    Todo,
    /// Task is currently being worked on
    InProgress,
    /// Task has been completed successfully
    Done,
    /// Task is blocked by an external dependency or issue
    Blocked,
    /// Task was intentionally skipped for later
    Skipped,
    /// Task is no longer relevant due to changed requirements
    Irrelevant,
}

impl TaskStatus {
    /// Returns the database string representation of this status.
    #[must_use]
    pub fn as_db_str(&self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Done => "done",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Skipped => "skipped",
            TaskStatus::Irrelevant => "irrelevant",
        }
    }

    /// Returns true if this status represents a terminal state (task won't be selected).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Done | TaskStatus::Blocked | TaskStatus::Skipped | TaskStatus::Irrelevant
        )
    }

    /// Returns true if this status means the task passed/completed successfully.
    #[must_use]
    pub fn is_passing(&self) -> bool {
        matches!(self, TaskStatus::Done)
    }

    /// Check if a transition from this status to the target status is valid.
    ///
    /// Valid transitions:
    /// - `todo` -> `in_progress` (claim)
    /// - `in_progress` -> `done`, `blocked`, `skipped`, `irrelevant`
    /// - `blocked` -> `todo` (unblock)
    /// - `skipped` -> `todo` (unskip)
    ///
    /// Invalid transitions:
    /// - `todo` -> `done` (must claim first via in_progress)
    /// - `done` -> anything (terminal state)
    /// - `irrelevant` -> anything (terminal state, permanent exclusion)
    #[must_use]
    pub fn can_transition_to(&self, target: TaskStatus) -> bool {
        // Same status is always valid (no-op)
        if *self == target {
            return true;
        }

        match self {
            // From todo: can only go to in_progress (claim)
            TaskStatus::Todo => matches!(target, TaskStatus::InProgress),

            // From in_progress: can go to any terminal state
            TaskStatus::InProgress => matches!(
                target,
                TaskStatus::Done
                    | TaskStatus::Blocked
                    | TaskStatus::Skipped
                    | TaskStatus::Irrelevant
            ),

            // From blocked: can only return to todo (unblock)
            TaskStatus::Blocked => matches!(target, TaskStatus::Todo),

            // From skipped: can only return to todo (unskip)
            TaskStatus::Skipped => matches!(target, TaskStatus::Todo),

            // From done: terminal state, no transitions allowed
            TaskStatus::Done => false,

            // From irrelevant: terminal state (permanent exclusion), no transitions
            TaskStatus::Irrelevant => false,
        }
    }

    /// Get human-readable valid transitions from this status.
    #[must_use]
    pub fn valid_transitions(&self) -> &'static [&'static str] {
        match self {
            TaskStatus::Todo => &["in_progress"],
            TaskStatus::InProgress => &["done", "blocked", "skipped", "irrelevant"],
            TaskStatus::Blocked => &["todo"],
            TaskStatus::Skipped => &["todo"],
            TaskStatus::Done => &[],
            TaskStatus::Irrelevant => &[],
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_db_str())
    }
}

impl FromStr for TaskStatus {
    type Err = TaskMgrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "todo" => Ok(TaskStatus::Todo),
            "in_progress" => Ok(TaskStatus::InProgress),
            "done" => Ok(TaskStatus::Done),
            "blocked" => Ok(TaskStatus::Blocked),
            "skipped" => Ok(TaskStatus::Skipped),
            "irrelevant" => Ok(TaskStatus::Irrelevant),
            _ => Err(TaskMgrError::invalid_state(
                "TaskStatus",
                s,
                "todo, in_progress, done, blocked, skipped, or irrelevant",
                s,
            )),
        }
    }
}

/// Represents a task (user story) in the task management system.
///
/// This struct maps to the `tasks` table in the database and includes
/// all fields from the PRD JSON format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier for the task (e.g., "US-001", "FIX-002")
    pub id: String,

    /// Short title describing the task
    pub title: String,

    /// Detailed description of what the task involves
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Priority number (lower = higher priority)
    pub priority: i32,

    /// Current status of the task
    pub status: TaskStatus,

    /// Additional notes about the task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    /// List of acceptance criteria that must be met
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,

    /// Review scope configuration (for review tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_scope: Option<serde_json::Value>,

    /// Severity level (for review tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,

    /// Source review that generated this task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_review: Option<String>,

    /// When the task was created
    pub created_at: DateTime<Utc>,

    /// When the task was last updated
    pub updated_at: DateTime<Utc>,

    /// When the task was first claimed/started
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,

    /// When the task was completed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,

    /// Most recent error message encountered
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,

    /// Number of times this task has encountered errors
    pub error_count: i32,

    /// Global iteration when task was blocked (for decay tracking)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_at_iteration: Option<i64>,

    /// Global iteration when task was skipped (for decay tracking)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_at_iteration: Option<i64>,

    /// Preferred model for this task (e.g., "claude-opus-4-6")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Difficulty level for this task (e.g., "low", "medium", "high")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,

    /// Note explaining why this task was escalated to a higher-tier model
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_note: Option<String>,

    /// Cargo test filter strings that must pass before task can be completed
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_tests: Vec<String>,

    /// Maximum consecutive failures before auto-blocking this task (0 = disabled)
    #[serde(default = "default_max_retries")]
    pub max_retries: i32,

    /// Number of consecutive failed iterations for this task (resets on success)
    #[serde(default)]
    pub consecutive_failures: i32,
}

fn default_max_retries() -> i32 {
    3
}

impl Task {
    /// Creates a new task with minimal required fields.
    #[must_use]
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: title.into(),
            description: None,
            priority: 50, // Default priority from schema
            status: TaskStatus::Todo,
            notes: None,
            acceptance_criteria: Vec::new(),
            review_scope: None,
            severity: None,
            source_review: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            last_error: None,
            error_count: 0,
            blocked_at_iteration: None,
            skipped_at_iteration: None,
            model: None,
            difficulty: None,
            escalation_note: None,
            required_tests: Vec::new(),
            max_retries: 3,
            consecutive_failures: 0,
        }
    }

    /// Returns true if all dependencies for this task are satisfied.
    ///
    /// Note: This method requires the list of completed task IDs to be passed in,
    /// as the Task struct doesn't store relationship information directly.
    #[must_use]
    pub fn dependencies_satisfied(
        &self,
        dependencies: &[String],
        completed_ids: &[String],
    ) -> bool {
        dependencies.iter().all(|dep| completed_ids.contains(dep))
    }
}

impl TryFrom<&Row<'_>> for Task {
    type Error = TaskMgrError;

    fn try_from(row: &Row<'_>) -> Result<Self, Self::Error> {
        // Parse status from string
        let status_str: String = row.get("status")?;
        let status = TaskStatus::from_str(&status_str)?;

        // Parse acceptance_criteria from JSON array
        let acceptance_criteria: Vec<String> = {
            let json_str: Option<String> = row.get("acceptance_criteria")?;
            match json_str {
                Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
                _ => Vec::new(),
            }
        };

        // Parse review_scope from JSON
        let review_scope: Option<serde_json::Value> = {
            let json_str: Option<String> = row.get("review_scope")?;
            match json_str {
                Some(s) if !s.is_empty() => serde_json::from_str(&s).ok(),
                _ => None,
            }
        };

        // Parse timestamps
        let created_at_str: String = row.get("created_at")?;
        let updated_at_str: String = row.get("updated_at")?;
        let started_at_str: Option<String> = row.get("started_at")?;
        let completed_at_str: Option<String> = row.get("completed_at")?;

        Ok(Task {
            id: row.get("id")?,
            title: row.get("title")?,
            description: row.get("description")?,
            priority: row.get("priority")?,
            status,
            notes: row.get("notes")?,
            acceptance_criteria,
            review_scope,
            severity: row.get("severity")?,
            source_review: row.get("source_review")?,
            created_at: parse_datetime(&created_at_str)?,
            updated_at: parse_datetime(&updated_at_str)?,
            started_at: parse_optional_datetime(started_at_str)?,
            completed_at: parse_optional_datetime(completed_at_str)?,
            last_error: row.get("last_error")?,
            error_count: row.get("error_count")?,
            blocked_at_iteration: row.get("blocked_at_iteration").ok().flatten(),
            skipped_at_iteration: row.get("skipped_at_iteration").ok().flatten(),
            model: row.get("model").ok().flatten(),
            difficulty: row.get("difficulty").ok().flatten(),
            escalation_note: row.get("escalation_note").ok().flatten(),
            required_tests: {
                let json_str: Option<String> = row.get("required_tests").ok().flatten();
                match json_str {
                    Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
                    _ => Vec::new(),
                }
            },
            // Graceful fallback: column doesn't exist until v13 migration runs (FEAT-001)
            max_retries: row.get::<_, i32>("max_retries").unwrap_or(3),
            consecutive_failures: row.get::<_, i32>("consecutive_failures").unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ TaskStatus tests ============

    #[test]
    fn test_task_status_display() {
        assert_eq!(TaskStatus::Todo.to_string(), "todo");
        assert_eq!(TaskStatus::InProgress.to_string(), "in_progress");
        assert_eq!(TaskStatus::Done.to_string(), "done");
        assert_eq!(TaskStatus::Blocked.to_string(), "blocked");
        assert_eq!(TaskStatus::Skipped.to_string(), "skipped");
        assert_eq!(TaskStatus::Irrelevant.to_string(), "irrelevant");
    }

    #[test]
    fn test_task_status_from_str() {
        assert_eq!(TaskStatus::from_str("todo").unwrap(), TaskStatus::Todo);
        assert_eq!(
            TaskStatus::from_str("in_progress").unwrap(),
            TaskStatus::InProgress
        );
        assert_eq!(TaskStatus::from_str("done").unwrap(), TaskStatus::Done);
        assert_eq!(
            TaskStatus::from_str("blocked").unwrap(),
            TaskStatus::Blocked
        );
        assert_eq!(
            TaskStatus::from_str("skipped").unwrap(),
            TaskStatus::Skipped
        );
        assert_eq!(
            TaskStatus::from_str("irrelevant").unwrap(),
            TaskStatus::Irrelevant
        );
    }

    #[test]
    fn test_task_status_from_str_invalid() {
        let result = TaskStatus::from_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_task_status_roundtrip() {
        let statuses = [
            TaskStatus::Todo,
            TaskStatus::InProgress,
            TaskStatus::Done,
            TaskStatus::Blocked,
            TaskStatus::Skipped,
            TaskStatus::Irrelevant,
        ];

        for status in statuses {
            let s = status.to_string();
            let parsed = TaskStatus::from_str(&s).unwrap();
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn test_task_status_is_terminal() {
        assert!(!TaskStatus::Todo.is_terminal());
        assert!(!TaskStatus::InProgress.is_terminal());
        assert!(TaskStatus::Done.is_terminal());
        assert!(TaskStatus::Blocked.is_terminal());
        assert!(TaskStatus::Skipped.is_terminal());
        assert!(TaskStatus::Irrelevant.is_terminal());
    }

    #[test]
    fn test_task_status_is_passing() {
        assert!(!TaskStatus::Todo.is_passing());
        assert!(!TaskStatus::InProgress.is_passing());
        assert!(TaskStatus::Done.is_passing());
        assert!(!TaskStatus::Blocked.is_passing());
        assert!(!TaskStatus::Skipped.is_passing());
        assert!(!TaskStatus::Irrelevant.is_passing());
    }

    // ============ TaskStatus transition tests ============

    #[test]
    fn test_can_transition_same_status() {
        // Same status is always valid (no-op)
        for status in [
            TaskStatus::Todo,
            TaskStatus::InProgress,
            TaskStatus::Done,
            TaskStatus::Blocked,
            TaskStatus::Skipped,
            TaskStatus::Irrelevant,
        ] {
            assert!(
                status.can_transition_to(status),
                "{} should be able to transition to itself",
                status
            );
        }
    }

    #[test]
    fn test_can_transition_from_todo() {
        // Todo can only go to in_progress (claim)
        assert!(TaskStatus::Todo.can_transition_to(TaskStatus::InProgress));

        // Todo cannot go directly to terminal states
        assert!(!TaskStatus::Todo.can_transition_to(TaskStatus::Done));
        assert!(!TaskStatus::Todo.can_transition_to(TaskStatus::Blocked));
        assert!(!TaskStatus::Todo.can_transition_to(TaskStatus::Skipped));
        assert!(!TaskStatus::Todo.can_transition_to(TaskStatus::Irrelevant));
    }

    #[test]
    fn test_can_transition_from_in_progress() {
        // InProgress can go to any terminal state
        assert!(TaskStatus::InProgress.can_transition_to(TaskStatus::Done));
        assert!(TaskStatus::InProgress.can_transition_to(TaskStatus::Blocked));
        assert!(TaskStatus::InProgress.can_transition_to(TaskStatus::Skipped));
        assert!(TaskStatus::InProgress.can_transition_to(TaskStatus::Irrelevant));

        // InProgress cannot go back to todo
        assert!(!TaskStatus::InProgress.can_transition_to(TaskStatus::Todo));
    }

    #[test]
    fn test_can_transition_from_blocked() {
        // Blocked can only return to todo (unblock)
        assert!(TaskStatus::Blocked.can_transition_to(TaskStatus::Todo));

        // Blocked cannot go to other states
        assert!(!TaskStatus::Blocked.can_transition_to(TaskStatus::InProgress));
        assert!(!TaskStatus::Blocked.can_transition_to(TaskStatus::Done));
        assert!(!TaskStatus::Blocked.can_transition_to(TaskStatus::Skipped));
        assert!(!TaskStatus::Blocked.can_transition_to(TaskStatus::Irrelevant));
    }

    #[test]
    fn test_can_transition_from_skipped() {
        // Skipped can only return to todo (unskip)
        assert!(TaskStatus::Skipped.can_transition_to(TaskStatus::Todo));

        // Skipped cannot go to other states
        assert!(!TaskStatus::Skipped.can_transition_to(TaskStatus::InProgress));
        assert!(!TaskStatus::Skipped.can_transition_to(TaskStatus::Done));
        assert!(!TaskStatus::Skipped.can_transition_to(TaskStatus::Blocked));
        assert!(!TaskStatus::Skipped.can_transition_to(TaskStatus::Irrelevant));
    }

    #[test]
    fn test_can_transition_from_done() {
        // Done is terminal - no transitions allowed
        assert!(!TaskStatus::Done.can_transition_to(TaskStatus::Todo));
        assert!(!TaskStatus::Done.can_transition_to(TaskStatus::InProgress));
        assert!(!TaskStatus::Done.can_transition_to(TaskStatus::Blocked));
        assert!(!TaskStatus::Done.can_transition_to(TaskStatus::Skipped));
        assert!(!TaskStatus::Done.can_transition_to(TaskStatus::Irrelevant));
    }

    #[test]
    fn test_can_transition_from_irrelevant() {
        // Irrelevant is terminal (permanent exclusion) - no transitions allowed
        assert!(!TaskStatus::Irrelevant.can_transition_to(TaskStatus::Todo));
        assert!(!TaskStatus::Irrelevant.can_transition_to(TaskStatus::InProgress));
        assert!(!TaskStatus::Irrelevant.can_transition_to(TaskStatus::Done));
        assert!(!TaskStatus::Irrelevant.can_transition_to(TaskStatus::Blocked));
        assert!(!TaskStatus::Irrelevant.can_transition_to(TaskStatus::Skipped));
    }

    #[test]
    fn test_valid_transitions() {
        assert_eq!(TaskStatus::Todo.valid_transitions(), &["in_progress"]);
        assert_eq!(
            TaskStatus::InProgress.valid_transitions(),
            &["done", "blocked", "skipped", "irrelevant"]
        );
        assert_eq!(TaskStatus::Blocked.valid_transitions(), &["todo"]);
        assert_eq!(TaskStatus::Skipped.valid_transitions(), &["todo"]);
        assert_eq!(TaskStatus::Done.valid_transitions(), &[] as &[&str]);
        assert_eq!(TaskStatus::Irrelevant.valid_transitions(), &[] as &[&str]);
    }

    #[test]
    fn test_task_status_serialization() {
        let status = TaskStatus::InProgress;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""in_progress""#);

        let deserialized: TaskStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, status);
    }

    // ============ Task tests ============

    #[test]
    fn test_task_new() {
        let task = Task::new("US-001", "Test Task");
        assert_eq!(task.id, "US-001");
        assert_eq!(task.title, "Test Task");
        assert_eq!(task.priority, 50);
        assert_eq!(task.status, TaskStatus::Todo);
        assert!(task.description.is_none());
        assert!(task.acceptance_criteria.is_empty());
        assert_eq!(task.error_count, 0);
    }

    #[test]
    fn test_task_new_max_retries_default() {
        let task = Task::new("US-001", "Test Task");
        assert_eq!(
            task.max_retries, 3,
            "Task::new() must default max_retries to 3"
        );
    }

    #[test]
    fn test_task_new_consecutive_failures_default() {
        let task = Task::new("US-001", "Test Task");
        assert_eq!(
            task.consecutive_failures, 0,
            "Task::new() must default consecutive_failures to 0"
        );
    }

    #[test]
    fn test_task_max_retries_zero_is_valid() {
        let mut task = Task::new("US-001", "Test Task");
        task.max_retries = 0;
        // max_retries=0 means auto-block is disabled — just verify it's storable
        assert_eq!(task.max_retries, 0);
    }

    #[test]
    fn test_task_retry_fields_serialize() {
        let task = Task::new("US-001", "Test Task");
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            json.contains("\"max_retries\":3"),
            "max_retries must serialize to 3"
        );
        assert!(
            json.contains("\"consecutive_failures\":0"),
            "consecutive_failures must serialize to 0"
        );
    }

    #[test]
    fn test_task_retry_fields_deserialize_with_defaults() {
        // JSON without retry fields must deserialize using serde defaults
        let json = r#"{
            "id": "US-001",
            "title": "Test Task",
            "priority": 10,
            "status": "done",
            "created_at": "2026-01-18T12:00:00Z",
            "updated_at": "2026-01-18T12:00:00Z",
            "error_count": 0
        }"#;

        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(
            task.max_retries, 3,
            "missing max_retries must deserialize to 3"
        );
        assert_eq!(
            task.consecutive_failures, 0,
            "missing consecutive_failures must deserialize to 0"
        );
    }

    #[test]
    fn test_task_retry_fields_deserialize_explicit() {
        let json = r#"{
            "id": "US-001",
            "title": "Test Task",
            "priority": 10,
            "status": "todo",
            "created_at": "2026-01-18T12:00:00Z",
            "updated_at": "2026-01-18T12:00:00Z",
            "error_count": 0,
            "max_retries": 5,
            "consecutive_failures": 2
        }"#;

        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.max_retries, 5);
        assert_eq!(task.consecutive_failures, 2);
    }

    #[test]
    fn test_task_serialization() {
        let task = Task::new("US-001", "Test Task");
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"id\":\"US-001\""));
        assert!(json.contains("\"title\":\"Test Task\""));
        assert!(json.contains("\"status\":\"todo\""));
    }

    #[test]
    fn test_task_deserialization() {
        let json = r#"{
            "id": "US-001",
            "title": "Test Task",
            "priority": 10,
            "status": "done",
            "created_at": "2026-01-18T12:00:00Z",
            "updated_at": "2026-01-18T12:00:00Z",
            "error_count": 0
        }"#;

        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.id, "US-001");
        assert_eq!(task.title, "Test Task");
        assert_eq!(task.priority, 10);
        assert_eq!(task.status, TaskStatus::Done);
    }

    #[test]
    fn test_task_dependencies_satisfied() {
        let task = Task::new("US-003", "Task 3");
        let deps = vec!["US-001".to_string(), "US-002".to_string()];

        // All dependencies completed
        let completed = vec![
            "US-001".to_string(),
            "US-002".to_string(),
            "US-004".to_string(),
        ];
        assert!(task.dependencies_satisfied(&deps, &completed));

        // Missing one dependency
        let partial = vec!["US-001".to_string()];
        assert!(!task.dependencies_satisfied(&deps, &partial));

        // No dependencies
        assert!(task.dependencies_satisfied(&[], &completed));
    }

    // Datetime parsing tests are in models/datetime.rs
}
