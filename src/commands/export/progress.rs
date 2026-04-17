//! Progress JSON export logic.
//!
//! This module handles exporting runs, learnings, and statistics to progress.json.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::models::{
    Confidence, LearningOutcome, RunStatus, RunTaskStatus, TaskStatus, parse_datetime,
    parse_optional_datetime,
};
use crate::models::{LearningExport, ProgressExport, ProgressStatistics, RunExport, RunTaskExport};

use super::write_json_atomic;

/// Export progress data (runs and learnings) to a file.
pub(crate) fn export_progress(
    conn: &Connection,
    dir: &Path,
    path: &Path,
) -> TaskMgrResult<(usize, usize)> {
    // Get global iteration counter
    let iteration_counter: i32 = conn.query_row(
        "SELECT iteration_counter FROM global_state WHERE id = 1",
        [],
        |row| row.get(0),
    )?;

    let mut progress = ProgressExport::new(
        dir.join("tasks.db").display().to_string(),
        iteration_counter,
    );

    // Load runs
    let runs = load_runs(conn)?;
    let runs_count = runs.len();
    progress.runs = runs;

    // Load learnings
    let learnings = load_learnings(conn)?;
    let learnings_count = learnings.len();
    progress.learnings = learnings;

    // Calculate statistics
    progress.statistics = Some(calculate_statistics(conn)?);

    write_json_atomic(path, &progress)?;

    Ok((runs_count, learnings_count))
}

/// Load all runs with their tasks.
pub(crate) fn load_runs(conn: &Connection) -> TaskMgrResult<Vec<RunExport>> {
    let mut stmt = conn.prepare(
        r#"SELECT run_id, started_at, ended_at, status, last_commit, last_files,
           iteration_count, notes
           FROM runs ORDER BY started_at DESC"#,
    )?;

    let run_rows = stmt.query_map([], |row| {
        let run_id: String = row.get(0)?;
        let started_at_str: String = row.get(1)?;
        let ended_at_str: Option<String> = row.get(2)?;
        let status_str: String = row.get(3)?;
        let last_commit: Option<String> = row.get(4)?;
        let last_files_str: Option<String> = row.get(5)?;
        let iteration_count: i32 = row.get(6)?;
        let notes: Option<String> = row.get(7)?;

        Ok((
            run_id,
            started_at_str,
            ended_at_str,
            status_str,
            last_commit,
            last_files_str,
            iteration_count,
            notes,
        ))
    })?;

    // Load run_tasks for each run
    let run_tasks_map = load_all_run_tasks(conn)?;

    let mut runs = Vec::new();
    for row in run_rows {
        let (
            run_id,
            started_at_str,
            ended_at_str,
            status_str,
            last_commit,
            last_files_str,
            iteration_count,
            notes,
        ) = row?;

        let started_at = parse_datetime(&started_at_str)?;
        let ended_at = parse_optional_datetime(ended_at_str)?;
        let status = RunStatus::from_str(&status_str).unwrap_or(RunStatus::Active);
        let last_files: Vec<String> = last_files_str
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        // Calculate duration if ended
        let duration_seconds = ended_at.map(|end| (end - started_at).num_seconds());

        let tasks = run_tasks_map.get(&run_id).cloned().unwrap_or_default();

        runs.push(RunExport {
            run_id,
            started_at,
            ended_at,
            status,
            last_commit,
            last_files,
            iteration_count,
            notes,
            tasks,
            duration_seconds,
        });
    }

    Ok(runs)
}

