//! Tests for progress export models.

use super::*;
use crate::models::learning::{Confidence, LearningOutcome};
use crate::models::run::{RunStatus, RunTaskStatus};
use crate::models::task::TaskStatus;

// ============ ProgressExport tests ============

#[test]
fn test_progress_export_new() {
    let export = ProgressExport::new("/path/to/tasks.db", 42);
    assert_eq!(export.source_db, "/path/to/tasks.db");
    assert_eq!(export.global_iteration, 42);
    assert_eq!(export.export_version, "1.0");
    assert!(export.runs.is_empty());
    assert!(export.learnings.is_empty());
    assert!(export.statistics.is_none());
}

#[test]
fn test_progress_export_serialization() {
    let export = ProgressExport::new("/path/to/tasks.db", 10);
    let json = serde_json::to_string(&export).unwrap();
    assert!(json.contains("\"source_db\":\"/path/to/tasks.db\""));
    assert!(json.contains("\"global_iteration\":10"));
    assert!(json.contains("\"export_version\":\"1.0\""));
    // Empty arrays should be omitted
    assert!(!json.contains("\"runs\""));
    assert!(!json.contains("\"learnings\""));
}

#[test]
fn test_progress_export_roundtrip() {
    let mut export = ProgressExport::new("/path/to/tasks.db", 5);
    export.runs.push(RunExport::new("run-001"));
    export.learnings.push(LearningExport::new(
        LearningOutcome::Pattern,
        "Test",
        "Content",
    ));
    export.statistics = Some(ProgressStatistics::new());

    let json = serde_json::to_string(&export).unwrap();
    let parsed: ProgressExport = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.source_db, export.source_db);
    assert_eq!(parsed.global_iteration, export.global_iteration);
    assert_eq!(parsed.runs.len(), 1);
    assert_eq!(parsed.learnings.len(), 1);
    assert!(parsed.statistics.is_some());
}

// ============ RunExport tests ============

#[test]
fn test_run_export_new() {
    let run = RunExport::new("run-001");
    assert_eq!(run.run_id, "run-001");
    assert_eq!(run.status, RunStatus::Active);
    assert!(run.ended_at.is_none());
    assert!(run.tasks.is_empty());
    assert!(run.duration_seconds.is_none());
}

#[test]
fn test_run_export_serialization() {
    let run = RunExport::new("run-001");
    let json = serde_json::to_string(&run).unwrap();
    assert!(json.contains("\"run_id\":\"run-001\""));
    assert!(json.contains("\"status\":\"active\""));
    // Optional None fields should be omitted
    assert!(!json.contains("\"ended_at\""));
    assert!(!json.contains("\"last_commit\""));
}

#[test]
fn test_run_export_with_tasks() {
    let mut run = RunExport::new("run-001");
    run.tasks.push(RunTaskExport::new("US-001", 1));
    run.tasks.push(RunTaskExport::new("US-002", 1));

    let json = serde_json::to_string(&run).unwrap();
    assert!(json.contains("\"tasks\""));
    assert!(json.contains("\"task_id\":\"US-001\""));
    assert!(json.contains("\"task_id\":\"US-002\""));
}

// ============ RunTaskExport tests ============

#[test]
fn test_run_task_export_new() {
    let task = RunTaskExport::new("US-001", 1);
    assert_eq!(task.task_id, "US-001");
    assert_eq!(task.iteration, 1);
    assert_eq!(task.status, RunTaskStatus::Started);
    assert!(task.ended_at.is_none());
}

#[test]
fn test_run_task_export_serialization() {
    let task = RunTaskExport::new("US-001", 2);
    let json = serde_json::to_string(&task).unwrap();
    assert!(json.contains("\"task_id\":\"US-001\""));
    assert!(json.contains("\"iteration\":2"));
    assert!(json.contains("\"status\":\"started\""));
}

// ============ LearningExport tests ============

#[test]
fn test_learning_export_new() {
    let learning = LearningExport::new(LearningOutcome::Failure, "Test failure", "Content");
    assert!(learning.id.is_none());
    assert!(learning.task_id.is_none());
    assert_eq!(learning.outcome, LearningOutcome::Failure);
    assert_eq!(learning.title, "Test failure");
    assert_eq!(learning.confidence, Confidence::Medium);
    assert!(learning.tags.is_empty());
}

#[test]
fn test_learning_export_application_rate() {
    let mut learning = LearningExport::new(LearningOutcome::Pattern, "Test", "Content");

    // No shows yet
    assert!(learning.application_rate().is_none());

    // Show 4 times, apply 2 times
    learning.times_shown = 4;
    learning.times_applied = 2;
    assert_eq!(learning.application_rate(), Some(0.5));
}

#[test]
fn test_learning_export_with_tags() {
    let mut learning = LearningExport::new(LearningOutcome::Success, "Test", "Content");
    learning.tags = vec!["rust".to_string(), "sqlite".to_string()];

    let json = serde_json::to_string(&learning).unwrap();
    assert!(json.contains("\"tags\":[\"rust\",\"sqlite\"]"));
}

// ============ LearningSummary tests ============

#[test]
fn test_learning_summary_from_export() {
    let mut export = LearningExport::new(LearningOutcome::Workaround, "Test", "Content");
    export.id = Some(42);
    export.times_shown = 10;
    export.times_applied = 7;

    let summary = LearningSummary::from_export(&export).unwrap();
    assert_eq!(summary.id, 42);
    assert_eq!(summary.title, "Test");
    assert_eq!(summary.outcome, LearningOutcome::Workaround);
    assert_eq!(summary.application_rate, Some(0.7));
}

