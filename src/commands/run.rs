//! Run lifecycle management commands.
//!
//! This module provides commands for managing run sessions:
//! - `begin` - Start a new run session
//! - `update` - Update an active run with progress information
//! - `end` - End a run session (completed or aborted)
//!
//! Runs track execution sessions for auditing, recovery, and metrics.

use rusqlite::Connection;
use serde::Serialize;
use uuid::Uuid;

use crate::models::{Run, RunStatus};
use crate::{TaskMgrError, TaskMgrResult};

/// Result of beginning a new run.
#[derive(Debug, Clone, Serialize)]
pub struct BeginResult {
    /// The newly created run ID
    pub run_id: String,
    /// Status of the new run
    pub status: RunStatus,
}

/// Result of updating a run.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateResult {
    /// The run that was updated
    pub run_id: String,
    /// Whether last_commit was updated
    pub commit_updated: bool,
    /// Whether last_files was updated
    pub files_updated: bool,
    /// Current iteration count
    pub iteration_count: i32,
}

/// Result of ending a run.
#[derive(Debug, Clone, Serialize)]
pub struct EndResult {
    /// The run that was ended
    pub run_id: String,
    /// Previous status before ending
    pub previous_status: RunStatus,
    /// New status after ending
    pub new_status: RunStatus,
    /// Duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
}

/// Begin a new run session.
///
/// Creates a new run with a generated UUID and status='active'.
///
/// # Arguments
/// * `conn` - Database connection
///
/// # Returns
/// * `Ok(BeginResult)` - Information about the new run
/// * `Err(TaskMgrError)` - If database error occurs
pub fn begin(conn: &Connection) -> TaskMgrResult<BeginResult> {
    let run_id = Uuid::new_v4().to_string();

    conn.execute(
        "INSERT INTO runs (run_id, status, started_at) VALUES (?, 'active', datetime('now'))",
        [&run_id],
    )?;

    Ok(BeginResult {
        run_id,
        status: RunStatus::Active,
    })
}

/// Update an active run with progress information.
///
/// # Arguments
/// * `conn` - Database connection
/// * `run_id` - ID of the run to update
/// * `last_commit` - Optional commit hash to record
/// * `last_files` - Optional list of files modified
///
/// # Returns
/// * `Ok(UpdateResult)` - Information about the update
/// * `Err(TaskMgrError)` - If run not found or not active
pub fn update(
    conn: &Connection,
    run_id: &str,
    last_commit: Option<&str>,
    last_files: Option<&[String]>,
) -> TaskMgrResult<UpdateResult> {
    // Verify run exists and is active
    let (status_str, current_iteration): (String, i32) = conn
        .query_row(
            "SELECT status, iteration_count FROM runs WHERE run_id = ?",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::run_not_found(run_id),
            _ => TaskMgrError::from(e),
        })?;

    let status: RunStatus = status_str.parse()?;

    if !status.is_active() {
        return Err(TaskMgrError::invalid_state(
            "Run",
            run_id,
            "active",
            &status_str,
        ));
    }

    let mut commit_updated = false;
    let mut files_updated = false;

    // Update last_commit if provided
    if let Some(commit) = last_commit {
        conn.execute(
            "UPDATE runs SET last_commit = ? WHERE run_id = ?",
            rusqlite::params![commit, run_id],
        )?;
        commit_updated = true;
    }

    // Update last_files if provided
    if let Some(files) = last_files {
        let files_json = serde_json::to_string(files).map_err(TaskMgrError::JsonError)?;
        conn.execute(
            "UPDATE runs SET last_files = ? WHERE run_id = ?",
            rusqlite::params![files_json, run_id],
        )?;
        files_updated = true;
    }

    // Increment iteration_count
    conn.execute(
        "UPDATE runs SET iteration_count = iteration_count + 1 WHERE run_id = ?",
        [run_id],
    )?;

    Ok(UpdateResult {
        run_id: run_id.to_string(),
        commit_updated,
        files_updated,
        iteration_count: current_iteration + 1,
    })
}

