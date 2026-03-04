//! Dependency satisfaction checking for tasks.
//!
//! Provides functions to query and validate whether a task's `dependsOn`
//! relationships are satisfied (i.e., all dependencies are `done` or
//! `irrelevant`) before allowing state transitions.

use rusqlite::Connection;

use crate::{TaskMgrError, TaskMgrResult};

/// Returns unsatisfied dependency IDs for a task.
///
/// Queries `task_relationships` for `dependsOn` entries, then checks if each
/// dependency is `done` or `irrelevant`. Returns only the IDs that are NOT
/// in a terminal state.
pub fn get_unsatisfied_deps(conn: &Connection, task_id: &str) -> TaskMgrResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT related_id FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'",
    )?;
    let dep_ids: Vec<String> = stmt
        .query_map([task_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut unsatisfied = Vec::new();
    for dep_id in &dep_ids {
        let status: Option<String> = conn
            .query_row("SELECT status FROM tasks WHERE id = ?", [dep_id], |row| {
                row.get(0)
            })
            .ok();
        match status.as_deref() {
            Some("done") | Some("irrelevant") => {} // satisfied
            _ => unsatisfied.push(dep_id.clone()),  // not done, missing, or other status
        }
    }

    Ok(unsatisfied)
}

/// Check whether all `dependsOn` dependencies for a task are satisfied.
///
/// Returns `true` if the task has no dependencies, or all dependencies are
/// `done` or `irrelevant`. **Fail-closed**: returns `false` on query errors.
pub fn are_dependencies_satisfied(conn: &Connection, task_id: &str) -> bool {
    match get_unsatisfied_deps(conn, task_id) {
        Ok(unsatisfied) => unsatisfied.is_empty(),
        Err(e) => {
            eprintln!(
                "Warning: dependency check failed for task {}, assuming unsatisfied: {}",
                task_id, e
            );
            false
        }
    }
}

/// Gate task completion on dependency satisfaction.
///
/// Returns `Ok(())` if all dependencies are met, or `Err(DependencyNotSatisfied)`
/// with the list of unsatisfied dependency IDs.
pub fn check_dependencies_satisfied(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    let unsatisfied = get_unsatisfied_deps(conn, task_id)?;
    if unsatisfied.is_empty() {
        Ok(())
    } else {
        Err(TaskMgrError::dependency_not_satisfied(task_id, unsatisfied))
    }
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

    fn insert_test_task(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, 'Test Task', ?, 10)",
            rusqlite::params![id, status],
        )
        .unwrap();
    }

    fn insert_relationship(conn: &Connection, task_id: &str, related_id: &str, rel_type: &str) {
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?, ?, ?)",
            rusqlite::params![task_id, related_id, rel_type],
        )
        .unwrap();
    }

    #[test]
    fn test_get_unsatisfied_deps_none() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "TASK-001", "in_progress");

        let result = get_unsatisfied_deps(&conn, "TASK-001").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_unsatisfied_deps_with_pending() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "todo");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = get_unsatisfied_deps(&conn, "TASK-001").unwrap();
        assert_eq!(result, vec!["DEP-001"]);
    }

    #[test]
    fn test_get_unsatisfied_deps_done_excluded() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "done");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = get_unsatisfied_deps(&conn, "TASK-001").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_unsatisfied_deps_irrelevant_excluded() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "irrelevant");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = get_unsatisfied_deps(&conn, "TASK-001").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_are_dependencies_satisfied_no_deps() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "TASK-001", "in_progress");

        assert!(are_dependencies_satisfied(&conn, "TASK-001"));
    }

    #[test]
    fn test_are_dependencies_satisfied_with_pending() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "todo");
        insert_test_task(&conn, "DEP-002", "done");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");
        insert_relationship(&conn, "TASK-001", "DEP-002", "dependsOn");

        assert!(!are_dependencies_satisfied(&conn, "TASK-001"));
        assert!(are_dependencies_satisfied(&conn, "DEP-001"));
    }

    #[test]
    fn test_check_dependencies_satisfied_ok() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "done");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        assert!(check_dependencies_satisfied(&conn, "TASK-001").is_ok());
    }

    #[test]
    fn test_check_dependencies_satisfied_err() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "todo");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = check_dependencies_satisfied(&conn, "TASK-001");
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
}