/// Load all run_tasks into a map keyed by run_id.
fn load_all_run_tasks(conn: &Connection) -> TaskMgrResult<HashMap<String, Vec<RunTaskExport>>> {
    let mut stmt = conn.prepare(
        r#"SELECT run_id, task_id, status, iteration, started_at, ended_at,
           duration_seconds, notes
           FROM run_tasks WHERE archived_at IS NULL ORDER BY run_id, iteration, started_at"#,
    )?;

    let rows = stmt.query_map([], |row| {
        let run_id: String = row.get(0)?;
        let task_id: String = row.get(1)?;
        let status_str: String = row.get(2)?;
        let iteration: i32 = row.get(3)?;
        let started_at_str: String = row.get(4)?;
        let ended_at_str: Option<String> = row.get(5)?;
        let duration_seconds: Option<i64> = row.get(6)?;
        let notes: Option<String> = row.get(7)?;

        Ok((
            run_id,
            task_id,
            status_str,
            iteration,
            started_at_str,
            ended_at_str,
            duration_seconds,
            notes,
        ))
    })?;

    let mut map: HashMap<String, Vec<RunTaskExport>> = HashMap::new();
    for row in rows {
        let (
            run_id,
            task_id,
            status_str,
            iteration,
            started_at_str,
            ended_at_str,
            duration_seconds,
            notes,
        ) = row?;

        let started_at = parse_datetime(&started_at_str)?;
        let ended_at = parse_optional_datetime(ended_at_str)?;
        let status = RunTaskStatus::from_str(&status_str).unwrap_or(RunTaskStatus::Started);

        let task_export = RunTaskExport {
            task_id,
            status,
            iteration,
            started_at,
            ended_at,
            duration_seconds,
            notes,
        };

        map.entry(run_id).or_default().push(task_export);
    }

    Ok(map)
}

/// Load all learnings with their tags.
pub(crate) fn load_learnings(conn: &Connection) -> TaskMgrResult<Vec<LearningExport>> {
    let mut stmt = conn.prepare(
        r#"SELECT id, created_at, task_id, run_id, outcome, title, content,
           root_cause, solution, applies_to_files, applies_to_task_types,
           applies_to_errors, confidence, times_shown, times_applied,
           last_shown_at, last_applied_at
           FROM learnings WHERE retired_at IS NULL ORDER BY created_at DESC"#,
    )?;

    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let created_at_str: String = row.get(1)?;
        let task_id: Option<String> = row.get(2)?;
        let run_id: Option<String> = row.get(3)?;
        let outcome_str: String = row.get(4)?;
        let title: String = row.get(5)?;
        let content: String = row.get(6)?;
        let root_cause: Option<String> = row.get(7)?;
        let solution: Option<String> = row.get(8)?;
        let applies_to_files_str: Option<String> = row.get(9)?;
        let applies_to_task_types_str: Option<String> = row.get(10)?;
        let applies_to_errors_str: Option<String> = row.get(11)?;
        let confidence_str: String = row.get(12)?;
        let times_shown: i32 = row.get(13)?;
        let times_applied: i32 = row.get(14)?;
        let last_shown_at_str: Option<String> = row.get(15)?;
        let last_applied_at_str: Option<String> = row.get(16)?;

        Ok((
            id,
            created_at_str,
            task_id,
            run_id,
            outcome_str,
            title,
            content,
            root_cause,
            solution,
            applies_to_files_str,
            applies_to_task_types_str,
            applies_to_errors_str,
            confidence_str,
            times_shown,
            times_applied,
            last_shown_at_str,
            last_applied_at_str,
        ))
    })?;

    // Load all tags
    let tags_map = load_all_learning_tags(conn)?;

    let mut learnings = Vec::new();
    for row in rows {
        let (
            id,
            created_at_str,
            task_id,
            run_id,
            outcome_str,
            title,
            content,
            root_cause,
            solution,
            applies_to_files_str,
            applies_to_task_types_str,
            applies_to_errors_str,
            confidence_str,
            times_shown,
            times_applied,
            last_shown_at_str,
            last_applied_at_str,
        ) = row?;

        let created_at = parse_datetime(&created_at_str)?;
        let outcome = LearningOutcome::from_str(&outcome_str).unwrap_or(LearningOutcome::Pattern);
        let confidence = Confidence::from_str(&confidence_str).unwrap_or(Confidence::Medium);
        let last_shown_at = parse_optional_datetime(last_shown_at_str)?;
        let last_applied_at = parse_optional_datetime(last_applied_at_str)?;

        // Parse JSON arrays
        let applies_to_files: Option<Vec<String>> =
            applies_to_files_str.and_then(|s| serde_json::from_str(&s).ok());
        let applies_to_task_types: Option<Vec<String>> =
            applies_to_task_types_str.and_then(|s| serde_json::from_str(&s).ok());
        let applies_to_errors: Option<Vec<String>> =
            applies_to_errors_str.and_then(|s| serde_json::from_str(&s).ok());

        let tags = tags_map.get(&id).cloned().unwrap_or_default();

        learnings.push(LearningExport {
            id: Some(id),
            created_at,
            task_id,
            run_id,
            outcome,
            title,
            content,
            root_cause,
            solution,
            applies_to_files,
            applies_to_task_types,
            applies_to_errors,
            confidence,
            times_shown,
            times_applied,
            last_shown_at,
            last_applied_at,
            tags,
        });
    }

    Ok(learnings)
}

