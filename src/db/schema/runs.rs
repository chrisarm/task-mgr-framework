//! Run tracking schema definitions.
//!
//! Creates the `runs` and `run_tasks` tables for tracking execution sessions
//! along with their indexes.

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Creates the runs table for tracking execution sessions.
pub fn create_runs_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS runs (
            run_id TEXT PRIMARY KEY NOT NULL,
            started_at TEXT NOT NULL DEFAULT (datetime('now')),
            ended_at TEXT,
            status TEXT NOT NULL DEFAULT 'active'
                CHECK(status IN ('active', 'completed', 'aborted')),
            last_commit TEXT,              -- Most recent git commit hash
            last_files TEXT,               -- JSON array of recently modified files
            iteration_count INTEGER NOT NULL DEFAULT 0,
            notes TEXT
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates the run_tasks table linking runs to tasks with iteration tracking.
pub fn create_run_tasks_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS run_tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            status TEXT NOT NULL DEFAULT 'started'
                CHECK(status IN ('started', 'completed', 'failed', 'skipped')),
            iteration INTEGER NOT NULL,    -- Which iteration within the run
            started_at TEXT NOT NULL DEFAULT (datetime('now')),
            ended_at TEXT,
            duration_seconds INTEGER,
            notes TEXT,
            UNIQUE(run_id, task_id, iteration)
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates indexes for run-related tables.
pub fn create_runs_indexes(conn: &Connection) -> TaskMgrResult<()> {
    // Index on runs status for filtering active runs
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_runs_status ON runs(status)",
        [],
    )?;

    // Index on run_tasks run_id for joining with runs
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_run_tasks_run_id ON run_tasks(run_id)",
        [],
    )?;

    // Index on run_tasks task_id for finding run history of a task
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_run_tasks_task_id ON run_tasks(task_id)",
        [],
    )?;

    Ok(())
}
