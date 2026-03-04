//! Status dashboard for PRD projects.
//!
//! Shows project completion, branch, deadline info, and optionally
//! lists pending tasks. Used by the `status` CLI command.

use std::path::Path;

use serde::Serialize;

use crate::db::open_connection;
use crate::TaskMgrResult;

use super::status_queries::{
    query_dashboard_task_counts, query_distinct_prefixes, query_pending_tasks, query_project_info,
    read_active_lock_prefixes, read_deadline_info, read_task_prefix_from_prd as _read_task_prefix,
};

// Re-export public API used by external callers.
pub use super::status_display::format_text;
pub use super::status_queries::read_task_prefix_from_prd;

/// Result of the status dashboard command.
#[derive(Debug, Serialize)]
pub struct DashboardResult {
    /// Project info from prd_metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectInfo>,
    /// Task counts by status
    pub tasks: DashboardTaskCounts,
    /// Completion percentage (done / total * 100)
    pub completion_percentage: f64,
    /// Deadline info if a deadline file exists
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DeadlineInfo>,
    /// Pending tasks (only populated in verbose mode)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pending_tasks: Vec<PendingTask>,
    /// Per-PRD summaries (populated when multiple PRDs exist and no prefix filter)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub prd_summaries: Vec<PrdSummary>,
}

/// Summary row for a single PRD in multi-PRD view.
#[derive(Debug, Serialize)]
pub struct PrdSummary {
    /// Task ID prefix identifying this PRD
    pub prefix: String,
    /// Total task count for this PRD
    pub total: i64,
    /// Completed task count
    pub done: i64,
    /// In-progress task count
    pub in_progress: i64,
    /// Completion percentage
    pub completion_pct: f64,
    /// Whether this PRD has an active loop lock
    pub active_lock: bool,
}

