//! Domain models for task-mgr.
//!
//! This module contains the core domain models:
//! - [`Task`] - A task/user story from the PRD
//! - [`TaskStatus`] - The status of a task
//! - [`Run`] - An execution session (run)
//! - [`RunStatus`] - The status of a run
//! - [`RunTask`] - A task execution within a run
//! - [`RunTaskStatus`] - The status of a task within a run
//! - [`Learning`] - A learning from the institutional memory system
//! - [`LearningOutcome`] - The outcome type of a learning
//! - [`Confidence`] - The confidence level of a learning
//! - [`TaskRelationship`] - A relationship between tasks
//! - [`RelationshipType`] - The type of relationship (dependsOn, synergyWith, etc.)
//!
//! Export format structs for progress.json:
//! - [`ProgressExport`] - Root structure for progress export
//! - [`RunExport`] - Export format for a run with nested tasks
//! - [`RunTaskExport`] - Export format for a task execution
//! - [`LearningExport`] - Export format for a learning
//! - [`LearningSummary`] - Summary view of a learning
//! - [`ProgressStatistics`] - Aggregate statistics
//! - [`LearningsByOutcome`] - Counts by outcome type
//! - [`TaskErrorSummary`] - Summary of tasks with errors

mod datetime;
pub mod learning;
pub mod progress;
pub mod relationships;
pub mod run;
pub mod task;

// Re-export datetime utilities for internal use by other model modules
pub(crate) use datetime::{parse_datetime, parse_optional_datetime};

pub use learning::{Confidence, Learning, LearningOutcome};
pub use progress::{
    LearningExport, LearningSummary, LearningsByOutcome, ProgressExport, ProgressStatistics,
    RunExport, RunTaskExport, TaskErrorSummary,
};
pub use relationships::{RelationshipType, TaskRelationship};
pub use run::{Run, RunStatus, RunTask, RunTaskStatus};
pub use task::{Task, TaskStatus};
