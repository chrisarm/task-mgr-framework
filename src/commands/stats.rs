//! Progress summary statistics.
//!
//! This module implements the `stats` command which displays
//! a summary of task progress, learnings, and run information.

use rusqlite::Connection;
use serde::Serialize;

use crate::db::open_connection;
use crate::TaskMgrResult;

/// Result of the stats command.
#[derive(Debug, Serialize)]
pub struct StatsResult {
    /// Task counts by status
    pub tasks: TaskCounts,
    /// Completion percentage (done / total * 100)
    pub completion_percentage: f64,
    /// Learnings counts by outcome type
    pub learnings: LearningCounts,
    /// Current active run info, if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_run: Option<ActiveRunInfo>,
    /// Global iteration count
    pub global_iteration: i64,
}

/// Task counts by status.
#[derive(Debug, Serialize)]
pub struct TaskCounts {
    /// Total tasks
    pub total: i64,
    /// Tasks with status 'todo'
    pub todo: i64,
    /// Tasks with status 'in_progress'
    pub in_progress: i64,
    /// Tasks with status 'done'
    pub done: i64,
    /// Tasks with status 'blocked'
    pub blocked: i64,
    /// Tasks with status 'skipped'
    pub skipped: i64,
    /// Tasks with status 'irrelevant'
    pub irrelevant: i64,
}

/// Learning counts by outcome type.
#[derive(Debug, Serialize)]
pub struct LearningCounts {
    /// Total learnings
    pub total: i64,
    /// Learnings with outcome 'failure'
    pub failure: i64,
    /// Learnings with outcome 'success'
    pub success: i64,
    /// Learnings with outcome 'workaround'
    pub workaround: i64,
    /// Learnings with outcome 'pattern'
    pub pattern: i64,
}

/// Info about an active run.
#[derive(Debug, Serialize)]
pub struct ActiveRunInfo {
    /// The run ID
    pub run_id: String,
    /// When the run started
    pub started_at: String,
    /// Number of iterations in this run
    pub iteration_count: i64,
    /// Number of tasks completed in this run
    pub tasks_completed: i64,
    /// Number of tasks failed in this run
    pub tasks_failed: i64,
}

/// Get progress statistics.
///
/// # Arguments
///
/// * `dir` - Directory containing the database
///
/// # Returns
///
/// Returns a `StatsResult` with progress summary.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or queried.
pub fn stats(dir: &std::path::Path) -> TaskMgrResult<StatsResult> {
    let conn = open_connection(dir)?;

    let tasks = query_task_counts(&conn)?;
    let learnings = query_learning_counts(&conn)?;
    let active_run = query_active_run(&conn)?;
    let global_iteration = query_global_iteration(&conn)?;

    // Calculate completion percentage
    let completion_percentage = if tasks.total > 0 {
        (tasks.done as f64 / tasks.total as f64) * 100.0
    } else {
        0.0
    };

    Ok(StatsResult {
        tasks,
        completion_percentage,
        learnings,
        active_run,
        global_iteration,
    })
}

/// Query task counts by status.
fn query_task_counts(conn: &Connection) -> TaskMgrResult<TaskCounts> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            COUNT(*) as total,
            COALESCE(SUM(CASE WHEN status = 'todo' THEN 1 ELSE 0 END), 0) as todo,
            COALESCE(SUM(CASE WHEN status = 'in_progress' THEN 1 ELSE 0 END), 0) as in_progress,
            COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0) as done,
            COALESCE(SUM(CASE WHEN status = 'blocked' THEN 1 ELSE 0 END), 0) as blocked,
            COALESCE(SUM(CASE WHEN status = 'skipped' THEN 1 ELSE 0 END), 0) as skipped,
            COALESCE(SUM(CASE WHEN status = 'irrelevant' THEN 1 ELSE 0 END), 0) as irrelevant
        FROM tasks
        "#,
    )?;

    let counts = stmt.query_row([], |row| {
        Ok(TaskCounts {
            total: row.get(0)?,
            todo: row.get(1)?,
            in_progress: row.get(2)?,
            done: row.get(3)?,
            blocked: row.get(4)?,
            skipped: row.get(5)?,
            irrelevant: row.get(6)?,
        })
    })?;

    Ok(counts)
}