/// Load all learning tags into a map keyed by learning_id.
fn load_all_learning_tags(conn: &Connection) -> TaskMgrResult<HashMap<i64, Vec<String>>> {
    let mut stmt =
        conn.prepare("SELECT learning_id, tag FROM learning_tags ORDER BY learning_id, tag")?;
    let rows = stmt.query_map([], |row| {
        let learning_id: i64 = row.get(0)?;
        let tag: String = row.get(1)?;
        Ok((learning_id, tag))
    })?;

    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    for row in rows {
        let (learning_id, tag) = row?;
        map.entry(learning_id).or_default().push(tag);
    }

    Ok(map)
}

/// Calculate progress statistics from the database.
pub(crate) fn calculate_statistics(conn: &Connection) -> TaskMgrResult<ProgressStatistics> {
    let mut stats = ProgressStatistics::new();

    // Count tasks by status
    let mut stmt = conn
        .prepare("SELECT status, COUNT(*) FROM tasks WHERE archived_at IS NULL GROUP BY status")?;
    let rows = stmt.query_map([], |row| {
        let status: String = row.get(0)?;
        let count: i32 = row.get(1)?;
        Ok((status, count))
    })?;

    for row in rows {
        let (status_str, count) = row?;
        if let Ok(status) = TaskStatus::from_str(&status_str) {
            for _ in 0..count {
                stats.increment_task_status(status);
            }
        }
    }

    // Count runs by status
    let mut stmt = conn
        .prepare("SELECT status, COUNT(*) FROM runs WHERE archived_at IS NULL GROUP BY status")?;
    let rows = stmt.query_map([], |row| {
        let status: String = row.get(0)?;
        let count: i32 = row.get(1)?;
        Ok((status, count))
    })?;

    for row in rows {
        let (status_str, count) = row?;
        if let Ok(status) = RunStatus::from_str(&status_str) {
            for _ in 0..count {
                stats.increment_run_status(status);
            }
        }
    }

    // Count learnings by outcome
    let mut stmt = conn.prepare(
        "SELECT outcome, COUNT(*) FROM learnings WHERE retired_at IS NULL GROUP BY outcome",
    )?;
    let rows = stmt.query_map([], |row| {
        let outcome: String = row.get(0)?;
        let count: i32 = row.get(1)?;
        Ok((outcome, count))
    })?;

    for row in rows {
        let (outcome_str, count) = row?;
        if let Ok(outcome) = LearningOutcome::from_str(&outcome_str) {
            for _ in 0..count {
                stats.increment_learning_outcome(outcome);
            }
        }
    }

    stats.calculate_completion_percentage();

    Ok(stats)
}

// Import FromStr implementations from models
use std::str::FromStr;
