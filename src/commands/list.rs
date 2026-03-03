//! List tasks with optional filtering.
//!
//! This module implements the `list` command which displays tasks
//! with optional filtering by status, file pattern, and task type prefix.

use rusqlite::Connection;
use serde::Serialize;

use crate::cli::TaskStatusFilter;
use crate::db::open_connection;
use crate::models::Task;
use crate::TaskMgrResult;

/// Result of the list command.
#[derive(Debug, Serialize)]
pub struct ListResult {
    /// The tasks matching the filter criteria
    pub tasks: Vec<TaskSummary>,
    /// Total number of tasks matching the filter
    pub count: usize,
    /// Filter criteria used
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_task_type: Option<String>,
}

/// Summary view of a task for list display.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSummary {
    /// Task identifier
    pub id: String,
    /// Task title
    pub title: String,
    /// Current status
    pub status: String,
    /// Priority (lower = higher priority)
    pub priority: i32,
}

impl From<&Task> for TaskSummary {
    fn from(task: &Task) -> Self {
        TaskSummary {
            id: task.id.clone(),
            title: task.title.clone(),
            status: task.status.to_string(),
            priority: task.priority,
        }
    }
}

/// List tasks with optional filtering.
///
/// # Arguments
///
/// * `dir` - Directory containing the database
/// * `status` - Optional status filter
/// * `file` - Optional file pattern filter (glob matching against touchesFiles)
/// * `task_type` - Optional task type prefix filter (e.g., "US-", "FIX-")
///
/// # Returns
///
/// Returns a `ListResult` with matching tasks.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or queried.
pub fn list(
    dir: &std::path::Path,
    status: Option<TaskStatusFilter>,
    file: Option<&str>,
    task_type: Option<&str>,
) -> TaskMgrResult<ListResult> {
    let conn = open_connection(dir)?;

    let tasks = query_tasks(&conn, status, file, task_type)?;
    let count = tasks.len();
    let summaries: Vec<TaskSummary> = tasks.iter().map(TaskSummary::from).collect();

    Ok(ListResult {
        tasks: summaries,
        count,
        filter_status: status.map(|s| s.to_string()),
        filter_file: file.map(String::from),
        filter_task_type: task_type.map(String::from),
    })
}

/// Query tasks from database with optional filtering.
fn query_tasks(
    conn: &Connection,
    status: Option<TaskStatusFilter>,
    file: Option<&str>,
    task_type: Option<&str>,
) -> TaskMgrResult<Vec<Task>> {
    // Build the query based on filters
    let mut conditions = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(status_filter) = status {
        conditions.push("t.status = ?");
        params.push(Box::new(status_filter.to_string()));
    }

    if let Some(task_type_filter) = task_type {
        conditions.push("t.id LIKE ?");
        params.push(Box::new(format!("{}%", task_type_filter)));
    }

    // Build the base query
    let base_query = if file.is_some() {
        // Join with task_files for file pattern matching
        "SELECT DISTINCT t.id, t.title, t.description, t.priority, t.status, t.notes, \
         t.acceptance_criteria, t.review_scope, t.severity, t.source_review, \
         t.created_at, t.updated_at, t.started_at, t.completed_at, \
         t.last_error, t.error_count \
         FROM tasks t \
         INNER JOIN task_files tf ON t.id = tf.task_id"
    } else {
        "SELECT t.id, t.title, t.description, t.priority, t.status, t.notes, \
         t.acceptance_criteria, t.review_scope, t.severity, t.source_review, \
         t.created_at, t.updated_at, t.started_at, t.completed_at, \
         t.last_error, t.error_count \
         FROM tasks t"
    };

    // Add file pattern filter if specified
    if let Some(file_pattern) = file {
        // Convert glob pattern to SQLite GLOB pattern
        conditions.push("tf.file_path GLOB ?");
        params.push(Box::new(file_pattern.to_string()));
    }

    // Construct full query
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    let query = format!(
        "{}{} ORDER BY t.priority ASC, t.id ASC",
        base_query, where_clause
    );

    // Execute query
    let mut stmt = conn.prepare(&query)?;

    // Create parameter references for rusqlite
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let tasks: Vec<Task> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Task::try_from(row).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(tasks)
}

