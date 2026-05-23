//! Complete command — mark tasks done. Routes the status mutation through
//! `TaskLifecycle::apply` (PRD §6 Category A). Retains CLI-specific
//! pre-validation (dependencies, required tests, force gate) and
//! post-apply bookkeeping (`runs.last_commit`, `runs.iteration_count`).

use std::process::Command;

use rusqlite::Connection;
use serde::Serialize;

use crate::commands::dependency_checker::check_dependencies_satisfied;
use crate::lifecycle::{TaskLifecycle, TransitionChange, TransitionIntent, TransitionSource};
use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

pub use crate::commands::dependency_checker::are_dependencies_satisfied;

#[derive(Debug, Clone, Serialize)]
pub struct TaskCompletionResult {
    pub task_id: String,
    pub previous_status: TaskStatus,
    pub was_already_done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompleteResult {
    pub tasks: Vec<TaskCompletionResult>,
    pub completed_count: usize,
    pub already_done_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Complete one or more tasks. Pre-validates dependencies, required tests,
/// and transition validity (when `!force`) on every task before any writes;
/// either all tasks pass or the call returns an error without touching any
/// row. Status mutation runs through `TaskLifecycle::apply`; per-run
/// `runs.last_commit` / `iteration_count` updates happen as a follow-up.
pub fn complete(
    conn: &mut Connection,
    task_ids: &[String],
    run_id: Option<&str>,
    commit: Option<&str>,
    force: bool,
) -> TaskMgrResult<CompleteResult> {
    // Snapshot previous statuses + warnings before any writes. Failing
    // here preserves the legacy all-or-nothing semantics.
    let mut previous_statuses = Vec::with_capacity(task_ids.len());
    let mut warnings = Vec::with_capacity(task_ids.len());
    for task_id in task_ids {
        let prev_str: String = conn
            .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |r| {
                r.get(0)
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
                _ => TaskMgrError::from(e),
            })?;
        let previous: TaskStatus = prev_str.parse()?;
        let done = previous == TaskStatus::Done;
        let needs_force = !done && previous != TaskStatus::InProgress;
        if !done && !force {
            check_dependencies_satisfied(conn, task_id)?;
            check_required_tests_pass(conn, task_id)?;
        }
        if needs_force && !force {
            return Err(invalid_transition_error(task_id, previous));
        }
        warnings.push(if needs_force && force {
            Some(format!(
                "Forced completion: task was in '{previous}' status (invalid transition, overridden with --force)."
            ))
        } else if needs_force {
            Some(format!(
                "Task was in '{previous}' status, not 'in_progress'. Completing anyway."
            ))
        } else {
            None
        });
        previous_statuses.push(previous);
    }

    // Force-advance non-InProgress / non-Done rows to InProgress via the
    // lifecycle's race-safe claim before apply() — the matrix gate inside
    // `TaskLifecycle::apply` would otherwise reject e.g. Operator Todo →
    // Done. Routing through `try_claim` keeps the raw `UPDATE tasks SET
    // status` SQL inside the lifecycle service (PRD §11 invariant).
    if force {
        let advancable: Vec<&String> = task_ids
            .iter()
            .zip(previous_statuses.iter())
            .filter_map(|(id, prev)| {
                (!matches!(prev, TaskStatus::InProgress | TaskStatus::Done)).then_some(id)
            })
            .collect();
        if !advancable.is_empty() {
            let lc = TaskLifecycle::new(conn);
            for id in advancable {
                // Conditional WHERE on the row's actual prior status — when
                // the row has already advanced via a concurrent process,
                // try_claim returns Ok(false) and we leave it alone.
                let _ = lc.try_claim(
                    id,
                    &[
                        TaskStatus::Todo,
                        TaskStatus::Blocked,
                        TaskStatus::Skipped,
                        TaskStatus::Irrelevant,
                    ],
                );
            }
        }
    }

    let intents: Vec<TransitionIntent> = task_ids
        .iter()
        .map(|id| TransitionIntent {
            task_id: id.clone(),
            change: TransitionChange::Done,
            source: TransitionSource::Operator,
            reason: None,
            fail_status: None,
            audit_note: None,
        })
        .collect();
    let outcomes = {
        let mut lc = match run_id {
            Some(rid) => TaskLifecycle::with_run(conn, rid),
            None => TaskLifecycle::new(conn),
        };
        lc.apply(&intents)
    };
    for outcome in &outcomes {
        if !outcome.applied
            && let Some(crate::lifecycle::TransitionRejectReason::DispatchFailed(msg)) =
                &outcome.reason
        {
            return Err(TaskMgrError::lock_error_with_hint(
                format!("complete dispatch failed for {}: {msg}", outcome.task_id),
                "internal lifecycle dispatch error; check earlier stderr for details",
            ));
        }
    }

    if let Some(rid) = run_id {
        if let Some(commit_hash) = commit {
            conn.execute(
                "UPDATE runs SET last_commit = ? WHERE run_id = ?",
                rusqlite::params![commit_hash, rid],
            )?;
        }
        conn.execute(
            "UPDATE runs SET iteration_count = iteration_count + 1 WHERE run_id = ?",
            [rid],
        )?;
    }

    let mut completed_count = 0;
    let mut already_done_count = 0;
    let tasks: Vec<TaskCompletionResult> = task_ids
        .iter()
        .zip(previous_statuses)
        .zip(warnings)
        .map(|((id, previous_status), warning)| {
            let was_already_done = previous_status == TaskStatus::Done;
            if was_already_done {
                already_done_count += 1;
            } else {
                completed_count += 1;
            }
            TaskCompletionResult {
                task_id: id.clone(),
                previous_status,
                was_already_done,
                warning,
            }
        })
        .collect();
    Ok(CompleteResult {
        tasks,
        completed_count,
        already_done_count,
        run_id: run_id.map(String::from),
        commit: commit.map(String::from),
    })
}

/// CLI-side invalid-transition hint (references `task-mgr next --claim` /
/// `--force` affordances, so it doesn't belong in the lifecycle service).
fn invalid_transition_error(task_id: &str, previous: TaskStatus) -> TaskMgrError {
    let valid = previous.valid_transitions();
    let hint = if valid.is_empty() {
        format!(
            "Task '{task_id}' is in '{previous}' status which is a terminal state. No transitions allowed."
        )
    } else if previous == TaskStatus::Todo {
        format!(
            "Task '{task_id}' is in 'todo' status. Use 'task-mgr next --claim {task_id}' to claim it first, then complete. Or use --force to override."
        )
    } else {
        format!(
            "Task '{task_id}' is in '{previous}' status. Valid transitions: {}. Use --force to override.",
            valid.join(", ")
        )
    };
    TaskMgrError::invalid_transition(task_id, previous.to_string(), "done", hint)
}

/// Check that all required tests pass for a task. Queries `required_tests`;
/// empty/null → Ok. For each filter, runs `cargo test <filter>`.
fn check_required_tests_pass(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    let required_tests: Option<String> = conn
        .query_row(
            "SELECT required_tests FROM tasks WHERE id = ?",
            [task_id],
            |row| row.get(0),
        )
        .unwrap_or(None);

    let filters: Vec<String> = match required_tests {
        Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
        _ => return Ok(()),
    };
    if filters.is_empty() {
        return Ok(());
    }

    let mut failed = Vec::new();
    for filter in &filters {
        let status = Command::new("cargo")
            .args(["test", filter, "--", "--no-capture"])
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => failed.push(filter.clone()),
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(TaskMgrError::RequiredTestsFailed {
            task_id: task_id.to_string(),
            failed_tests: failed.join(", "),
        })
    }
}

#[must_use]
pub fn format_text(result: &CompleteResult) -> String {
    let mut output = String::new();
    if result.tasks.len() == 1 {
        let task = &result.tasks[0];
        if task.was_already_done {
            output.push_str(&format!("Task {} was already done.\n", task.task_id));
        } else {
            output.push_str(&format!(
                "Completed task {} (was {}).\n",
                task.task_id, task.previous_status
            ));
        }
        if let Some(ref warning) = task.warning {
            output.push_str(&format!("Warning: {warning}\n"));
        }
    } else {
        output.push_str(&format!(
            "Completed {} task(s), {} already done.\n",
            result.completed_count, result.already_done_count
        ));
        for task in &result.tasks {
            if task.was_already_done {
                output.push_str(&format!("  - {} (already done)\n", task.task_id));
            } else {
                output.push_str(&format!(
                    "  - {} (was {})\n",
                    task.task_id, task.previous_status
                ));
            }
            if let Some(ref warning) = task.warning {
                output.push_str(&format!("    Warning: {warning}\n"));
            }
        }
    }
    if let Some(ref commit) = result.commit {
        output.push_str(&format!("Recorded commit: {commit}\n"));
    }
    if let Some(ref rid) = result.run_id {
        output.push_str(&format!("Run: {rid}\n"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, migrations::run_migrations, open_connection};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_test_task(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, 'Test Task', ?, 10)",
            rusqlite::params![id, status],
        )
        .unwrap();
    }

    #[test]
    fn test_complete_single_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        let result = complete(&mut conn, &["US-001".to_string()], None, None, false).unwrap();

        assert_eq!(result.completed_count, 1);
        assert_eq!(result.already_done_count, 0);
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].task_id, "US-001");
        assert_eq!(result.tasks[0].previous_status, TaskStatus::InProgress);
        assert!(!result.tasks[0].was_already_done);
        assert!(result.tasks[0].warning.is_none());

        // Verify status in database
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");
    }

    #[test]
    fn test_complete_multiple_tasks() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");
        insert_test_task(&conn, "US-002", "in_progress");
        insert_test_task(&conn, "US-003", "in_progress");

        let result = complete(
            &mut conn,
            &[
                "US-001".to_string(),
                "US-002".to_string(),
                "US-003".to_string(),
            ],
            None,
            None,
            false,
        )
        .unwrap();

        assert_eq!(result.completed_count, 3);
        assert_eq!(result.already_done_count, 0);
        assert_eq!(result.tasks.len(), 3);

        // Verify all tasks are done
        for task_id in ["US-001", "US-002", "US-003"] {
            let status: String = conn
                .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(status, "done");
        }
    }

    #[test]
    fn test_complete_already_done_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "done");

        let result = complete(&mut conn, &["US-001".to_string()], None, None, false).unwrap();

        assert_eq!(result.completed_count, 0);
        assert_eq!(result.already_done_count, 1);
        assert!(result.tasks[0].was_already_done);
        assert_eq!(result.tasks[0].previous_status, TaskStatus::Done);
    }

    #[test]
    fn test_complete_todo_task_requires_force() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "todo");

        // Without force, should return InvalidTransition error
        let result = complete(&mut conn, &["US-001".to_string()], None, None, false);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidTransition {
                task_id, from, to, ..
            }) => {
                assert_eq!(task_id, "US-001");
                assert_eq!(from, "todo");
                assert_eq!(to, "done");
            }
            _ => panic!("Expected InvalidTransition error"),
        }
    }

    #[test]
    fn test_complete_todo_task_with_force() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "todo");

        // With force, should complete but with warning
        let result = complete(&mut conn, &["US-001".to_string()], None, None, true).unwrap();

        assert_eq!(result.completed_count, 1);
        assert!(!result.tasks[0].was_already_done);
        assert!(result.tasks[0].warning.is_some());
        assert!(
            result.tasks[0]
                .warning
                .as_ref()
                .unwrap()
                .contains("--force")
        );
    }

    #[test]
    fn test_complete_nonexistent_task() {
        let (_dir, mut conn) = setup_test_db();

        let result = complete(&mut conn, &["NONEXISTENT".to_string()], None, None, false);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_complete_with_run_id() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        // Create a run and run_task entry
        conn.execute(
            "INSERT INTO runs (run_id, status, iteration_count) VALUES ('run-123', 'active', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration, started_at) VALUES ('run-123', 'US-001', 'started', 1, datetime('now'))",
            [],
        )
        .unwrap();

        let result = complete(
            &mut conn,
            &["US-001".to_string()],
            Some("run-123"),
            None,
            false,
        )
        .unwrap();

        assert_eq!(result.run_id, Some("run-123".to_string()));

        // Verify run_tasks was updated
        let run_task_status: String = conn
            .query_row(
                "SELECT status FROM run_tasks WHERE run_id = 'run-123' AND task_id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_task_status, "completed");

        // Verify iteration_count was incremented
        let iteration_count: i32 = conn
            .query_row(
                "SELECT iteration_count FROM runs WHERE run_id = 'run-123'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(iteration_count, 1);
    }

    #[test]
    fn test_complete_with_commit() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        // Create a run
        conn.execute(
            "INSERT INTO runs (run_id, status, iteration_count) VALUES ('run-456', 'active', 0)",
            [],
        )
        .unwrap();

        let result = complete(
            &mut conn,
            &["US-001".to_string()],
            Some("run-456"),
            Some("abc123def"),
            false,
        )
        .unwrap();

        assert_eq!(result.commit, Some("abc123def".to_string()));

        // Verify last_commit was updated
        let last_commit: Option<String> = conn
            .query_row(
                "SELECT last_commit FROM runs WHERE run_id = 'run-456'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last_commit, Some("abc123def".to_string()));
    }

    #[test]
    fn test_complete_sets_completed_at() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        complete(&mut conn, &["US-001".to_string()], None, None, false).unwrap();

        // Verify completed_at was set
        let completed_at: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(completed_at.is_some());
    }

    #[test]
    fn test_complete_mixed_statuses_with_force() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");
        insert_test_task(&conn, "US-002", "done");
        insert_test_task(&conn, "US-003", "todo");

        // Use force=true since US-003 is in todo status
        let result = complete(
            &mut conn,
            &[
                "US-001".to_string(),
                "US-002".to_string(),
                "US-003".to_string(),
            ],
            None,
            None,
            true,
        )
        .unwrap();

        assert_eq!(result.completed_count, 2); // US-001 and US-003
        assert_eq!(result.already_done_count, 1); // US-002
        assert!(result.tasks[2].warning.is_some()); // US-003 had warning about force
    }

    #[test]
    fn test_format_text_single_task() {
        let result = CompleteResult {
            tasks: vec![TaskCompletionResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::InProgress,
                was_already_done: false,
                warning: None,
            }],
            completed_count: 1,
            already_done_count: 0,
            run_id: None,
            commit: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Completed task US-001"));
        assert!(text.contains("was in_progress"));
    }

    #[test]
    fn test_format_text_already_done() {
        let result = CompleteResult {
            tasks: vec![TaskCompletionResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::Done,
                was_already_done: true,
                warning: None,
            }],
            completed_count: 0,
            already_done_count: 1,
            run_id: None,
            commit: None,
        };

        let text = format_text(&result);
        assert!(text.contains("already done"));
    }

    #[test]
    fn test_format_text_with_warning() {
        let result = CompleteResult {
            tasks: vec![TaskCompletionResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::Todo,
                was_already_done: false,
                warning: Some("Task was in 'todo' status".to_string()),
            }],
            completed_count: 1,
            already_done_count: 0,
            run_id: None,
            commit: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Warning:"));
    }

    #[test]
    fn test_format_text_multiple_tasks() {
        let result = CompleteResult {
            tasks: vec![
                TaskCompletionResult {
                    task_id: "US-001".to_string(),
                    previous_status: TaskStatus::InProgress,
                    was_already_done: false,
                    warning: None,
                },
                TaskCompletionResult {
                    task_id: "US-002".to_string(),
                    previous_status: TaskStatus::InProgress,
                    was_already_done: false,
                    warning: None,
                },
            ],
            completed_count: 2,
            already_done_count: 0,
            run_id: Some("run-123".to_string()),
            commit: Some("abc123".to_string()),
        };

        let text = format_text(&result);
        assert!(text.contains("Completed 2 task(s)"));
        assert!(text.contains("US-001"));
        assert!(text.contains("US-002"));
        assert!(text.contains("Recorded commit: abc123"));
        assert!(text.contains("Run: run-123"));
    }

    #[test]
    fn test_complete_duration_calculation() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        // Create a run and run_task entry with a known started_at
        conn.execute(
            "INSERT INTO runs (run_id, status, iteration_count) VALUES ('run-789', 'active', 0)",
            [],
        )
        .unwrap();
        // Set started_at to 60 seconds ago
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration, started_at) VALUES ('run-789', 'US-001', 'started', 1, datetime('now', '-60 seconds'))",
            [],
        )
        .unwrap();

        complete(
            &mut conn,
            &["US-001".to_string()],
            Some("run-789"),
            None,
            false,
        )
        .unwrap();

        // Verify duration_seconds is approximately 60
        let duration: Option<i64> = conn
            .query_row(
                "SELECT duration_seconds FROM run_tasks WHERE run_id = 'run-789' AND task_id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Allow some tolerance for test execution time
        assert!(duration.is_some());
        let d = duration.unwrap();
        assert!((59..=65).contains(&d), "Duration was {} seconds", d);
    }

    // --- Dependency gating tests ---

    fn insert_relationship(conn: &Connection, task_id: &str, related_id: &str, rel_type: &str) {
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?, ?, ?)",
            rusqlite::params![task_id, related_id, rel_type],
        )
        .unwrap();
    }

    #[test]
    fn test_complete_blocked_by_unsatisfied_dependency() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "todo");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, false);
        assert!(result.is_err());
        match result {
            Err(TaskMgrError::DependencyNotSatisfied {
                task_id,
                unsatisfied,
                ..
            }) => {
                assert_eq!(task_id, "TASK-001");
                assert!(unsatisfied.contains("DEP-001"));
            }
            other => panic!("Expected DependencyNotSatisfied, got {:?}", other),
        }
    }

    #[test]
    fn test_complete_succeeds_when_dependencies_satisfied() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "done");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().completed_count, 1);
    }

    #[test]
    fn test_complete_force_bypasses_dependency_check() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "todo");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, true);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().completed_count, 1);
    }

    #[test]
    fn test_complete_no_dependencies_succeeds() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "TASK-001", "in_progress");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().completed_count, 1);
    }

    #[test]
    fn test_complete_dependency_on_irrelevant_task_succeeds() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "irrelevant");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().completed_count, 1);
    }

    #[test]
    fn test_complete_circular_dependency_with_force_bypasses() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "A", "in_progress");
        insert_test_task(&conn, "B", "in_progress");
        insert_relationship(&conn, "A", "B", "dependsOn");
        insert_relationship(&conn, "B", "A", "dependsOn");

        // Without force, both should fail
        let result_a = complete(&mut conn, &["A".to_string()], None, None, false);
        assert!(result_a.is_err());

        // With force, both complete
        let result_a = complete(&mut conn, &["A".to_string()], None, None, true);
        assert!(result_a.is_ok());
        let result_b = complete(&mut conn, &["B".to_string()], None, None, true);
        assert!(result_b.is_ok());
    }

    // --- retry tracking reset tests (TDD for FEAT-004) ---

    /// complete() must reset consecutive_failures to 0 for the completed task.
    ///
    /// Uses `test_utils::setup_test_db` (runs migrations) to ensure the
    /// `consecutive_failures` column added by migration v13 is present.
    #[test]
    fn test_complete_resets_consecutive_failures_to_zero() {
        use crate::loop_engine::test_utils::setup_test_db as setup_migrated_db;
        let (_dir, mut conn) = setup_migrated_db();
        // Insert task with consecutive_failures=3 (has been failing repeatedly)
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, consecutive_failures) \
             VALUES ('T-001', 'Test', 'in_progress', 10, 3)",
            [],
        )
        .unwrap();

        complete(&mut conn, &["T-001".to_string()], None, None, false).unwrap();

        let count: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "complete() must reset consecutive_failures to 0");
    }

    /// Completing one task must not reset a different task's consecutive_failures.
    ///
    /// Uses `test_utils::setup_test_db` (runs migrations) to ensure the
    /// `consecutive_failures` column added by migration v13 is present.
    #[test]
    fn test_complete_does_not_reset_other_tasks_consecutive_failures() {
        use crate::loop_engine::test_utils::setup_test_db as setup_migrated_db;
        let (_dir, mut conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, consecutive_failures) \
             VALUES ('T-001', 'Task A', 'in_progress', 10, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, consecutive_failures) \
             VALUES ('T-002', 'Task B', 'in_progress', 10, 0)",
            [],
        )
        .unwrap();

        // Completing T-002 must NOT touch T-001's counter
        complete(&mut conn, &["T-002".to_string()], None, None, false).unwrap();

        let count_a: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count_a, 2,
            "completing T-002 must not reset T-001's consecutive_failures"
        );
    }
}