/// Project metadata from prd_metadata table.
#[derive(Debug, Serialize)]
pub struct ProjectInfo {
    /// Project name
    pub name: String,
    /// Branch name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Project description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Task counts for the dashboard.
#[derive(Debug, Serialize)]
pub struct DashboardTaskCounts {
    pub total: i64,
    pub done: i64,
    pub todo: i64,
    pub in_progress: i64,
    pub blocked: i64,
    pub skipped: i64,
    pub irrelevant: i64,
}

/// Deadline information.
#[derive(Debug, Serialize)]
pub struct DeadlineInfo {
    /// Basename of the PRD the deadline applies to
    pub prd_basename: String,
    /// Whether the deadline has passed
    pub expired: bool,
    /// Seconds remaining (0 if expired)
    pub seconds_remaining: u64,
    /// Human-readable time remaining
    pub time_remaining: String,
}

/// A pending task summary for verbose output.
#[derive(Debug, Serialize)]
pub struct PendingTask {
    /// Task ID
    pub id: String,
    /// Task title
    pub title: String,
    /// Task priority
    pub priority: i64,
    /// Task status
    pub status: String,
}

/// Show the status dashboard.
///
/// Queries the database for project metadata, task counts, and optionally
/// lists pending tasks (in verbose mode). Also reads .deadline-* files
/// to show deadline information.
///
/// When a PRD file or prefix is provided, tasks are filtered to only those
/// belonging to that PRD. When neither is provided and multiple PRDs exist,
/// a per-PRD summary row is shown for each.
///
/// # Arguments
///
/// * `dir` - Directory containing the database
/// * `prd_file` - Optional PRD file path (filters tasks by PRD's taskPrefix)
/// * `verbose` - If true, includes pending task listing
/// * `prefix` - Optional task ID prefix filter (overrides prd_file-derived prefix)
pub fn show_status(
    dir: &Path,
    prd_file: Option<&Path>,
    verbose: bool,
    prefix: Option<&str>,
) -> TaskMgrResult<DashboardResult> {
    let conn = open_connection(dir)?;

    // Resolve prefix: explicit flag > prd_file-derived > none
    let task_prefix = prefix
        .map(|s| s.to_string())
        .or_else(|| prd_file.and_then(_read_task_prefix));

    let project = query_project_info(&conn, task_prefix.as_deref())?;
    let tasks = query_dashboard_task_counts(&conn, task_prefix.as_deref())?;

    let completion_percentage = if tasks.total > 0 {
        (tasks.done as f64 / tasks.total as f64) * 100.0
    } else {
        0.0
    };

    let deadline = read_deadline_info(dir, prd_file);

    let pending_tasks = if verbose {
        query_pending_tasks(&conn, task_prefix.as_deref())?
    } else {
        Vec::new()
    };

    // Populate per-PRD summaries only when no prefix filter is active
    let prd_summaries = if task_prefix.is_none() {
        let prefixes = query_distinct_prefixes(&conn)?;
        if prefixes.len() >= 2 {
            let active_lock_prefixes = read_active_lock_prefixes(dir);
            prefixes
                .into_iter()
                .map(|p| {
                    let counts = query_dashboard_task_counts(&conn, Some(&p))?;
                    let pct = if counts.total > 0 {
                        (counts.done as f64 / counts.total as f64) * 100.0
                    } else {
                        0.0
                    };
                    let active_lock = active_lock_prefixes.iter().any(|lp| lp == &p);
                    Ok(PrdSummary {
                        prefix: p,
                        total: counts.total,
                        done: counts.done,
                        in_progress: counts.in_progress,
                        completion_pct: pct,
                        active_lock,
                    })
                })
                .collect::<TaskMgrResult<Vec<_>>>()?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    Ok(DashboardResult {
        project,
        tasks,
        completion_percentage,
        deadline,
        pending_tasks,
        prd_summaries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::test_utils::{insert_prd_metadata, insert_task, setup_test_db};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    // --- show_status tests ---

    #[test]
    fn test_show_status_empty_db() {
        let (temp_dir, conn) = setup_test_db();
        drop(conn);

        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        assert!(result.project.is_none());
        assert_eq!(result.tasks.total, 0);
        assert_eq!(result.completion_percentage, 0.0);
        assert!(result.deadline.is_none());
        assert!(result.pending_tasks.is_empty());
    }

    #[test]
    fn test_show_status_with_project() {
        let (temp_dir, conn) = setup_test_db();
        insert_prd_metadata(&conn, "my-project", Some("main"), Some("A test project"));
        drop(conn);

        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        let project = result.project.unwrap();
        assert_eq!(project.name, "my-project");
        assert_eq!(project.branch.unwrap(), "main");
        assert_eq!(project.description.unwrap(), "A test project");
    }

    #[test]
    fn test_show_status_task_counts() {
        let (temp_dir, conn) = setup_test_db();
        insert_task(&conn, "T-001", "Task 1", "done", 10);
        insert_task(&conn, "T-002", "Task 2", "done", 20);
        insert_task(&conn, "T-003", "Task 3", "todo", 30);
        insert_task(&conn, "T-004", "Task 4", "in_progress", 40);
        insert_task(&conn, "T-005", "Task 5", "blocked", 50);
        drop(conn);

        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        assert_eq!(result.tasks.total, 5);
        assert_eq!(result.tasks.done, 2);
        assert_eq!(result.tasks.todo, 1);
        assert_eq!(result.tasks.in_progress, 1);
        assert_eq!(result.tasks.blocked, 1);
        assert_eq!(result.tasks.skipped, 0);
        assert_eq!(result.tasks.irrelevant, 0);
    }

    #[test]
    fn test_show_status_completion_percentage() {
        let (temp_dir, conn) = setup_test_db();
        insert_task(&conn, "T-001", "Task 1", "done", 10);
        insert_task(&conn, "T-002", "Task 2", "done", 20);
        insert_task(&conn, "T-003", "Task 3", "todo", 30);
        insert_task(&conn, "T-004", "Task 4", "todo", 40);
        drop(conn);

        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        assert_eq!(result.completion_percentage, 50.0);
    }

    #[test]
    fn test_show_status_100_percent() {
        let (temp_dir, conn) = setup_test_db();
        insert_task(&conn, "T-001", "Task 1", "done", 10);
        insert_task(&conn, "T-002", "Task 2", "done", 20);
        drop(conn);

        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        assert_eq!(result.completion_percentage, 100.0);
    }

    #[test]
    fn test_show_status_verbose_lists_pending() {
        let (temp_dir, conn) = setup_test_db();
        insert_task(&conn, "T-001", "Done Task", "done", 10);
        insert_task(&conn, "T-002", "Todo Task", "todo", 20);
        insert_task(&conn, "T-003", "In Progress", "in_progress", 15);
        insert_task(&conn, "T-004", "Blocked Task", "blocked", 30);
        insert_task(&conn, "T-005", "Skipped Task", "skipped", 40);
        insert_task(&conn, "T-006", "Irrelevant", "irrelevant", 50);
        drop(conn);

        let result = show_status(temp_dir.path(), None, true, None).unwrap();
        // Pending = todo + in_progress + blocked
        assert_eq!(result.pending_tasks.len(), 3);
        // Ordered by priority ASC
        assert_eq!(result.pending_tasks[0].id, "T-003");
        assert_eq!(result.pending_tasks[0].status, "in_progress");
        assert_eq!(result.pending_tasks[1].id, "T-002");
        assert_eq!(result.pending_tasks[1].status, "todo");
        assert_eq!(result.pending_tasks[2].id, "T-004");
        assert_eq!(result.pending_tasks[2].status, "blocked");
    }

    #[test]
    fn test_show_status_non_verbose_no_pending() {
        let (temp_dir, conn) = setup_test_db();
        insert_task(&conn, "T-001", "Todo Task", "todo", 10);
        drop(conn);

        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        assert!(result.pending_tasks.is_empty());
    }

    #[test]
    fn test_show_status_with_deadline() {
        let (temp_dir, _conn) = setup_test_db();
        let tasks_dir = temp_dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();

        // Create a deadline file 1 hour in the future
        let future_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        fs::write(tasks_dir.join(".deadline-my-prd"), future_epoch.to_string()).unwrap();

        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        let deadline = result.deadline.unwrap();
        assert_eq!(deadline.prd_basename, "my-prd");
        assert!(!deadline.expired);
        assert!(deadline.seconds_remaining > 3500);
        assert!(deadline.time_remaining.contains("remaining"));
    }

    #[test]
    fn test_show_status_with_expired_deadline() {
        let (temp_dir, _conn) = setup_test_db();
        let tasks_dir = temp_dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();

        // Create a deadline file in the past
        let past_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 10;
        fs::write(tasks_dir.join(".deadline-test"), past_epoch.to_string()).unwrap();

        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        let deadline = result.deadline.unwrap();
        assert!(deadline.expired);
        assert_eq!(deadline.seconds_remaining, 0);
        assert_eq!(deadline.time_remaining, "expired");
    }

    #[test]
    fn test_show_status_with_prd_file_deadline() {
        let (temp_dir, _conn) = setup_test_db();
        let tasks_dir = temp_dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();

        // Create two deadline files
        let future_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        fs::write(tasks_dir.join(".deadline-prd-a"), future_epoch.to_string()).unwrap();
        fs::write(tasks_dir.join(".deadline-prd-b"), future_epoch.to_string()).unwrap();

        // When prd_file is specified, only read that deadline
        let prd_path = Path::new("tasks/prd-a.json");
        let result = show_status(temp_dir.path(), Some(prd_path), false, None).unwrap();
        let deadline = result.deadline.unwrap();
        assert_eq!(deadline.prd_basename, "prd-a");
    }

    #[test]
    fn test_show_status_no_deadline_file() {
        let (temp_dir, _conn) = setup_test_db();
        let result = show_status(temp_dir.path(), None, false, None).unwrap();
        assert!(result.deadline.is_none());
    }

    // --- multi-PRD summary tests ---

    #[test]
    fn test_show_status_single_prefix_no_prd_summaries() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "abc123-FEAT-001", "A feat", "done", 10);
        insert_task(&conn, "abc123-FEAT-002", "A feat 2", "todo", 20);
        drop(conn);

        let result = show_status(_temp_dir.path(), None, false, None).unwrap();
        // Only one prefix → no prd_summaries
        assert!(result.prd_summaries.is_empty());
    }

    #[test]
    fn test_show_status_multiple_prefixes_populates_prd_summaries() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "abc123-FEAT-001", "A feat", "done", 10);
        insert_task(&conn, "abc123-FEAT-002", "A feat 2", "todo", 20);
        insert_task(&conn, "def456-FEAT-001", "B feat", "done", 10);
        insert_task(&conn, "def456-FEAT-002", "B feat 2", "in_progress", 20);
        drop(conn);

        let result = show_status(_temp_dir.path(), None, false, None).unwrap();
        assert_eq!(result.prd_summaries.len(), 2);
        let a = result
            .prd_summaries
            .iter()
            .find(|s| s.prefix == "abc123")
            .unwrap();
        assert_eq!(a.total, 2);
        assert_eq!(a.done, 1);
        assert_eq!(a.in_progress, 0);
        let b = result
            .prd_summaries
            .iter()
            .find(|s| s.prefix == "def456")
            .unwrap();
        assert_eq!(b.total, 2);
        assert_eq!(b.done, 1);
        assert_eq!(b.in_progress, 1);
    }

    #[test]
    fn test_show_status_prefix_filter_suppresses_prd_summaries() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "abc123-FEAT-001", "A feat", "done", 10);
        insert_task(&conn, "def456-FEAT-001", "B feat", "todo", 10);
        drop(conn);

        // Prefix filter → no prd_summaries, counts scoped to prefix
        let result = show_status(_temp_dir.path(), None, false, Some("abc123")).unwrap();
        assert!(result.prd_summaries.is_empty());
        assert_eq!(result.tasks.total, 1);
        assert_eq!(result.tasks.done, 1);
    }
}
