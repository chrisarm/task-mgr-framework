//! Dependency section builder for the autonomous agent loop prompt.
//!
//! Queries completed `dependsOn` tasks and formats them as a prompt section
//! listing task IDs and titles so the agent knows what prerequisite work is done.

use rusqlite::Connection;

use crate::error::TaskMgrResult;

/// Build a dependency completion section string.
pub(crate) fn build_dependency_section(conn: &Connection, task_id: &str) -> String {
    let deps = match get_completed_dependencies(conn, task_id) {
        Ok(deps) if !deps.is_empty() => deps,
        _ => return String::new(),
    };

    let mut section = String::from("## Completed Dependencies\n\n");
    for (dep_id, dep_title) in &deps {
        section.push_str(&format!("- **{}**: {}\n", dep_id, dep_title));
    }
    section.push('\n');
    section
}

/// Get completed dependency task IDs and titles for a task.
fn get_completed_dependencies(
    conn: &Connection,
    task_id: &str,
) -> TaskMgrResult<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title FROM tasks t
         INNER JOIN task_relationships tr ON tr.related_id = t.id
         WHERE tr.task_id = ?1
           AND tr.rel_type = 'dependsOn'
           AND t.status = 'done'
           AND t.archived_at IS NULL
         ORDER BY t.id",
    )?;

    let deps: Vec<(String, String)> = stmt
        .query_map([task_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;

    Ok(deps)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::loop_engine::test_utils::{insert_relationship, insert_task, setup_test_db};

    #[test]
    fn test_get_completed_dependencies_none_done() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "DEP-001", "Still in progress", "in_progress", 1);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let deps = get_completed_dependencies(&conn, "TASK-001").unwrap();
        assert!(deps.is_empty(), "In-progress deps should not be listed");
    }

    #[test]
    fn test_get_completed_dependencies_ignores_synergy_relationships() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "SYN-001", "Synergy task", "done", 1);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        // Only synergyWith, NOT dependsOn
        insert_relationship(&conn, "TASK-001", "SYN-001", "synergyWith");

        let deps = get_completed_dependencies(&conn, "TASK-001").unwrap();
        assert!(
            deps.is_empty(),
            "Synergy relationships should not appear in dependency summaries"
        );
    }

    #[test]
    fn test_get_completed_dependencies_ordered_by_id() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "DEP-C", "Dep C", "done", 3);
        insert_task(&conn, "DEP-A", "Dep A", "done", 1);
        insert_task(&conn, "DEP-B", "Dep B", "done", 2);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "DEP-C", "dependsOn");
        insert_relationship(&conn, "TASK-001", "DEP-A", "dependsOn");
        insert_relationship(&conn, "TASK-001", "DEP-B", "dependsOn");

        let deps = get_completed_dependencies(&conn, "TASK-001").unwrap();
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].0, "DEP-A", "Should be ordered by ID");
        assert_eq!(deps[1].0, "DEP-B");
        assert_eq!(deps[2].0, "DEP-C");
    }
}
