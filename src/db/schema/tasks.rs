//! Task-related schema definitions.
//!
//! Creates the `tasks`, `task_files`, and `task_relationships` tables
//! along with their indexes.

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Creates the tasks table with all fields from PRD user stories.
pub fn create_tasks_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY NOT NULL,
            title TEXT NOT NULL,
            description TEXT,
            priority INTEGER NOT NULL DEFAULT 50,
            status TEXT NOT NULL DEFAULT 'todo'
                CHECK(status IN ('todo', 'in_progress', 'done', 'blocked', 'skipped', 'irrelevant')),
            notes TEXT,
            acceptance_criteria TEXT,  -- JSON array stored as TEXT
            review_scope TEXT,         -- JSON object stored as TEXT (optional)
            severity TEXT,             -- For review tasks (optional)
            source_review TEXT,        -- For review tasks (optional)
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            started_at TEXT,           -- When task was first claimed
            completed_at TEXT,         -- When task was marked done
            last_error TEXT,           -- Most recent error message
            error_count INTEGER NOT NULL DEFAULT 0,
            blocked_at_iteration INTEGER,  -- Global iteration when task was blocked (for decay)
            skipped_at_iteration INTEGER   -- Global iteration when task was skipped (for decay)
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates the task_files table for touchesFiles relationship.
pub fn create_task_files_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS task_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            UNIQUE(task_id, file_path)
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates the task_relationships table for inter-task relationships.
pub fn create_task_relationships_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS task_relationships (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            related_id TEXT NOT NULL,
            rel_type TEXT NOT NULL
                CHECK(rel_type IN ('dependsOn', 'synergyWith', 'batchWith', 'conflictsWith')),
            UNIQUE(task_id, related_id, rel_type)
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates indexes for task-related tables.
pub fn create_tasks_indexes(conn: &Connection) -> TaskMgrResult<()> {
    // Index on status for filtering tasks by status
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status)",
        [],
    )?;

    // Index on priority for ordering tasks
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority)",
        [],
    )?;

    // Composite index for next command's primary query pattern:
    // SELECT ... FROM tasks WHERE status = 'todo' ORDER BY priority
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_tasks_status_priority ON tasks(status, priority)",
        [],
    )?;

    // Index on task_files task_id for finding files by task
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_files_task_id ON task_files(task_id)",
        [],
    )?;

    // Index on task_files file_path for reverse lookups (which tasks touch a file)
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_files_file_path ON task_files(file_path)",
        [],
    )?;

    // Index on task_relationships task_id for finding relationships from a task
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_relationships_task_id ON task_relationships(task_id)",
        [],
    )?;

    // Index on task_relationships related_id for reverse lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_relationships_related_id ON task_relationships(related_id)",
        [],
    )?;

    // Index on task_relationships rel_type for filtering by relationship type
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_relationships_rel_type ON task_relationships(rel_type)",
        [],
    )?;

    // Composite index for relationship queries that filter by type and return task_id/related_id
    // This provides a covering index for the task selection query pattern:
    // SELECT task_id, related_id FROM task_relationships WHERE rel_type = ?
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_relationships_type_taskid ON task_relationships(rel_type, task_id, related_id)",
        [],
    )?;

    Ok(())
}
