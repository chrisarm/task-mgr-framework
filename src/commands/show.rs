//! Show detailed task information.
//!
//! This module implements the `show` command which displays detailed
//! information about a single task including its files and relationships.

use rusqlite::Connection;
use serde::Serialize;

use crate::db::open_and_migrate as open_connection;
use crate::models::{RelationshipType, Task, TaskRelationship};
use crate::{TaskMgrError, TaskMgrResult};

/// Result of the show command.
#[derive(Debug, Serialize)]
pub struct ShowResult {
    /// The task details
    pub task: Task,
    /// Files that this task touches
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Tasks that this task depends on (hard dependencies)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Tasks that have synergy with this task
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub synergy_with: Vec<String>,
    /// Tasks to batch with this task
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub batch_with: Vec<String>,
    /// Tasks that conflict with this task
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflicts_with: Vec<String>,
    /// Tasks that depend on this task (reverse dependency)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depended_on_by: Vec<String>,
}

/// Show detailed information about a task.
///
/// # Arguments
///
/// * `dir` - Directory containing the database
/// * `task_id` - The ID of the task to show
///
/// # Returns
///
/// Returns a `ShowResult` with task details, files, and relationships.
///
/// # Errors
///
/// Returns an error if:
/// - The database cannot be opened
/// - The task is not found
/// - Database queries fail
pub fn show(dir: &std::path::Path, task_id: &str) -> TaskMgrResult<ShowResult> {
    let conn = open_connection(dir)?;

    // Query the task by ID
    let task = query_task(&conn, task_id)?;

    // Query related files
    let files = query_task_files(&conn, task_id)?;

    // Query relationships where this task is the source
    let relationships = query_relationships(&conn, task_id)?;

    // Query reverse dependencies (tasks that depend on this one)
    let depended_on_by = query_reverse_dependencies(&conn, task_id)?;

    // Group relationships by type
    let mut depends_on = Vec::new();
    let mut synergy_with = Vec::new();
    let mut batch_with = Vec::new();
    let mut conflicts_with = Vec::new();

    for rel in relationships {
        match rel.rel_type {
            RelationshipType::DependsOn => depends_on.push(rel.related_id),
            RelationshipType::SynergyWith => synergy_with.push(rel.related_id),
            RelationshipType::BatchWith => batch_with.push(rel.related_id),
            RelationshipType::ConflictsWith => conflicts_with.push(rel.related_id),
        }
    }

    Ok(ShowResult {
        task,
        files,
        depends_on,
        synergy_with,
        batch_with,
        conflicts_with,
        depended_on_by,
    })
}

/// Query a single task by ID.
fn query_task(conn: &Connection, task_id: &str) -> TaskMgrResult<Task> {
    let mut stmt = conn.prepare(
        "SELECT id, title, description, priority, status, notes, \
         acceptance_criteria, review_scope, severity, source_review, \
         created_at, updated_at, started_at, completed_at, \
         last_error, error_count \
         FROM tasks WHERE id = ?",
    )?;

    let task = stmt
        .query_row([task_id], |row| {
            Task::try_from(row).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            other => TaskMgrError::DatabaseError(other),
        })?;

    Ok(task)
}

