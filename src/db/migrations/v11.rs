//! Migration 11: Add required_tests column to tasks table
//!
//! Adds `required_tests TEXT DEFAULT NULL` to support hard completion gating.
//! A JSON array of cargo test filter strings. When non-empty, `task-mgr complete`
//! runs each test and refuses completion if any fail.

use super::Migration;

/// Migration 11: Add required_tests to tasks for test-gated completion
pub static MIGRATION: Migration = Migration {
    version: 11,
    description: "Add required_tests column to tasks for test-gated completion",
    up_sql: r#"
        ALTER TABLE tasks ADD COLUMN required_tests TEXT DEFAULT NULL;

        -- Update schema version
        UPDATE global_state SET schema_version = 11 WHERE id = 1;
    "#,
    down_sql: r#"
        -- SQLite doesn't support DROP COLUMN before 3.35.0,
        -- but we can safely leave the column in place.
        -- NULL columns are equivalent to not existing for our code.

        -- Update schema version back to 10
        UPDATE global_state SET schema_version = 10 WHERE id = 1;
    "#,
};
