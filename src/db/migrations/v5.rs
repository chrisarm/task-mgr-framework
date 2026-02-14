//! Migration 5: Add task_prefix column to prd_metadata
//!
//! Stores the task ID prefix used during `init --from-json` import.
//! The loop engine uses this to strip auto-generated prefixes from DB task IDs
//! when matching against commit messages (which use unprefixed IDs).

use super::Migration;

/// Migration 5: Add task_prefix to prd_metadata
pub static MIGRATION: Migration = Migration {
    version: 5,
    description: "Add task_prefix column to prd_metadata",
    up_sql: r#"
        ALTER TABLE prd_metadata ADD COLUMN task_prefix TEXT;

        -- Update schema version
        UPDATE global_state SET schema_version = 5 WHERE id = 1;
    "#,
    down_sql: r#"
        -- SQLite doesn't support DROP COLUMN before 3.35.0,
        -- but we can safely leave the column in place.
        -- The column being NULL is the same as not existing for our code.

        -- Update schema version back to 4
        UPDATE global_state SET schema_version = 4 WHERE id = 1;
    "#,
};