/// Extract the task ID prefix (first segment before `-`).
fn extract_task_prefix(id: &str) -> &str {
    id.find('-').map_or(id, |i| &id[..i])
}

/// Format list result as human-readable text.
///
/// When tasks from multiple distinct prefixes are present, groups them
/// under section headers. When a single prefix exists, renders a flat list.
pub fn format_text(result: &ListResult) -> String {
    if result.tasks.is_empty() {
        return "No tasks found matching the filter criteria.".to_string();
    }

    // Collect distinct prefixes in order of first appearance
    let mut seen = std::collections::HashSet::new();
    let mut prefixes: Vec<&str> = Vec::new();
    for task in &result.tasks {
        let p = extract_task_prefix(&task.id);
        if seen.insert(p) {
            prefixes.push(p);
        }
    }

    let multi_prefix = prefixes.len() >= 2;
    let mut output = String::new();

    let render_task_row = |task: &TaskSummary, out: &mut String| {
        let title_display = super::truncate_str(&task.title, 37);
        out.push_str(&format!(
            "{:<12} {:<12} {:>5}  {}\n",
            task.id, task.status, task.priority, title_display
        ));
    };

    if multi_prefix {
        for prefix in &prefixes {
            output.push_str(&format!("=== {} ===\n", prefix));
            output.push_str(&format!(
                "{:<12} {:<12} {:>5}  {}\n",
                "ID", "STATUS", "PRI", "TITLE"
            ));
            output.push_str(&format!("{}\n", "-".repeat(70)));
            for task in result
                .tasks
                .iter()
                .filter(|t| extract_task_prefix(&t.id) == *prefix)
            {
                render_task_row(task, &mut output);
            }
            output.push('\n');
        }
    } else {
        output.push_str(&format!(
            "{:<12} {:<12} {:>5}  {}\n",
            "ID", "STATUS", "PRI", "TITLE"
        ));
        output.push_str(&format!("{}\n", "-".repeat(70)));
        for task in &result.tasks {
            render_task_row(task, &mut output);
        }
    }

    // Footer
    output.push_str(&format!("\nTotal: {} task(s)", result.count));

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

    fn insert_test_task(conn: &Connection, id: &str, title: &str, status: &str, priority: i32) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
            params![id, title, status, priority],
        )
        .unwrap();
    }

    fn insert_test_task_file(conn: &Connection, task_id: &str, file_path: &str) {
        conn.execute(
            "INSERT INTO task_files (task_id, file_path) VALUES (?, ?)",
            params![task_id, file_path],
        )
        .unwrap();
    }

    #[test]
    fn test_list_all_tasks() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        insert_test_task(&conn, "US-002", "Task 2", "done", 20);
        insert_test_task(&conn, "FIX-001", "Fix 1", "in_progress", 5);
        drop(conn);

        let result = list(temp_dir.path(), None, None, None).unwrap();
        assert_eq!(result.count, 3);
        // Should be ordered by priority
        assert_eq!(result.tasks[0].id, "FIX-001");
        assert_eq!(result.tasks[1].id, "US-001");
        assert_eq!(result.tasks[2].id, "US-002");
    }

    #[test]
    fn test_list_empty_database() {
        let (temp_dir, conn) = setup_test_db();
        drop(conn);

        let result = list(temp_dir.path(), None, None, None).unwrap();
        assert_eq!(result.count, 0);
        assert!(result.tasks.is_empty());
    }

    #[test]
    fn test_list_filter_by_status_todo() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        insert_test_task(&conn, "US-002", "Task 2", "done", 20);
        insert_test_task(&conn, "US-003", "Task 3", "todo", 30);
        drop(conn);

        let result = list(temp_dir.path(), Some(TaskStatusFilter::Todo), None, None).unwrap();
        assert_eq!(result.count, 2);
        assert!(result.tasks.iter().all(|t| t.status == "todo"));
    }

    #[test]
    fn test_list_filter_by_status_in_progress() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        insert_test_task(&conn, "US-002", "Task 2", "in_progress", 20);
        drop(conn);

        let result = list(
            temp_dir.path(),
            Some(TaskStatusFilter::InProgress),
            None,
            None,
        )
        .unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.tasks[0].id, "US-002");
    }

    #[test]
    fn test_list_filter_by_status_done() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "done", 10);
        insert_test_task(&conn, "US-002", "Task 2", "done", 20);
        insert_test_task(&conn, "US-003", "Task 3", "todo", 30);
        drop(conn);

        let result = list(temp_dir.path(), Some(TaskStatusFilter::Done), None, None).unwrap();
        assert_eq!(result.count, 2);
        assert!(result.tasks.iter().all(|t| t.status == "done"));
    }

    #[test]
    fn test_list_filter_by_task_type() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "User Story 1", "todo", 10);
        insert_test_task(&conn, "US-002", "User Story 2", "todo", 20);
        insert_test_task(&conn, "FIX-001", "Bug Fix 1", "todo", 5);
        insert_test_task(&conn, "TECH-001", "Tech Debt 1", "todo", 15);
        drop(conn);

        let result = list(temp_dir.path(), None, None, Some("US-")).unwrap();
        assert_eq!(result.count, 2);
        assert!(result.tasks.iter().all(|t| t.id.starts_with("US-")));
    }

    #[test]
    fn test_list_filter_by_file_glob() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        insert_test_task(&conn, "US-002", "Task 2", "todo", 20);
        insert_test_task(&conn, "US-003", "Task 3", "todo", 30);
        insert_test_task_file(&conn, "US-001", "src/commands/init.rs");
        insert_test_task_file(&conn, "US-001", "src/models/task.rs");
        insert_test_task_file(&conn, "US-002", "src/commands/list.rs");
        insert_test_task_file(&conn, "US-003", "Cargo.toml");
        drop(conn);

        // Match all .rs files in src/commands/
        let result = list(temp_dir.path(), None, Some("src/commands/*.rs"), None).unwrap();
        assert_eq!(result.count, 2);
        assert!(result.tasks.iter().any(|t| t.id == "US-001"));
        assert!(result.tasks.iter().any(|t| t.id == "US-002"));
    }

    #[test]
    fn test_list_filter_by_file_no_match() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        insert_test_task_file(&conn, "US-001", "src/main.rs");
        drop(conn);

        let result = list(temp_dir.path(), None, Some("nonexistent/*.rs"), None).unwrap();
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_list_combined_filters() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "User Story 1", "todo", 10);
        insert_test_task(&conn, "US-002", "User Story 2", "done", 20);
        insert_test_task(&conn, "FIX-001", "Bug Fix", "todo", 5);
        insert_test_task_file(&conn, "US-001", "src/commands/init.rs");
        insert_test_task_file(&conn, "US-002", "src/commands/init.rs");
        insert_test_task_file(&conn, "FIX-001", "src/commands/init.rs");
        drop(conn);

        // Status = todo, task type = US-, file = src/commands/*
        let result = list(
            temp_dir.path(),
            Some(TaskStatusFilter::Todo),
            Some("src/commands/*"),
            Some("US-"),
        )
        .unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.tasks[0].id, "US-001");
    }

    #[test]
    fn test_list_result_includes_filter_info() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        drop(conn);

        let result = list(
            temp_dir.path(),
            Some(TaskStatusFilter::Todo),
            Some("*.rs"),
            Some("US-"),
        )
        .unwrap();

        assert_eq!(result.filter_status, Some("todo".to_string()));
        assert_eq!(result.filter_file, Some("*.rs".to_string()));
        assert_eq!(result.filter_task_type, Some("US-".to_string()));
    }

    #[test]
    fn test_list_no_duplicates_with_multiple_files() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        // Task touches multiple files that match the pattern
        insert_test_task_file(&conn, "US-001", "src/commands/init.rs");
        insert_test_task_file(&conn, "US-001", "src/commands/list.rs");
        insert_test_task_file(&conn, "US-001", "src/commands/show.rs");
        drop(conn);

        let result = list(temp_dir.path(), None, Some("src/commands/*.rs"), None).unwrap();
        // Should only return the task once, not three times
        assert_eq!(result.count, 1);
        assert_eq!(result.tasks[0].id, "US-001");
    }

    #[test]
    fn test_format_text_with_tasks() {
        let result = ListResult {
            tasks: vec![
                TaskSummary {
                    id: "US-001".to_string(),
                    title: "Implement feature".to_string(),
                    status: "todo".to_string(),
                    priority: 10,
                },
                TaskSummary {
                    id: "FIX-001".to_string(),
                    title: "Fix bug".to_string(),
                    status: "done".to_string(),
                    priority: 5,
                },
            ],
            count: 2,
            filter_status: None,
            filter_file: None,
            filter_task_type: None,
        };

        let text = format_text(&result);
        assert!(text.contains("US-001"));
        assert!(text.contains("FIX-001"));
        assert!(text.contains("Implement feature"));
        assert!(text.contains("Fix bug"));
        assert!(text.contains("Total: 2 task(s)"));
    }

    #[test]
    fn test_format_text_empty() {
        let result = ListResult {
            tasks: vec![],
            count: 0,
            filter_status: None,
            filter_file: None,
            filter_task_type: None,
        };

        let text = format_text(&result);
        assert!(text.contains("No tasks found"));
    }

    #[test]
    fn test_format_text_truncates_long_titles() {
        let result = ListResult {
            tasks: vec![TaskSummary {
                id: "US-001".to_string(),
                title: "This is a very long title that exceeds the maximum display length"
                    .to_string(),
                status: "todo".to_string(),
                priority: 10,
            }],
            count: 1,
            filter_status: None,
            filter_file: None,
            filter_task_type: None,
        };

        let text = format_text(&result);
        assert!(text.contains("..."));
        // Should not contain the full title
        assert!(!text.contains("maximum display length"));
    }

    #[test]
    fn test_format_text_single_prefix_no_headers() {
        let result = ListResult {
            tasks: vec![
                TaskSummary {
                    id: "abc123-FEAT-001".to_string(),
                    title: "Feature 1".to_string(),
                    status: "todo".to_string(),
                    priority: 10,
                },
                TaskSummary {
                    id: "abc123-FEAT-002".to_string(),
                    title: "Feature 2".to_string(),
                    status: "done".to_string(),
                    priority: 20,
                },
            ],
            count: 2,
            filter_status: None,
            filter_file: None,
            filter_task_type: None,
        };

        let text = format_text(&result);
        // Single prefix → flat list, no section header
        assert!(!text.contains("=== abc123 ==="));
        assert!(text.contains("abc123-FEAT-001"));
        assert!(text.contains("abc123-FEAT-002"));
        assert!(text.contains("Total: 2 task(s)"));
    }

    #[test]
    fn test_format_text_multiple_prefixes_shows_headers() {
        let result = ListResult {
            tasks: vec![
                TaskSummary {
                    id: "abc123-FEAT-001".to_string(),
                    title: "Feature 1".to_string(),
                    status: "todo".to_string(),
                    priority: 10,
                },
                TaskSummary {
                    id: "def456-FEAT-001".to_string(),
                    title: "Other Feature".to_string(),
                    status: "done".to_string(),
                    priority: 10,
                },
            ],
            count: 2,
            filter_status: None,
            filter_file: None,
            filter_task_type: None,
        };

        let text = format_text(&result);
        // Two prefixes → section headers
        assert!(text.contains("=== abc123 ==="));
        assert!(text.contains("=== def456 ==="));
        assert!(text.contains("abc123-FEAT-001"));
        assert!(text.contains("def456-FEAT-001"));
        assert!(text.contains("Total: 2 task(s)"));
    }

    #[test]
    fn test_task_summary_from_task() {
        use crate::models::{Task, TaskStatus};

        let task = Task {
            id: "US-001".to_string(),
            title: "Test Task".to_string(),
            description: Some("Description".to_string()),
            priority: 15,
            status: TaskStatus::InProgress,
            notes: None,
            acceptance_criteria: vec![],
            review_scope: None,
            severity: None,
            source_review: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            last_error: None,
            error_count: 0,
            blocked_at_iteration: None,
            skipped_at_iteration: None,
            model: None,
            difficulty: None,
            escalation_note: None,
            required_tests: vec![],
        };

        let summary = TaskSummary::from(&task);
        assert_eq!(summary.id, "US-001");
        assert_eq!(summary.title, "Test Task");
        assert_eq!(summary.status, "in_progress");
        assert_eq!(summary.priority, 15);
    }
}
