//! Statistics models for progress export.
//!
//! Contains aggregate statistics about tasks, runs, and learnings.

use serde::{Deserialize, Serialize};

use crate::models::learning::LearningOutcome;
use crate::models::run::RunStatus;
use crate::models::task::TaskStatus;

use super::learnings::LearningSummary;

/// Aggregate statistics about task progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressStatistics {
    /// Total number of tasks
    pub total_tasks: i32,

    /// Number of completed tasks (status = done)
    pub completed_tasks: i32,

    /// Number of pending tasks (status = todo)
    pub pending_tasks: i32,

    /// Number of blocked tasks
    pub blocked_tasks: i32,

    /// Number of in-progress tasks
    pub in_progress_tasks: i32,

    /// Number of skipped tasks
    pub skipped_tasks: i32,

    /// Number of irrelevant tasks
    pub irrelevant_tasks: i32,

    /// Completion percentage (completed / total * 100)
    pub completion_percentage: f64,

    /// Total number of runs
    pub total_runs: i32,

    /// Number of completed runs
    pub completed_runs: i32,

    /// Number of aborted runs
    pub aborted_runs: i32,

    /// Total number of learnings
    pub total_learnings: i32,

    /// Learnings by outcome type
    pub learnings_by_outcome: LearningsByOutcome,

    /// Most frequently applied learnings
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_applied_learnings: Vec<LearningSummary>,

    /// Tasks with most errors (error_count > 0)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks_with_errors: Vec<TaskErrorSummary>,
}

impl ProgressStatistics {
    /// Creates a new ProgressStatistics with all counts at zero.
    #[must_use]
    pub fn new() -> Self {
        ProgressStatistics {
            total_tasks: 0,
            completed_tasks: 0,
            pending_tasks: 0,
            blocked_tasks: 0,
            in_progress_tasks: 0,
            skipped_tasks: 0,
            irrelevant_tasks: 0,
            completion_percentage: 0.0,
            total_runs: 0,
            completed_runs: 0,
            aborted_runs: 0,
            total_learnings: 0,
            learnings_by_outcome: LearningsByOutcome::new(),
            top_applied_learnings: Vec::new(),
            tasks_with_errors: Vec::new(),
        }
    }

    /// Calculates completion percentage from current counts.
    pub fn calculate_completion_percentage(&mut self) {
        if self.total_tasks > 0 {
            self.completion_percentage =
                (f64::from(self.completed_tasks) / f64::from(self.total_tasks)) * 100.0;
        } else {
            self.completion_percentage = 0.0;
        }
    }

    /// Increments the count for a task status.
    pub fn increment_task_status(&mut self, status: TaskStatus) {
        self.total_tasks += 1;
        match status {
            TaskStatus::Todo => self.pending_tasks += 1,
            TaskStatus::InProgress => self.in_progress_tasks += 1,
            TaskStatus::Done => self.completed_tasks += 1,
            TaskStatus::Blocked => self.blocked_tasks += 1,
            TaskStatus::Skipped => self.skipped_tasks += 1,
            TaskStatus::Irrelevant => self.irrelevant_tasks += 1,
        }
    }

    /// Increments the count for a run status.
    pub fn increment_run_status(&mut self, status: RunStatus) {
        self.total_runs += 1;
        match status {
            RunStatus::Active => {}
            RunStatus::Completed => self.completed_runs += 1,
            RunStatus::Aborted => self.aborted_runs += 1,
        }
    }

    /// Increments the count for a learning outcome.
    pub fn increment_learning_outcome(&mut self, outcome: LearningOutcome) {
        self.total_learnings += 1;
        self.learnings_by_outcome.increment(outcome);
    }
}

impl Default for ProgressStatistics {
    fn default() -> Self {
        Self::new()
    }
}

/// Counts of learnings by outcome type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningsByOutcome {
    /// Number of failure learnings
    pub failures: i32,

    /// Number of success learnings
    pub successes: i32,

    /// Number of workaround learnings
    pub workarounds: i32,

    /// Number of pattern learnings
    pub patterns: i32,
}

impl LearningsByOutcome {
    /// Creates a new LearningsByOutcome with all counts at zero.
    #[must_use]
    pub fn new() -> Self {
        LearningsByOutcome {
            failures: 0,
            successes: 0,
            workarounds: 0,
            patterns: 0,
        }
    }

    /// Increments the count for a learning outcome.
    pub fn increment(&mut self, outcome: LearningOutcome) {
        match outcome {
            LearningOutcome::Failure => self.failures += 1,
            LearningOutcome::Success => self.successes += 1,
            LearningOutcome::Workaround => self.workarounds += 1,
            LearningOutcome::Pattern => self.patterns += 1,
        }
    }
}

impl Default for LearningsByOutcome {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of a task with errors for statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskErrorSummary {
    /// Task ID
    pub task_id: String,

    /// Task title
    pub title: String,

    /// Number of errors encountered
    pub error_count: i32,

    /// Most recent error message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,

    /// Current task status
    pub status: TaskStatus,
}

impl TaskErrorSummary {
    /// Creates a new TaskErrorSummary.
    #[must_use]
    pub fn new(
        task_id: impl Into<String>,
        title: impl Into<String>,
        error_count: i32,
        status: TaskStatus,
    ) -> Self {
        TaskErrorSummary {
            task_id: task_id.into(),
            title: title.into(),
            error_count,
            last_error: None,
            status,
        }
    }
}
