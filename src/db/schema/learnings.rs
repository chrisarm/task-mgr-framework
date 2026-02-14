//! Learnings schema definitions.
//!
//! Creates the `learnings` and `learning_tags` tables for institutional memory
//! along with their indexes.

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Creates the learnings table for institutional memory.
pub fn create_learnings_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS learnings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
            run_id TEXT REFERENCES runs(run_id) ON DELETE SET NULL,
            outcome TEXT NOT NULL
                CHECK(outcome IN ('failure', 'success', 'workaround', 'pattern')),
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            root_cause TEXT,                    -- What caused the issue (for failures)
            solution TEXT,                      -- How it was resolved
            applies_to_files TEXT,              -- JSON array of file patterns
            applies_to_task_types TEXT,         -- JSON array of task type prefixes
            applies_to_errors TEXT,             -- JSON array of error patterns
            confidence TEXT NOT NULL DEFAULT 'medium'
                CHECK(confidence IN ('high', 'medium', 'low')),
            times_shown INTEGER NOT NULL DEFAULT 0,
            times_applied INTEGER NOT NULL DEFAULT 0,
            last_shown_at TEXT,
            last_applied_at TEXT
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates the learning_tags table for flexible categorization.
pub fn create_learning_tags_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS learning_tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            learning_id INTEGER NOT NULL REFERENCES learnings(id) ON DELETE CASCADE,
            tag TEXT NOT NULL,
            UNIQUE(learning_id, tag)
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates indexes for learnings-related tables.
pub fn create_learnings_indexes(conn: &Connection) -> TaskMgrResult<()> {
    // Index on learnings outcome for filtering by learning type
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_learnings_outcome ON learnings(outcome)",
        [],
    )?;

    // Index on learnings task_id for finding learnings related to a task
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_learnings_task_id ON learnings(task_id)",
        [],
    )?;

    // Index on learnings created_at for ordering by recency
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_learnings_created_at ON learnings(created_at DESC)",
        [],
    )?;

    // Index on learnings run_id for finding learnings by run
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_learnings_run_id ON learnings(run_id)",
        [],
    )?;

    // Index on learning_tags learning_id for joining with learnings
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_learning_tags_learning_id ON learning_tags(learning_id)",
        [],
    )?;

    // Index on learning_tags tag for filtering by tag
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_learning_tags_tag ON learning_tags(tag)",
        [],
    )?;

    Ok(())
}
