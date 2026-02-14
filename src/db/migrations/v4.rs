//! Migration 4: Add external_git_repo column to prd_metadata
//!
//! Stores an optional path to an external git repository where Claude
//! makes commits. Used by the loop engine to scan for task completions
//! in repos other than the working directory.

use super::Migration;

/// Migration 4: Add external_git_repo to prd_metadata
pub static MIGRATION: Migration = Migration {
    version: 4,
    description: "Add external_git_repo column to prd_metadata",
    up_sql: r#"
        ALTER TABLE prd_metadata ADD COLUMN external_git_repo TEXT;

        -- Update schema version
        UPDATE global_state SET schema_version = 4 WHERE id = 1;
    "#,
    down_sql: r#"
        -- SQLite doesn't support DROP COLUMN before 3.35.0,
        -- but we can safely leave the column in place.
        -- The column being NULL is the same as not existing for our code.

        -- Update schema version back to 3
        UPDATE global_state SET schema_version = 3 WHERE id = 1;
    "#,
};