/// End a run session.
///
/// # Arguments
/// * `conn` - Database connection
/// * `run_id` - ID of the run to end
/// * `status` - Final status (completed or aborted)
///
/// # Returns
/// * `Ok(EndResult)` - Information about the ended run
/// * `Err(TaskMgrError)` - If run not found or not active
pub fn end(conn: &Connection, run_id: &str, status: RunStatus) -> TaskMgrResult<EndResult> {
    // Validate that we're ending with a terminal status
    if status.is_active() {
        return Err(TaskMgrError::invalid_state(
            "RunStatus",
            run_id,
            "completed or aborted",
            "active",
        ));
    }

    // Verify run exists and is active
    let run = query_run(conn, run_id)?;

    if !run.status.is_active() {
        return Err(TaskMgrError::invalid_state(
            "Run",
            run_id,
            "active",
            run.status.to_string(),
        ));
    }

    // Update run status and ended_at
    conn.execute(
        "UPDATE runs SET status = ?, ended_at = datetime('now') WHERE run_id = ?",
        rusqlite::params![status.as_db_str(), run_id],
    )?;

    // Calculate duration by re-querying the run
    let duration_seconds = conn
        .query_row(
            "SELECT CAST((julianday(ended_at) - julianday(started_at)) * 86400 AS INTEGER) FROM runs WHERE run_id = ?",
            [run_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten();

    Ok(EndResult {
        run_id: run_id.to_string(),
        previous_status: run.status,
        new_status: status,
        duration_seconds,
    })
}

/// Row data from the runs table query.
struct RunRow {
    run_id: String,
    started_at: String,
    ended_at: Option<String>,
    status: String,
    last_commit: Option<String>,
    last_files: Option<String>,
    iteration_count: i32,
    notes: Option<String>,
}

/// Query a run by ID.
fn query_run(conn: &Connection, run_id: &str) -> TaskMgrResult<Run> {
    // Query the run data
    let row = conn
        .query_row(
            "SELECT run_id, started_at, ended_at, status, last_commit, last_files, iteration_count, notes
             FROM runs WHERE run_id = ?",
            [run_id],
            |row| Ok(RunRow {
                run_id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                status: row.get(3)?,
                last_commit: row.get(4)?,
                last_files: row.get(5)?,
                iteration_count: row.get(6)?,
                notes: row.get(7)?,
            }),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::run_not_found(run_id),
            _ => TaskMgrError::from(e),
        })?;

    // Parse status
    let status: RunStatus = row.status.parse()?;

    // Parse timestamps
    use crate::models::{parse_datetime, parse_optional_datetime};
    let started_at = parse_datetime(&row.started_at)?;
    let ended_at = parse_optional_datetime(row.ended_at)?;

    // Parse last_files from JSON
    let last_files: Vec<String> = match row.last_files {
        Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
        _ => Vec::new(),
    };

    Ok(Run {
        run_id: row.run_id,
        started_at,
        ended_at,
        status,
        last_commit: row.last_commit,
        last_files,
        iteration_count: row.iteration_count,
        notes: row.notes,
    })
}

/// Format begin result as human-readable text.
#[must_use]
pub fn format_begin_text(result: &BeginResult) -> String {
    format!(
        "Started new run: {}\nStatus: {}",
        result.run_id, result.status
    )
}

/// Format update result as human-readable text.
#[must_use]
pub fn format_update_text(result: &UpdateResult) -> String {
    let mut updates = Vec::new();
    if result.commit_updated {
        updates.push("commit");
    }
    if result.files_updated {
        updates.push("files");
    }

    let updates_str = if updates.is_empty() {
        "iteration count".to_string()
    } else {
        updates.join(", ")
    };

    format!(
        "Updated run {}\nFields updated: {}\nIteration: {}",
        result.run_id, updates_str, result.iteration_count
    )
}

/// Format end result as human-readable text.
#[must_use]
pub fn format_end_text(result: &EndResult) -> String {
    let duration_str = result
        .duration_seconds
        .map(|d| format!(" ({}s)", d))
        .unwrap_or_default();

    format!(
        "Ended run {}{}\nStatus: {} -> {}",
        result.run_id, duration_str, result.previous_status, result.new_status
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    // ============ begin() tests ============

    #[test]
    fn test_begin_creates_run() {
        let (_dir, conn) = setup_test_db();

        let result = begin(&conn).unwrap();

        assert_eq!(result.status, RunStatus::Active);
        assert!(!result.run_id.is_empty());

        // Verify run exists in database
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id = ?",
                [&result.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_begin_generates_uuid() {
        let (_dir, conn) = setup_test_db();

        let result1 = begin(&conn).unwrap();
        let result2 = begin(&conn).unwrap();

        // UUIDs should be different
        assert_ne!(result1.run_id, result2.run_id);

        // Should be valid UUIDs (36 chars with dashes)
        assert_eq!(result1.run_id.len(), 36);
        assert_eq!(result2.run_id.len(), 36);
    }

    #[test]
    fn test_begin_sets_started_at() {
        let (_dir, conn) = setup_test_db();

        let result = begin(&conn).unwrap();

        let started_at: String = conn
            .query_row(
                "SELECT started_at FROM runs WHERE run_id = ?",
                [&result.run_id],
                |row| row.get(0),
            )
            .unwrap();

        // Should have a timestamp
        assert!(!started_at.is_empty());
    }

    #[test]
    fn test_begin_initializes_iteration_count() {
        let (_dir, conn) = setup_test_db();

        let result = begin(&conn).unwrap();

        let iteration_count: i32 = conn
            .query_row(
                "SELECT iteration_count FROM runs WHERE run_id = ?",
                [&result.run_id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(iteration_count, 0);
    }

    // ============ update() tests ============

    #[test]
    fn test_update_nonexistent_run() {
        let (_dir, conn) = setup_test_db();

        let result = update(&conn, "nonexistent-run-id", None, None);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_update_inactive_run_fails() {
        let (_dir, conn) = setup_test_db();

        // Create a completed run
        conn.execute(
            "INSERT INTO runs (run_id, status, started_at, ended_at) VALUES ('run-123', 'completed', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let result = update(&conn, "run-123", Some("abc123"), None);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState { .. }) => {}
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_update_last_commit() {
        let (_dir, conn) = setup_test_db();

        let begin_result = begin(&conn).unwrap();
        let result = update(&conn, &begin_result.run_id, Some("abc123def"), None).unwrap();

        assert!(result.commit_updated);
        assert!(!result.files_updated);

        let last_commit: String = conn
            .query_row(
                "SELECT last_commit FROM runs WHERE run_id = ?",
                [&begin_result.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last_commit, "abc123def");
    }

    #[test]
    fn test_update_last_files() {
        let (_dir, conn) = setup_test_db();

        let begin_result = begin(&conn).unwrap();
        let files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
        let result = update(&conn, &begin_result.run_id, None, Some(&files)).unwrap();

        assert!(!result.commit_updated);
        assert!(result.files_updated);

        let last_files_json: String = conn
            .query_row(
                "SELECT last_files FROM runs WHERE run_id = ?",
                [&begin_result.run_id],
                |row| row.get(0),
            )
            .unwrap();

        let parsed: Vec<String> = serde_json::from_str(&last_files_json).unwrap();
        assert_eq!(parsed, files);
    }

    #[test]
    fn test_update_increments_iteration() {
        let (_dir, conn) = setup_test_db();

        let begin_result = begin(&conn).unwrap();

        let result1 = update(&conn, &begin_result.run_id, None, None).unwrap();
        assert_eq!(result1.iteration_count, 1);

        let result2 = update(&conn, &begin_result.run_id, None, None).unwrap();
        assert_eq!(result2.iteration_count, 2);

        let result3 = update(&conn, &begin_result.run_id, None, None).unwrap();
        assert_eq!(result3.iteration_count, 3);
    }

    #[test]
    fn test_update_with_all_options() {
        let (_dir, conn) = setup_test_db();

        let begin_result = begin(&conn).unwrap();
        let files = vec!["file1.rs".to_string()];
        let result = update(
            &conn,
            &begin_result.run_id,
            Some("commit-hash"),
            Some(&files),
        )
        .unwrap();

        assert!(result.commit_updated);
        assert!(result.files_updated);
        assert_eq!(result.iteration_count, 1);
    }

    // ============ end() tests ============

    #[test]
    fn test_end_nonexistent_run() {
        let (_dir, conn) = setup_test_db();

        let result = end(&conn, "nonexistent", RunStatus::Completed);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_end_already_ended_run_fails() {
        let (_dir, conn) = setup_test_db();

        conn.execute(
            "INSERT INTO runs (run_id, status, started_at, ended_at) VALUES ('run-done', 'completed', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let result = end(&conn, "run-done", RunStatus::Aborted);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState { .. }) => {}
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_end_with_active_status_fails() {
        let (_dir, conn) = setup_test_db();

        let begin_result = begin(&conn).unwrap();
        let result = end(&conn, &begin_result.run_id, RunStatus::Active);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState { .. }) => {}
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_end_completed() {
        let (_dir, conn) = setup_test_db();

        let begin_result = begin(&conn).unwrap();
        let result = end(&conn, &begin_result.run_id, RunStatus::Completed).unwrap();

        assert_eq!(result.run_id, begin_result.run_id);
        assert_eq!(result.previous_status, RunStatus::Active);
        assert_eq!(result.new_status, RunStatus::Completed);

        // Verify in database
        let status: String = conn
            .query_row(
                "SELECT status FROM runs WHERE run_id = ?",
                [&begin_result.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "completed");
    }

    #[test]
    fn test_end_aborted() {
        let (_dir, conn) = setup_test_db();

        let begin_result = begin(&conn).unwrap();
        let result = end(&conn, &begin_result.run_id, RunStatus::Aborted).unwrap();

        assert_eq!(result.new_status, RunStatus::Aborted);

        let status: String = conn
            .query_row(
                "SELECT status FROM runs WHERE run_id = ?",
                [&begin_result.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "aborted");
    }

    #[test]
    fn test_end_sets_ended_at() {
        let (_dir, conn) = setup_test_db();

        let begin_result = begin(&conn).unwrap();
        end(&conn, &begin_result.run_id, RunStatus::Completed).unwrap();

        let ended_at: Option<String> = conn
            .query_row(
                "SELECT ended_at FROM runs WHERE run_id = ?",
                [&begin_result.run_id],
                |row| row.get(0),
            )
            .unwrap();

        assert!(ended_at.is_some());
    }

    #[test]
    fn test_full_run_lifecycle() {
        let (_dir, conn) = setup_test_db();

        // Begin
        let begin_result = begin(&conn).unwrap();
        assert_eq!(begin_result.status, RunStatus::Active);

        // Update several times
        let files = vec!["src/main.rs".to_string()];
        update(&conn, &begin_result.run_id, Some("commit1"), Some(&files)).unwrap();
        update(&conn, &begin_result.run_id, Some("commit2"), None).unwrap();
        update(&conn, &begin_result.run_id, None, None).unwrap();

        // Verify iteration count
        let iteration: i32 = conn
            .query_row(
                "SELECT iteration_count FROM runs WHERE run_id = ?",
                [&begin_result.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(iteration, 3);

        // End
        let end_result = end(&conn, &begin_result.run_id, RunStatus::Completed).unwrap();
        assert_eq!(end_result.new_status, RunStatus::Completed);
        assert!(end_result.duration_seconds.is_some());
    }

    // ============ format_text tests ============

    #[test]
    fn test_format_begin_text() {
        let result = BeginResult {
            run_id: "abc-123".to_string(),
            status: RunStatus::Active,
        };

        let text = format_begin_text(&result);
        assert!(text.contains("Started new run: abc-123"));
        assert!(text.contains("Status: active"));
    }

    #[test]
    fn test_format_update_text_with_updates() {
        let result = UpdateResult {
            run_id: "run-456".to_string(),
            commit_updated: true,
            files_updated: true,
            iteration_count: 5,
        };

        let text = format_update_text(&result);
        assert!(text.contains("Updated run run-456"));
        assert!(text.contains("commit"));
        assert!(text.contains("files"));
        assert!(text.contains("Iteration: 5"));
    }

    #[test]
    fn test_format_update_text_no_updates() {
        let result = UpdateResult {
            run_id: "run-789".to_string(),
            commit_updated: false,
            files_updated: false,
            iteration_count: 1,
        };

        let text = format_update_text(&result);
        assert!(text.contains("iteration count"));
    }

    #[test]
    fn test_format_end_text() {
        let result = EndResult {
            run_id: "run-end".to_string(),
            previous_status: RunStatus::Active,
            new_status: RunStatus::Completed,
            duration_seconds: Some(300),
        };

        let text = format_end_text(&result);
        assert!(text.contains("Ended run run-end"));
        assert!(text.contains("(300s)"));
        assert!(text.contains("active -> completed"));
    }

    #[test]
    fn test_format_end_text_no_duration() {
        let result = EndResult {
            run_id: "run-no-dur".to_string(),
            previous_status: RunStatus::Active,
            new_status: RunStatus::Aborted,
            duration_seconds: None,
        };

        let text = format_end_text(&result);
        assert!(!text.contains("("));
        assert!(text.contains("aborted"));
    }
}
