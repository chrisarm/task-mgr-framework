//! Migration 1: Add schema_version column to global_state
//!
//! This is the bootstrap migration - after this, version tracking works.
//! It adds the schema_version column that enables tracking of applied migrations.

use super::Migration;

/// Migration 1: Add schema_version to global_state for migration tracking
pub static MIGRATION: Migration = Migration {
    version: 1,
    description: "Add schema_version to global_state for migration tracking",
    up_sql: r#"
        -- Add schema_version column with default 0 (pre-migration state)
        ALTER TABLE global_state ADD COLUMN schema_version INTEGER NOT NULL DEFAULT 0;
        -- Update to version 1 since we just applied this migration
        UPDATE global_state SET schema_version = 1 WHERE id = 1;
    "#,
    down_sql: r#"
        -- SQLite doesn't support DROP COLUMN directly, so we recreate the table
        -- This is safe because global_state only has one row
        CREATE TABLE global_state_new (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            iteration_counter INTEGER NOT NULL DEFAULT 0,
            last_task_id TEXT,
            last_run_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO global_state_new (id, iteration_counter, last_task_id, last_run_id, created_at, updated_at)
        SELECT id, iteration_counter, last_task_id, last_run_id, created_at, updated_at FROM global_state;
        DROP TABLE global_state;
        ALTER TABLE global_state_new RENAME TO global_state;
    "#,
};
