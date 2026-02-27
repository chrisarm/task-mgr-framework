//! Status dashboard for PRD projects.
//!
//! Shows project completion, branch, deadline info, and optionally
//! lists pending tasks. Used by the `status` CLI command.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::Serialize;

use crate::db::open_connection;
use crate::db::prefix::{prefix_and, prefix_where};
use crate::TaskMgrResult;

use super::DEADLINE_FILE_PREFIX;

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
/// When a PRD file is provided, tasks are filtered to only those belonging
/// to that PRD (matched by the `taskPrefix` field in the PRD JSON).
///
/// # Arguments
///
/// * `dir` - Directory containing the database
/// * `prd_file` - Optional PRD file path (filters tasks by PRD's taskPrefix)
/// * `verbose` - If true, includes pending task listing
pub fn show_status(
    dir: &Path,
    prd_file: Option<&Path>,
    verbose: bool,
) -> TaskMgrResult<DashboardResult> {
    let conn = open_connection(dir)?;

    // Extract task prefix from PRD JSON for filtering
    let task_prefix = prd_file.and_then(read_task_prefix_from_prd);

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

    Ok(DashboardResult {
        project,
        tasks,
        completion_percentage,
        deadline,
        pending_tasks,
    })
}

/// Read the `taskPrefix` field from a PRD JSON file.
fn read_task_prefix_from_prd(prd_path: &Path) -> Option<String> {
    let content = fs::read_to_string(prd_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("taskPrefix")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Query project metadata from prd_metadata table.
///
/// When `task_prefix` is provided, queries `WHERE task_prefix = ?` to select the
/// matching PRD row. Falls back to `LIMIT 1 ORDER BY id ASC` when no prefix is given.
fn query_project_info(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<Option<ProjectInfo>> {
    let result = if let Some(prefix) = task_prefix {
        conn.query_row(
            "SELECT project, branch_name, description FROM prd_metadata WHERE task_prefix = ?1",
            rusqlite::params![prefix],
            |row| {
                Ok(ProjectInfo {
                    name: row.get(0)?,
                    branch: row.get(1)?,
                    description: row.get(2)?,
                })
            },
        )
    } else {
        conn.query_row(
            "SELECT project, branch_name, description FROM prd_metadata ORDER BY id ASC LIMIT 1",
            [],
            |row| {
                Ok(ProjectInfo {
                    name: row.get(0)?,
                    branch: row.get(1)?,
                    description: row.get(2)?,
                })
            },
        )
    };

    match result {
        Ok(info) => Ok(Some(info)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Query task counts grouped by status.
///
/// When `task_prefix` is provided, only counts tasks whose ID starts with `{prefix}-`.
fn query_dashboard_task_counts(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<DashboardTaskCounts> {
    let (where_clause, like_pattern) = prefix_where(task_prefix);

    let sql = format!(
        r#"
        SELECT
            COUNT(*) as total,
            COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0) as done,
            COALESCE(SUM(CASE WHEN status = 'todo' THEN 1 ELSE 0 END), 0) as todo,
            COALESCE(SUM(CASE WHEN status = 'in_progress' THEN 1 ELSE 0 END), 0) as in_progress,
            COALESCE(SUM(CASE WHEN status = 'blocked' THEN 1 ELSE 0 END), 0) as blocked,
            COALESCE(SUM(CASE WHEN status = 'skipped' THEN 1 ELSE 0 END), 0) as skipped,
            COALESCE(SUM(CASE WHEN status = 'irrelevant' THEN 1 ELSE 0 END), 0) as irrelevant
        FROM tasks
        {where_clause}
        "#,
    );

    let mut stmt = conn.prepare(&sql)?;
    let counts = if let Some(ref pattern) = like_pattern {
        stmt.query_row(rusqlite::params![pattern], |row| {
            Ok(DashboardTaskCounts {
                total: row.get(0)?,
                done: row.get(1)?,
                todo: row.get(2)?,
                in_progress: row.get(3)?,
                blocked: row.get(4)?,
                skipped: row.get(5)?,
                irrelevant: row.get(6)?,
            })
        })?
    } else {
        stmt.query_row([], |row| {
            Ok(DashboardTaskCounts {
                total: row.get(0)?,
                done: row.get(1)?,
                todo: row.get(2)?,
                in_progress: row.get(3)?,
                blocked: row.get(4)?,
                skipped: row.get(5)?,
                irrelevant: row.get(6)?,
            })
        })?
    };

    Ok(counts)
}

/// Query pending tasks (todo + in_progress + blocked) ordered by priority.
///
/// When `task_prefix` is provided, only returns tasks whose ID starts with `{prefix}-`.
fn query_pending_tasks(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<Vec<PendingTask>> {
    let (and_clause, like_pattern) = prefix_and(task_prefix);

    let sql = format!(
        r#"
        SELECT id, title, priority, status
        FROM tasks
        WHERE status IN ('todo', 'in_progress', 'blocked')
        {and_clause}
        ORDER BY priority ASC, id ASC
        "#,
    );

    let mut stmt = conn.prepare(&sql)?;
    let tasks = if let Some(ref pattern) = like_pattern {
        stmt.query_map(rusqlite::params![pattern], |row| {
            Ok(PendingTask {
                id: row.get(0)?,
                title: row.get(1)?,
                priority: row.get(2)?,
                status: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([], |row| {
            Ok(PendingTask {
                id: row.get(0)?,
                title: row.get(1)?,
                priority: row.get(2)?,
                status: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };

    Ok(tasks)
}


/// Read deadline info from .deadline-* files in the tasks directory.
///
/// If a specific PRD file is provided, reads only its deadline.
/// Otherwise, scans for any .deadline-* files.
fn read_deadline_info(dir: &Path, prd_file: Option<&Path>) -> Option<DeadlineInfo> {
    let tasks_dir = dir.join("tasks");

    if let Some(prd) = prd_file {
        let basename = prd_basename_from_path(prd);
        return read_single_deadline(&tasks_dir, &basename);
    }

    // Scan for any .deadline-* file
    let entries = fs::read_dir(&tasks_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(DEADLINE_FILE_PREFIX) {
            let basename = name_str
                .trim_start_matches(DEADLINE_FILE_PREFIX)
                .to_string();
            if let Some(info) = read_single_deadline(&tasks_dir, &basename) {
                return Some(info);
            }
        }
    }

    None
}

/// Read a single deadline file and compute remaining time.
fn read_single_deadline(tasks_dir: &Path, basename: &str) -> Option<DeadlineInfo> {
    let path = tasks_dir.join(format!("{}{}", DEADLINE_FILE_PREFIX, basename));
    let content = fs::read_to_string(&path).ok()?;
    let deadline_epoch: u64 = content.trim().parse().ok()?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs();

    let (expired, seconds_remaining) = if now >= deadline_epoch {
        (true, 0u64)
    } else {
        (false, deadline_epoch - now)
    };

    let time_remaining = format_remaining(seconds_remaining);

    Some(DeadlineInfo {
        prd_basename: basename.to_string(),
        expired,
        seconds_remaining,
        time_remaining,
    })
}

/// Derive PRD basename from file path (strip directory and .json extension).
fn prd_basename_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Format remaining seconds as human-readable string.
fn format_remaining(seconds: u64) -> String {
    if seconds == 0 {
        return "expired".to_string();
    }

    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;

    if hours > 0 {
        format!("{}h {}m remaining", hours, minutes)
    } else if minutes > 0 {
        format!("{}m remaining", minutes)
    } else {
        format!("{}s remaining", seconds)
    }
}

/// Format the dashboard result as human-readable text.
pub fn format_text(result: &DashboardResult) -> String {
    let mut output = String::new();

    // Status icon based on completion
    let icon = status_icon(result.completion_percentage);

    output.push_str("=== Status Dashboard ===\n");

    // Project info
    if let Some(ref project) = result.project {
        output.push_str(&format!("{} Project: {}\n", icon, project.name));
        if let Some(ref branch) = project.branch {
            output.push_str(&format!("  Branch:  {}\n", branch));
        }
    } else {
        output.push_str(&format!("{} No project initialized\n", icon));
    }

    // Completion bar
    output.push('\n');
    output.push_str(&format!(
        "Progress: {}/{} tasks ({:.1}%)\n",
        result.tasks.done, result.tasks.total, result.completion_percentage
    ));
    output.push_str(&format!(
        "  {}\n",
        progress_bar(result.completion_percentage)
    ));

    // Status breakdown
    output.push('\n');
    output.push_str("Status:\n");
    output.push_str(&format!("  done:        {:>4}\n", result.tasks.done));
    output.push_str(&format!("  todo:        {:>4}\n", result.tasks.todo));
    output.push_str(&format!("  in_progress: {:>4}\n", result.tasks.in_progress));
    output.push_str(&format!("  blocked:     {:>4}\n", result.tasks.blocked));
    output.push_str(&format!("  skipped:     {:>4}\n", result.tasks.skipped));
    output.push_str(&format!("  irrelevant:  {:>4}\n", result.tasks.irrelevant));

    // Deadline
    if let Some(ref deadline) = result.deadline {
        output.push('\n');
        if deadline.expired {
            output.push_str(&format!("Deadline: EXPIRED ({})\n", deadline.prd_basename));
        } else {
            output.push_str(&format!(
                "Deadline: {} ({})\n",
                deadline.time_remaining, deadline.prd_basename
            ));
        }
    }

    // Verbose: pending tasks
    if !result.pending_tasks.is_empty() {
        output.push('\n');
        output.push_str("Pending tasks:\n");
        for task in &result.pending_tasks {
            let status_indicator = match task.status.as_str() {
                "in_progress" => ">",
                "blocked" => "!",
                _ => " ",
            };
            output.push_str(&format!(
                "  {} [P{}] {} - {}\n",
                status_indicator, task.priority, task.id, task.title
            ));
        }
    }

    output
}

/// Generate a progress bar string.
fn progress_bar(percentage: f64) -> String {
    let filled = (percentage / 5.0).round() as usize; // 20 chars wide
    let empty = 20_usize.saturating_sub(filled);
    format!(
        "[{}{}] {:.1}%",
        "#".repeat(filled),
        "-".repeat(empty),
        percentage
    )
}

/// Return a status icon based on completion percentage.
fn status_icon(percentage: f64) -> &'static str {
    if percentage >= 100.0 {
        "[DONE]"
    } else if percentage > 0.0 {
        "[....]"
    } else {
        "[    ]"
    }
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

    fn insert_prd_metadata(
        conn: &Connection,
        project: &str,
        branch: Option<&str>,
        desc: Option<&str>,
    ) {
        conn.execute(
            r#"INSERT OR REPLACE INTO prd_metadata (id, project, branch_name, description)
               VALUES (1, ?, ?, ?)"#,
            params![project, branch, desc],
        )
        .unwrap();
    }

    fn insert_task(conn: &Connection, id: &str, title: &str, status: &str, priority: i64) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
            params![id, title, status, priority],
        )
        .unwrap();
    }

    // --- show_status tests ---

    #[test]
    fn test_show_status_empty_db() {
        let (temp_dir, conn) = setup_test_db();
        drop(conn);

        let result = show_status(temp_dir.path(), None, false).unwrap();
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

        let result = show_status(temp_dir.path(), None, false).unwrap();
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

        let result = show_status(temp_dir.path(), None, false).unwrap();
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

        let result = show_status(temp_dir.path(), None, false).unwrap();
        assert_eq!(result.completion_percentage, 50.0);
    }

    #[test]
    fn test_show_status_100_percent() {
        let (temp_dir, conn) = setup_test_db();
        insert_task(&conn, "T-001", "Task 1", "done", 10);
        insert_task(&conn, "T-002", "Task 2", "done", 20);
        drop(conn);

        let result = show_status(temp_dir.path(), None, false).unwrap();
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

        let result = show_status(temp_dir.path(), None, true).unwrap();
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

        let result = show_status(temp_dir.path(), None, false).unwrap();
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

        let result = show_status(temp_dir.path(), None, false).unwrap();
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

        let result = show_status(temp_dir.path(), None, false).unwrap();
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
        let result = show_status(temp_dir.path(), Some(prd_path), false).unwrap();
        let deadline = result.deadline.unwrap();
        assert_eq!(deadline.prd_basename, "prd-a");
    }

    #[test]
    fn test_show_status_no_deadline_file() {
        let (temp_dir, _conn) = setup_test_db();
        let result = show_status(temp_dir.path(), None, false).unwrap();
        assert!(result.deadline.is_none());
    }

    // --- format_text tests ---

    #[test]
    fn test_format_text_with_data() {
        let result = DashboardResult {
            project: Some(ProjectInfo {
                name: "my-project".to_string(),
                branch: Some("feat/cool".to_string()),
                description: Some("A description".to_string()),
            }),
            tasks: DashboardTaskCounts {
                total: 10,
                done: 5,
                todo: 3,
                in_progress: 1,
                blocked: 1,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 50.0,
            deadline: None,
            pending_tasks: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("my-project"));
        assert!(text.contains("feat/cool"));
        assert!(text.contains("5/10 tasks"));
        assert!(text.contains("50.0%"));
        assert!(text.contains("done:"));
        assert!(text.contains("todo:"));
    }

    #[test]
    fn test_format_text_no_project() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 0,
                done: 0,
                todo: 0,
                in_progress: 0,
                blocked: 0,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            deadline: None,
            pending_tasks: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("No project initialized"));
    }

    #[test]
    fn test_format_text_with_deadline() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 0,
                done: 0,
                todo: 0,
                in_progress: 0,
                blocked: 0,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            deadline: Some(DeadlineInfo {
                prd_basename: "test-prd".to_string(),
                expired: false,
                seconds_remaining: 3600,
                time_remaining: "1h 0m remaining".to_string(),
            }),
            pending_tasks: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("1h 0m remaining"));
        assert!(text.contains("test-prd"));
    }

    #[test]
    fn test_format_text_with_expired_deadline() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 0,
                done: 0,
                todo: 0,
                in_progress: 0,
                blocked: 0,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            deadline: Some(DeadlineInfo {
                prd_basename: "test-prd".to_string(),
                expired: true,
                seconds_remaining: 0,
                time_remaining: "expired".to_string(),
            }),
            pending_tasks: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("EXPIRED"));
    }

    #[test]
    fn test_format_text_with_pending_tasks() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 3,
                done: 0,
                todo: 1,
                in_progress: 1,
                blocked: 1,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            deadline: None,
            pending_tasks: vec![
                PendingTask {
                    id: "T-001".to_string(),
                    title: "In progress task".to_string(),
                    priority: 10,
                    status: "in_progress".to_string(),
                },
                PendingTask {
                    id: "T-002".to_string(),
                    title: "Todo task".to_string(),
                    priority: 20,
                    status: "todo".to_string(),
                },
                PendingTask {
                    id: "T-003".to_string(),
                    title: "Blocked task".to_string(),
                    priority: 30,
                    status: "blocked".to_string(),
                },
            ],
        };

        let text = format_text(&result);
        assert!(text.contains("Pending tasks:"));
        assert!(text.contains("> [P10] T-001"));
        assert!(text.contains("  [P20] T-002"));
        assert!(text.contains("! [P30] T-003"));
    }

    // --- helper function tests ---

    #[test]
    fn test_progress_bar_0_percent() {
        let bar = progress_bar(0.0);
        assert_eq!(bar, "[--------------------] 0.0%");
    }

    #[test]
    fn test_progress_bar_50_percent() {
        let bar = progress_bar(50.0);
        assert_eq!(bar, "[##########----------] 50.0%");
    }

    #[test]
    fn test_progress_bar_100_percent() {
        let bar = progress_bar(100.0);
        assert_eq!(bar, "[####################] 100.0%");
    }

    #[test]
    fn test_format_remaining_expired() {
        assert_eq!(format_remaining(0), "expired");
    }

    #[test]
    fn test_format_remaining_seconds() {
        assert_eq!(format_remaining(30), "30s remaining");
    }

    #[test]
    fn test_format_remaining_minutes() {
        assert_eq!(format_remaining(300), "5m remaining");
    }

    #[test]
    fn test_format_remaining_hours_and_minutes() {
        assert_eq!(format_remaining(5400), "1h 30m remaining");
    }

    #[test]
    fn test_prd_basename_from_path() {
        assert_eq!(
            prd_basename_from_path(Path::new("tasks/my-prd.json")),
            "my-prd"
        );
        assert_eq!(
            prd_basename_from_path(Path::new("/abs/path/task-mgr-final.json")),
            "task-mgr-final"
        );
    }

    #[test]
    fn test_prd_basename_no_extension() {
        assert_eq!(prd_basename_from_path(Path::new("tasks/my-prd")), "my-prd");
    }

    #[test]
    fn test_status_icon_complete() {
        assert_eq!(status_icon(100.0), "[DONE]");
    }

    #[test]
    fn test_status_icon_partial() {
        assert_eq!(status_icon(50.0), "[....]");
        assert_eq!(status_icon(25.0), "[....]");
    }

    #[test]
    fn test_status_icon_empty() {
        assert_eq!(status_icon(0.0), "[    ]");
    }

    // --- query tests ---

    #[test]
    fn test_query_project_info_no_metadata() {
        let (_temp_dir, conn) = setup_test_db();
        let result = query_project_info(&conn, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_query_project_info_with_metadata() {
        let (_temp_dir, conn) = setup_test_db();
        insert_prd_metadata(&conn, "test-proj", Some("develop"), None);
        let result = query_project_info(&conn, None).unwrap();
        let info = result.unwrap();
        assert_eq!(info.name, "test-proj");
        assert_eq!(info.branch.unwrap(), "develop");
        assert!(info.description.is_none());
    }

    #[test]
    fn test_query_dashboard_task_counts_empty() {
        let (_temp_dir, conn) = setup_test_db();
        let counts = query_dashboard_task_counts(&conn, None).unwrap();
        assert_eq!(counts.total, 0);
        assert_eq!(counts.done, 0);
    }

    #[test]
    fn test_query_pending_tasks_ordering() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "B-002", "Task B", "todo", 20);
        insert_task(&conn, "A-001", "Task A", "todo", 10);
        insert_task(&conn, "C-003", "Task C", "in_progress", 10);

        let tasks = query_pending_tasks(&conn, None).unwrap();
        assert_eq!(tasks.len(), 3);
        // Priority 10 first, then by ID
        assert_eq!(tasks[0].id, "A-001");
        assert_eq!(tasks[1].id, "C-003");
        assert_eq!(tasks[2].id, "B-002");
    }

    #[test]
    fn test_query_pending_tasks_excludes_done_skipped_irrelevant() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "T-001", "Done", "done", 10);
        insert_task(&conn, "T-002", "Skipped", "skipped", 20);
        insert_task(&conn, "T-003", "Irrelevant", "irrelevant", 30);
        insert_task(&conn, "T-004", "Todo", "todo", 40);

        let tasks = query_pending_tasks(&conn, None).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "T-004");
    }

    // --- prefix filtering tests ---

    #[test]
    fn test_query_task_counts_with_prefix_filter() {
        let (_temp_dir, conn) = setup_test_db();
        // Phase A tasks
        insert_task(&conn, "abc123-FEAT-001", "A feat 1", "done", 10);
        insert_task(&conn, "abc123-FEAT-002", "A feat 2", "todo", 20);
        // Phase B tasks
        insert_task(&conn, "def456-FEAT-001", "B feat 1", "done", 10);
        insert_task(&conn, "def456-FEAT-002", "B feat 2", "done", 20);
        insert_task(&conn, "def456-FEAT-003", "B feat 3", "todo", 30);

        // No filter: all 5
        let all = query_dashboard_task_counts(&conn, None).unwrap();
        assert_eq!(all.total, 5);
        assert_eq!(all.done, 3);

        // Filter to phase A: 2 tasks
        let a = query_dashboard_task_counts(&conn, Some("abc123")).unwrap();
        assert_eq!(a.total, 2);
        assert_eq!(a.done, 1);
        assert_eq!(a.todo, 1);

        // Filter to phase B: 3 tasks
        let b = query_dashboard_task_counts(&conn, Some("def456")).unwrap();
        assert_eq!(b.total, 3);
        assert_eq!(b.done, 2);
        assert_eq!(b.todo, 1);
    }

    #[test]
    fn test_query_pending_tasks_with_prefix_filter() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "abc123-FEAT-001", "A todo", "todo", 10);
        insert_task(&conn, "def456-FEAT-001", "B todo", "todo", 10);
        insert_task(&conn, "def456-FEAT-002", "B blocked", "blocked", 20);

        // No filter: all 3 pending
        let all = query_pending_tasks(&conn, None).unwrap();
        assert_eq!(all.len(), 3);

        // Filter to abc123: 1 pending
        let a = query_pending_tasks(&conn, Some("abc123")).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].id, "abc123-FEAT-001");

        // Filter to def456: 2 pending
        let b = query_pending_tasks(&conn, Some("def456")).unwrap();
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn test_prefix_filter_nonexistent_returns_zero() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "abc123-FEAT-001", "A feat", "done", 10);

        let counts = query_dashboard_task_counts(&conn, Some("zzz999")).unwrap();
        assert_eq!(counts.total, 0);
    }
}
