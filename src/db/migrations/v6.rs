//! Migration 6: Add prd_files table
//!
//! Tracks which files belong to a PRD (task list JSON, prompt markdown,
//! PRD markdown). Used by the `archive` command to discover files to move
//! instead of guessing from the project name.

use super::Migration;

/// Migration 6: Create prd_files table
pub static MIGRATION: Migration = Migration {
    version: 6,
    description: "Add prd_files table for archive file discovery",
    up_sql: r#"
        CREATE TABLE IF NOT EXISTS prd_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            prd_id INTEGER NOT NULL DEFAULT 1 REFERENCES prd_metadata(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            file_type TEXT NOT NULL CHECK(file_type IN ('prd', 'task_list', 'prompt')),
            UNIQUE(prd_id, file_path)
        );
        CREATE INDEX IF NOT EXISTS idx_prd_files_prd_id ON prd_files(prd_id);

        -- Update schema version
        UPDATE global_state SET schema_version = 6 WHERE id = 1;
    "#,
    down_sql: r#"
        DROP INDEX IF EXISTS idx_prd_files_prd_id;
        DROP TABLE IF EXISTS prd_files;

        -- Update schema version back to 5
        UPDATE global_state SET schema_version = 5 WHERE id = 1;
    "#,
};