#[test]
fn test_learning_summary_from_export_without_id() {
    let export = LearningExport::new(LearningOutcome::Pattern, "Test", "Content");
    let summary = LearningSummary::from_export(&export);
    assert!(summary.is_none());
}

// ============ ProgressStatistics tests ============

#[test]
fn test_progress_statistics_new() {
    let stats = ProgressStatistics::new();
    assert_eq!(stats.total_tasks, 0);
    assert_eq!(stats.completed_tasks, 0);
    assert_eq!(stats.completion_percentage, 0.0);
    assert!(stats.top_applied_learnings.is_empty());
}

#[test]
fn test_progress_statistics_default() {
    let stats = ProgressStatistics::default();
    assert_eq!(stats.total_tasks, 0);
}

#[test]
fn test_progress_statistics_increment_task_status() {
    let mut stats = ProgressStatistics::new();

    stats.increment_task_status(TaskStatus::Done);
    stats.increment_task_status(TaskStatus::Done);
    stats.increment_task_status(TaskStatus::Todo);
    stats.increment_task_status(TaskStatus::Blocked);

    assert_eq!(stats.total_tasks, 4);
    assert_eq!(stats.completed_tasks, 2);
    assert_eq!(stats.pending_tasks, 1);
    assert_eq!(stats.blocked_tasks, 1);
}

#[test]
fn test_progress_statistics_calculate_completion_percentage() {
    let mut stats = ProgressStatistics::new();
    stats.total_tasks = 10;
    stats.completed_tasks = 3;
    stats.calculate_completion_percentage();
    assert!((stats.completion_percentage - 30.0).abs() < 0.001);
}

#[test]
fn test_progress_statistics_calculate_completion_percentage_empty() {
    let mut stats = ProgressStatistics::new();
    stats.calculate_completion_percentage();
    assert_eq!(stats.completion_percentage, 0.0);
}

#[test]
fn test_progress_statistics_increment_run_status() {
    let mut stats = ProgressStatistics::new();

    stats.increment_run_status(RunStatus::Completed);
    stats.increment_run_status(RunStatus::Aborted);
    stats.increment_run_status(RunStatus::Active);

    assert_eq!(stats.total_runs, 3);
    assert_eq!(stats.completed_runs, 1);
    assert_eq!(stats.aborted_runs, 1);
}

#[test]
fn test_progress_statistics_increment_learning_outcome() {
    let mut stats = ProgressStatistics::new();

    stats.increment_learning_outcome(LearningOutcome::Failure);
    stats.increment_learning_outcome(LearningOutcome::Failure);
    stats.increment_learning_outcome(LearningOutcome::Success);
    stats.increment_learning_outcome(LearningOutcome::Pattern);

    assert_eq!(stats.total_learnings, 4);
    assert_eq!(stats.learnings_by_outcome.failures, 2);
    assert_eq!(stats.learnings_by_outcome.successes, 1);
    assert_eq!(stats.learnings_by_outcome.patterns, 1);
}

#[test]
fn test_progress_statistics_serialization() {
    let stats = ProgressStatistics::new();
    let json = serde_json::to_string(&stats).unwrap();
    assert!(json.contains("\"total_tasks\":0"));
    assert!(json.contains("\"completion_percentage\":0.0"));
    // Empty arrays should be omitted
    assert!(!json.contains("\"top_applied_learnings\""));
}

// ============ LearningsByOutcome tests ============

#[test]
fn test_learnings_by_outcome_new() {
    let counts = LearningsByOutcome::new();
    assert_eq!(counts.failures, 0);
    assert_eq!(counts.successes, 0);
    assert_eq!(counts.workarounds, 0);
    assert_eq!(counts.patterns, 0);
}

#[test]
fn test_learnings_by_outcome_default() {
    let counts = LearningsByOutcome::default();
    assert_eq!(counts.failures, 0);
}

#[test]
fn test_learnings_by_outcome_increment() {
    let mut counts = LearningsByOutcome::new();

    counts.increment(LearningOutcome::Failure);
    counts.increment(LearningOutcome::Success);
    counts.increment(LearningOutcome::Workaround);
    counts.increment(LearningOutcome::Pattern);
    counts.increment(LearningOutcome::Failure);

    assert_eq!(counts.failures, 2);
    assert_eq!(counts.successes, 1);
    assert_eq!(counts.workarounds, 1);
    assert_eq!(counts.patterns, 1);
}

// ============ TaskErrorSummary tests ============

#[test]
fn test_task_error_summary_new() {
    let summary = TaskErrorSummary::new("US-001", "Test task", 3, TaskStatus::Blocked);
    assert_eq!(summary.task_id, "US-001");
    assert_eq!(summary.title, "Test task");
    assert_eq!(summary.error_count, 3);
    assert!(summary.last_error.is_none());
    assert_eq!(summary.status, TaskStatus::Blocked);
}

#[test]
fn test_task_error_summary_serialization() {
    let mut summary = TaskErrorSummary::new("US-001", "Test task", 2, TaskStatus::Todo);
    summary.last_error = Some("Connection failed".to_string());

    let json = serde_json::to_string(&summary).unwrap();
    assert!(json.contains("\"task_id\":\"US-001\""));
    assert!(json.contains("\"error_count\":2"));
    assert!(json.contains("\"last_error\":\"Connection failed\""));
}