/// Query files associated with a task.
fn query_task_files(conn: &Connection, task_id: &str) -> TaskMgrResult<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT file_path FROM task_files WHERE task_id = ? ORDER BY file_path")?;
    let files = stmt
        .query_map([task_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(files)
}

/// Query relationships where this task is the source.
fn query_relationships(conn: &Connection, task_id: &str) -> TaskMgrResult<Vec<TaskRelationship>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, related_id, rel_type FROM task_relationships WHERE task_id = ? ORDER BY rel_type, related_id",
    )?;
    let relationships = stmt
        .query_map([task_id], |row| {
            TaskRelationship::try_from(row).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(relationships)
}

/// Query reverse dependencies (tasks that depend on this one).
fn query_reverse_dependencies(conn: &Connection, task_id: &str) -> TaskMgrResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT task_id FROM task_relationships WHERE related_id = ? AND rel_type = 'dependsOn' ORDER BY task_id",
    )?;
    let task_ids = stmt
        .query_map([task_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(task_ids)
}

/// Format show result as human-readable text.
pub fn format_text(result: &ShowResult) -> String {
    let mut output = String::new();

    // Header with task ID and title
    output.push_str(&format!(
        "Task: {} - {}\n",
        result.task.id, result.task.title
    ));
    output.push_str(&format!("{}\n\n", "=".repeat(60)));

    // Status and priority
    output.push_str(&format!("Status:   {}\n", result.task.status));
    output.push_str(&format!("Priority: {}\n", result.task.priority));

    // Description
    if let Some(ref desc) = result.task.description {
        output.push_str(&format!("\nDescription:\n  {}\n", desc));
    }

    // Notes
    if let Some(ref notes) = result.task.notes {
        output.push_str(&format!("\nNotes:\n  {}\n", notes));
    }

    // Acceptance criteria
    if !result.task.acceptance_criteria.is_empty() {
        output.push_str("\nAcceptance Criteria:\n");
        for (i, criterion) in result.task.acceptance_criteria.iter().enumerate() {
            output.push_str(&format!("  {}. {}\n", i + 1, criterion));
        }
    }

    // Files
    if !result.files.is_empty() {
        output.push_str("\nTouches Files:\n");
        for file in &result.files {
            output.push_str(&format!("  - {}\n", file));
        }
    }

    // Dependencies
    if !result.depends_on.is_empty() {
        output.push_str("\nDepends On:\n");
        for dep in &result.depends_on {
            output.push_str(&format!("  - {}\n", dep));
        }
    }

    // Synergy
    if !result.synergy_with.is_empty() {
        output.push_str("\nSynergy With:\n");
        for syn in &result.synergy_with {
            output.push_str(&format!("  - {}\n", syn));
        }
    }

    // Batch with
    if !result.batch_with.is_empty() {
        output.push_str("\nBatch With:\n");
        for batch in &result.batch_with {
            output.push_str(&format!("  - {}\n", batch));
        }
    }

    // Conflicts
    if !result.conflicts_with.is_empty() {
        output.push_str("\nConflicts With:\n");
        for conflict in &result.conflicts_with {
            output.push_str(&format!("  - {}\n", conflict));
        }
    }

    // Reverse dependencies
    if !result.depended_on_by.is_empty() {
        output.push_str("\nDepended On By:\n");
        for dep in &result.depended_on_by {
            output.push_str(&format!("  - {}\n", dep));
        }
    }

    // Error info (if any)
    if result.task.error_count > 0 {
        output.push_str(&format!("\nError Count: {}\n", result.task.error_count));
        if let Some(ref err) = result.task.last_error {
            output.push_str(&format!("Last Error: {}\n", err));
        }
    }

    // Timestamps
    output.push_str(&format!(
        "\nCreated:  {}\n",
        result.task.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    output.push_str(&format!(
        "Updated:  {}\n",
        result.task.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    if let Some(started) = result.task.started_at {
        output.push_str(&format!(
            "Started:  {}\n",
            started.format("%Y-%m-%d %H:%M:%S UTC")
        ));
    }
    if let Some(completed) = result.task.completed_at {
        output.push_str(&format!(
            "Completed: {}\n",
            completed.format("%Y-%m-%d %H:%M:%S UTC")
        ));
    }

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

    fn insert_test_relationship(
        conn: &Connection,
        task_id: &str,
        related_id: &str,
        rel_type: &str,
    ) {
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?, ?, ?)",
            params![task_id, related_id, rel_type],
        )
        .unwrap();
    }

    #[test]
    fn test_show_task_exists() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Test Task", "todo", 10);
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert_eq!(result.task.id, "US-001");
        assert_eq!(result.task.title, "Test Task");
        assert_eq!(result.task.priority, 10);
    }

    #[test]
    fn test_show_task_not_found() {
        let (temp_dir, conn) = setup_test_db();
        drop(conn);

        let result = show(temp_dir.path(), "NONEXISTENT");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("NONEXISTENT"));
    }

    #[test]
    fn test_show_includes_files() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Test Task", "todo", 10);
        insert_test_task_file(&conn, "US-001", "src/main.rs");
        insert_test_task_file(&conn, "US-001", "src/lib.rs");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert_eq!(result.files.len(), 2);
        assert!(result.files.contains(&"src/lib.rs".to_string()));
        assert!(result.files.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn test_show_includes_depends_on() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "done", 1);
        insert_test_task(&conn, "US-002", "Second Task", "todo", 2);
        insert_test_relationship(&conn, "US-002", "US-001", "dependsOn");
        drop(conn);

        let result = show(temp_dir.path(), "US-002").unwrap();
        assert_eq!(result.depends_on, vec!["US-001"]);
    }

    #[test]
    fn test_show_includes_synergy_with() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 1);
        insert_test_task(&conn, "US-002", "Second Task", "todo", 2);
        insert_test_relationship(&conn, "US-001", "US-002", "synergyWith");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert_eq!(result.synergy_with, vec!["US-002"]);
    }

    #[test]
    fn test_show_includes_batch_with() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 1);
        insert_test_task(&conn, "FIX-001", "Fix Task", "todo", 2);
        insert_test_relationship(&conn, "US-001", "FIX-001", "batchWith");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert_eq!(result.batch_with, vec!["FIX-001"]);
    }

    #[test]
    fn test_show_includes_conflicts_with() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 1);
        insert_test_task(&conn, "TECH-001", "Tech Task", "todo", 2);
        insert_test_relationship(&conn, "US-001", "TECH-001", "conflictsWith");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert_eq!(result.conflicts_with, vec!["TECH-001"]);
    }

    #[test]
    fn test_show_includes_reverse_dependencies() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "done", 1);
        insert_test_task(&conn, "US-002", "Second Task", "todo", 2);
        insert_test_task(&conn, "US-003", "Third Task", "todo", 3);
        insert_test_relationship(&conn, "US-002", "US-001", "dependsOn");
        insert_test_relationship(&conn, "US-003", "US-001", "dependsOn");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert_eq!(result.depended_on_by.len(), 2);
        assert!(result.depended_on_by.contains(&"US-002".to_string()));
        assert!(result.depended_on_by.contains(&"US-003".to_string()));
    }

    #[test]
    fn test_show_all_relationship_types() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-000", "Prereq", "done", 0);
        insert_test_task(&conn, "US-001", "Main Task", "todo", 1);
        insert_test_task(&conn, "US-002", "Synergy", "todo", 2);
        insert_test_task(&conn, "FIX-001", "Batch", "todo", 3);
        insert_test_task(&conn, "TECH-001", "Conflict", "todo", 4);
        insert_test_task(&conn, "US-005", "Dependent", "todo", 5);

        insert_test_relationship(&conn, "US-001", "US-000", "dependsOn");
        insert_test_relationship(&conn, "US-001", "US-002", "synergyWith");
        insert_test_relationship(&conn, "US-001", "FIX-001", "batchWith");
        insert_test_relationship(&conn, "US-001", "TECH-001", "conflictsWith");
        insert_test_relationship(&conn, "US-005", "US-001", "dependsOn");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert_eq!(result.depends_on, vec!["US-000"]);
        assert_eq!(result.synergy_with, vec!["US-002"]);
        assert_eq!(result.batch_with, vec!["FIX-001"]);
        assert_eq!(result.conflicts_with, vec!["TECH-001"]);
        assert_eq!(result.depended_on_by, vec!["US-005"]);
    }

    #[test]
    fn test_show_empty_relationships() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Solo Task", "todo", 1);
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert!(result.files.is_empty());
        assert!(result.depends_on.is_empty());
        assert!(result.synergy_with.is_empty());
        assert!(result.batch_with.is_empty());
        assert!(result.conflicts_with.is_empty());
        assert!(result.depended_on_by.is_empty());
    }

    #[test]
    fn test_format_text_basic() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Test Task", "todo", 10);
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        let text = format_text(&result);

        assert!(text.contains("Task: US-001 - Test Task"));
        assert!(text.contains("Status:   todo"));
        assert!(text.contains("Priority: 10"));
    }

    #[test]
    fn test_format_text_with_files() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Test Task", "todo", 10);
        insert_test_task_file(&conn, "US-001", "src/main.rs");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        let text = format_text(&result);

        assert!(text.contains("Touches Files:"));
        assert!(text.contains("src/main.rs"));
    }

    #[test]
    fn test_format_text_with_dependencies() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "done", 1);
        insert_test_task(&conn, "US-002", "Second Task", "todo", 2);
        insert_test_relationship(&conn, "US-002", "US-001", "dependsOn");
        drop(conn);

        let result = show(temp_dir.path(), "US-002").unwrap();
        let text = format_text(&result);

        assert!(text.contains("Depends On:"));
        assert!(text.contains("US-001"));
    }

    #[test]
    fn test_format_text_with_reverse_dependencies() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "done", 1);
        insert_test_task(&conn, "US-002", "Second Task", "todo", 2);
        insert_test_relationship(&conn, "US-002", "US-001", "dependsOn");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        let text = format_text(&result);

        assert!(text.contains("Depended On By:"));
        assert!(text.contains("US-002"));
    }

    #[test]
    fn test_show_result_serialization() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Test Task", "todo", 10);
        insert_test_task_file(&conn, "US-001", "src/main.rs");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        let json = serde_json::to_string(&result).unwrap();

        assert!(json.contains(r#""id":"US-001""#));
        assert!(json.contains(r#""title":"Test Task""#));
        assert!(json.contains(r#""files":["src/main.rs"]"#));
    }

    #[test]
    fn test_show_result_serialization_skips_empty() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Test Task", "todo", 10);
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        let json = serde_json::to_string(&result).unwrap();

        // Empty vecs should be omitted
        assert!(!json.contains(r#""files""#));
        assert!(!json.contains(r#""depends_on""#));
        assert!(!json.contains(r#""synergy_with""#));
        assert!(!json.contains(r#""batch_with""#));
        assert!(!json.contains(r#""conflicts_with""#));
        assert!(!json.contains(r#""depended_on_by""#));
    }

    #[test]
    fn test_files_ordered_alphabetically() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Test Task", "todo", 10);
        insert_test_task_file(&conn, "US-001", "src/z_last.rs");
        insert_test_task_file(&conn, "US-001", "src/a_first.rs");
        insert_test_task_file(&conn, "US-001", "src/m_middle.rs");
        drop(conn);

        let result = show(temp_dir.path(), "US-001").unwrap();
        assert_eq!(result.files[0], "src/a_first.rs");
        assert_eq!(result.files[1], "src/m_middle.rs");
        assert_eq!(result.files[2], "src/z_last.rs");
    }
}
