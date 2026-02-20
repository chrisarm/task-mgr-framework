//! Metadata and global state schema definitions.
//!
//! Creates the `prd_metadata` and `global_state` tables for storing
//! PRD metadata and tracking global iteration state.

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Creates the prd_metadata table for preserving JSON PRD structure.
pub fn create_prd_metadata_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS prd_metadata (
            id INTEGER PRIMARY KEY CHECK(id = 1),  -- Only allow one row
            project TEXT NOT NULL,
            branch_name TEXT,
            description TEXT,
            priority_philosophy TEXT,              -- JSON object stored as TEXT
            global_acceptance_criteria TEXT,       -- JSON object stored as TEXT
            review_guidelines TEXT,                -- JSON object stored as TEXT
            raw_json TEXT,                         -- Full original JSON for round-trip
            imported_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates the global_state table for tracking iteration counter and other state.
pub fn create_global_state_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS global_state (
            id INTEGER PRIMARY KEY CHECK(id = 1),  -- Only allow one row
            iteration_counter INTEGER NOT NULL DEFAULT 0,
            last_task_id TEXT,                     -- Last selected task
            last_run_id TEXT,                      -- Most recent run
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
        [],
    )?;

    Ok(())
}

/// Creates the prd_files table for tracking files associated with a PRD.
///
/// Used by the `archive` command to discover which files to move.
pub fn create_prd_files_table(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS prd_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            prd_id INTEGER NOT NULL DEFAULT 1 REFERENCES prd_metadata(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            file_type TEXT NOT NULL CHECK(file_type IN ('prd', 'task_list', 'prompt')),
            UNIQUE(prd_id, file_path)
        )
        "#,
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prd_files_prd_id ON prd_files(prd_id)",
        [],
    )?;

    Ok(())
}

/// Initializes global_state with default values if not exists.
pub fn initialize_global_state(conn: &Connection) -> TaskMgrResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO global_state (id, iteration_counter) VALUES (1, 0)",
        [],
    )?;

    Ok(())
}