/// Query learning counts by outcome type.
fn query_learning_counts(conn: &Connection) -> TaskMgrResult<LearningCounts> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            COUNT(*) as total,
            COALESCE(SUM(CASE WHEN outcome = 'failure' THEN 1 ELSE 0 END), 0) as failure,
            COALESCE(SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END), 0) as success,
            COALESCE(SUM(CASE WHEN outcome = 'workaround' THEN 1 ELSE 0 END), 0) as workaround,
            COALESCE(SUM(CASE WHEN outcome = 'pattern' THEN 1 ELSE 0 END), 0) as pattern
        FROM learnings
        "#,
    )?;

    let counts = stmt.query_row([], |row| {
        Ok(LearningCounts {
            total: row.get(0)?,
            failure: row.get(1)?,
            success: row.get(2)?,
            workaround: row.get(3)?,
            pattern: row.get(4)?,
        })
    })?;

    Ok(counts)
}

/// Query active run information.
fn query_active_run(conn: &Connection) -> TaskMgrResult<Option<ActiveRunInfo>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT run_id, started_at, iteration_count
        FROM runs
        WHERE status = 'active'
        ORDER BY started_at DESC
        LIMIT 1
        "#,
    )?;

    let run_result: Result<(String, String, i64), _> =
        stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));

    match run_result {
        Ok((run_id, started_at, iteration_count)) => {
            // Get task completion stats for this run
            let (tasks_completed, tasks_failed) = query_run_task_stats(conn, &run_id)?;

            Ok(Some(ActiveRunInfo {
                run_id,
                started_at,
                iteration_count,
                tasks_completed,
                tasks_failed,
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Query task completion stats for a specific run.
fn query_run_task_stats(conn: &Connection, run_id: &str) -> TaskMgrResult<(i64, i64)> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as completed,
            SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as failed
        FROM run_tasks
        WHERE run_id = ?
        "#,
    )?;

    let (completed, failed): (i64, i64) = stmt.query_row([run_id], |row| {
        Ok((
            row.get::<_, Option<i64>>(0)?.unwrap_or(0),
            row.get::<_, Option<i64>>(1)?.unwrap_or(0),
        ))
    })?;

    Ok((completed, failed))
}

/// Query global iteration counter.
fn query_global_iteration(conn: &Connection) -> TaskMgrResult<i64> {
    let iteration: i64 = conn.query_row(
        "SELECT iteration_counter FROM global_state WHERE id = 1",
        [],
        |row| row.get(0),
    )?;

    Ok(iteration)
}

/// Format stats result as human-readable text.
pub fn format_text(result: &StatsResult) -> String {
    let mut output = String::new();

    output.push_str("=== Task Progress ===\n");
    output.push_str(&format!(
        "Total: {}  |  Completion: {:.1}%\n\n",
        result.tasks.total, result.completion_percentage
    ));

    output.push_str("Status breakdown:\n");
    output.push_str(&format!("  todo:        {:>4}\n", result.tasks.todo));
    output.push_str(&format!("  in_progress: {:>4}\n", result.tasks.in_progress));
    output.push_str(&format!("  done:        {:>4}\n", result.tasks.done));
    output.push_str(&format!("  blocked:     {:>4}\n", result.tasks.blocked));
    output.push_str(&format!("  skipped:     {:>4}\n", result.tasks.skipped));
    output.push_str(&format!("  irrelevant:  {:>4}\n", result.tasks.irrelevant));

    output.push_str("\n=== Learnings ===\n");
    output.push_str(&format!("Total: {}\n", result.learnings.total));
    if result.learnings.total > 0 {
        output.push_str(&format!("  failure:    {:>4}\n", result.learnings.failure));
        output.push_str(&format!("  success:    {:>4}\n", result.learnings.success));
        output.push_str(&format!(
            "  workaround: {:>4}\n",
            result.learnings.workaround
        ));
        output.push_str(&format!("  pattern:    {:>4}\n", result.learnings.pattern));
    }

    if let Some(ref run) = result.active_run {
        output.push_str("\n=== Active Run ===\n");
        output.push_str(&format!("Run ID:     {}\n", run.run_id));
        output.push_str(&format!("Started:    {}\n", run.started_at));
        output.push_str(&format!("Iterations: {}\n", run.iteration_count));
        output.push_str(&format!(
            "Tasks:      {} completed, {} failed\n",
            run.tasks_completed, run.tasks_failed
        ));
    }

    output.push_str(&format!(
        "\nGlobal iteration: {}\n",
        result.global_iteration
    ));

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::create_schema;
    use rusqlite::params;
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_test_task(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES (?, 'Test Task', ?)",
            params![id, status],
        )
        .unwrap();
    }

    fn insert_test_learning(conn: &Connection, outcome: &str) {
        conn.execute(
            "INSERT INTO learnings (outcome, title, content) VALUES (?, 'Test', 'Content')",
            params![outcome],
        )
        .unwrap();
    }

    fn insert_test_run(conn: &Connection, run_id: &str, status: &str) {
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES (?, ?)",
            params![run_id, status],
        )
        .unwrap();
    }

    #[test]
    fn test_stats_empty_database() {
        let (temp_dir, conn) = setup_test_db();
        drop(conn);

        let result = stats(temp_dir.path()).unwrap();
        assert_eq!(result.tasks.total, 0);
        assert_eq!(result.completion_percentage, 0.0);
        assert_eq!(result.learnings.total, 0);
        assert!(result.active_run.is_none());
        assert_eq!(result.global_iteration, 0);
    }

    #[test]
    fn test_stats_task_counts() {
        let (temp_dir, conn) = setup_test_db();

        // Insert tasks with various statuses
        insert_test_task(&conn, "US-001", "todo");
        insert_test_task(&conn, "US-002", "todo");
        insert_test_task(&conn, "US-003", "in_progress");
        insert_test_task(&conn, "US-004", "done");
        insert_test_task(&conn, "US-005", "done");
        insert_test_task(&conn, "US-006", "done");
        insert_test_task(&conn, "US-007", "blocked");
        insert_test_task(&conn, "US-008", "skipped");
        insert_test_task(&conn, "US-009", "irrelevant");

        drop(conn);

        let result = stats(temp_dir.path()).unwrap();
        assert_eq!(result.tasks.total, 9);
        assert_eq!(result.tasks.todo, 2);
        assert_eq!(result.tasks.in_progress, 1);
        assert_eq!(result.tasks.done, 3);
        assert_eq!(result.tasks.blocked, 1);
        assert_eq!(result.tasks.skipped, 1);
        assert_eq!(result.tasks.irrelevant, 1);
    }

    #[test]
    fn test_stats_completion_percentage() {
        let (temp_dir, conn) = setup_test_db();

        // 2 done out of 4 total = 50%
        insert_test_task(&conn, "US-001", "todo");
        insert_test_task(&conn, "US-002", "done");
        insert_test_task(&conn, "US-003", "done");
        insert_test_task(&conn, "US-004", "blocked");

        drop(conn);

        let result = stats(temp_dir.path()).unwrap();
        assert_eq!(result.completion_percentage, 50.0);
    }

    #[test]
    fn test_stats_learning_counts() {
        let (temp_dir, conn) = setup_test_db();

        insert_test_learning(&conn, "failure");
        insert_test_learning(&conn, "failure");
        insert_test_learning(&conn, "success");
        insert_test_learning(&conn, "workaround");
        insert_test_learning(&conn, "pattern");
        insert_test_learning(&conn, "pattern");

        drop(conn);

        let result = stats(temp_dir.path()).unwrap();
        assert_eq!(result.learnings.total, 6);
        assert_eq!(result.learnings.failure, 2);
        assert_eq!(result.learnings.success, 1);
        assert_eq!(result.learnings.workaround, 1);
        assert_eq!(result.learnings.pattern, 2);
    }

    #[test]
    fn test_stats_active_run() {
        let (temp_dir, conn) = setup_test_db();

        // Create a task first (needed for run_tasks FK)
        insert_test_task(&conn, "US-001", "done");

        // Create an active run
        insert_test_run(&conn, "run-001", "active");

        // Set iteration count
        conn.execute(
            "UPDATE runs SET iteration_count = 5 WHERE run_id = 'run-001'",
            [],
        )
        .unwrap();

        // Add a completed task to the run
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, iteration, status) VALUES ('run-001', 'US-001', 1, 'completed')",
            [],
        )
        .unwrap();

        drop(conn);

        let result = stats(temp_dir.path()).unwrap();
        assert!(result.active_run.is_some());
        let run = result.active_run.unwrap();
        assert_eq!(run.run_id, "run-001");
        assert_eq!(run.iteration_count, 5);
        assert_eq!(run.tasks_completed, 1);
        assert_eq!(run.tasks_failed, 0);
    }

    #[test]
    fn test_stats_no_active_run() {
        let (temp_dir, conn) = setup_test_db();

        // Create a completed run (not active)
        insert_test_run(&conn, "run-001", "completed");

        drop(conn);

        let result = stats(temp_dir.path()).unwrap();
        assert!(result.active_run.is_none());
    }

    #[test]
    fn test_stats_global_iteration() {
        let (temp_dir, conn) = setup_test_db();

        // Update global iteration counter
        conn.execute(
            "UPDATE global_state SET iteration_counter = 42 WHERE id = 1",
            [],
        )
        .unwrap();

        drop(conn);

        let result = stats(temp_dir.path()).unwrap();
        assert_eq!(result.global_iteration, 42);
    }

    #[test]
    fn test_format_text_with_data() {
        let result = StatsResult {
            tasks: TaskCounts {
                total: 10,
                todo: 3,
                in_progress: 1,
                done: 4,
                blocked: 1,
                skipped: 1,
                irrelevant: 0,
            },
            completion_percentage: 40.0,
            learnings: LearningCounts {
                total: 5,
                failure: 2,
                success: 1,
                workaround: 1,
                pattern: 1,
            },
            active_run: Some(ActiveRunInfo {
                run_id: "run-123".to_string(),
                started_at: "2024-01-01T00:00:00".to_string(),
                iteration_count: 10,
                tasks_completed: 5,
                tasks_failed: 1,
            }),
            global_iteration: 100,
        };

        let text = format_text(&result);
        assert!(text.contains("Total: 10"));
        assert!(text.contains("40.0%"));
        assert!(text.contains("todo:"));
        assert!(text.contains("Learnings"));
        assert!(text.contains("Active Run"));
        assert!(text.contains("run-123"));
        assert!(text.contains("Global iteration: 100"));
    }

    #[test]
    fn test_format_text_empty_database() {
        let result = StatsResult {
            tasks: TaskCounts {
                total: 0,
                todo: 0,
                in_progress: 0,
                done: 0,
                blocked: 0,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            learnings: LearningCounts {
                total: 0,
                failure: 0,
                success: 0,
                workaround: 0,
                pattern: 0,
            },
            active_run: None,
            global_iteration: 0,
        };

        let text = format_text(&result);
        assert!(text.contains("Total: 0"));
        assert!(text.contains("0.0%"));
        assert!(!text.contains("Active Run"));
    }

    // ========== TEST-INIT-001: retired_at Filtering Tests ==========
    //
    // Tests verify retired learnings are excluded from stats query.
    // #[ignore] until FEAT-001 and FEAT-002 are implemented.
    //
    // Query location covered:
    //  12. Stats query_learning_counts (query_learning_counts via get_stats)

    /// Sets `retired_at = NOW` on a learning.
    /// Requires FEAT-001 (retired_at column).
    fn retire_learning_stats(conn: &Connection, id: i64) {
        conn.execute(
            "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
            [id],
        )
        .expect("retire_learning: requires FEAT-001 (retired_at column in learnings)");
    }

    #[test]
    #[ignore = "requires FEAT-001 (retired_at migration) and FEAT-002 (retired_at IS NULL filters)"]
    fn test_retired_excluded_from_stats_query_learning_counts() {
        // AC: retired learning excluded from stats query_learning_counts
        use crate::learnings::{record_learning, RecordLearningParams};
        use crate::models::{Confidence, LearningOutcome};

        let (temp_dir, conn) = setup_test_db();

        // Active success learning (should be counted)
        let active = RecordLearningParams {
            outcome: LearningOutcome::Success,
            title: "Active stats learning".to_string(),
            content: "Should be counted".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        };
        record_learning(&conn, active).unwrap();

        // Retired failure learning (must NOT be counted)
        let retired_params = RecordLearningParams {
            outcome: LearningOutcome::Failure,
            title: "Retired stats learning".to_string(),
            content: "Must not be counted".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Low,
        };
        let retired_result = record_learning(&conn, retired_params).unwrap();
        retire_learning_stats(&conn, retired_result.learning_id);

        let counts = query_learning_counts(&conn).unwrap();

        assert_eq!(
            counts.total, 1,
            "query_learning_counts total must exclude retired learning (expected 1, got {})",
            counts.total
        );
        assert_eq!(
            counts.failure, 0,
            "retired failure learning must not be included in failure count"
        );
        assert_eq!(
            counts.success, 1,
            "active success learning must still be counted"
        );

        // Keep temp_dir alive until end of test
        drop(temp_dir);
    }
}
