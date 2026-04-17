//! Migration 7: Add model selection fields
//!
//! Adds model, difficulty, and escalation_note columns to tasks table,
//! and default_model column to prd_metadata table.
//! These support per-task model selection and escalation in the loop engine.

use super::Migration;

/// Migration 7: Add model selection fields to tasks and prd_metadata
pub static MIGRATION: Migration = Migration {
    version: 7,
    description: "Add model, difficulty, escalation_note to tasks and default_model to prd_metadata",
    up_sql: r#"
        ALTER TABLE tasks ADD COLUMN model TEXT;
        ALTER TABLE tasks ADD COLUMN difficulty TEXT;
        ALTER TABLE tasks ADD COLUMN escalation_note TEXT;
        ALTER TABLE prd_metadata ADD COLUMN default_model TEXT;

        -- Update schema version
        UPDATE global_state SET schema_version = 7 WHERE id = 1;
    "#,
    down_sql: r#"
        -- SQLite doesn't support DROP COLUMN before 3.35.0,
        -- but we can safely leave the columns in place.
        -- NULL columns are equivalent to not existing for our code.

        -- Update schema version back to 6
        UPDATE global_state SET schema_version = 6 WHERE id = 1;
    "#,
};
