//! Migration 10: Add retired_at column to learnings table
//!
//! Adds `retired_at TEXT DEFAULT NULL` to support soft-archiving of learnings.
//! A NULL value means the learning is active; a non-NULL ISO-8601 timestamp
//! means the learning has been retired and should be excluded from retrieval queries.

use super::Migration;

/// Migration 10: Add retired_at to learnings for soft-archive support
pub static MIGRATION: Migration = Migration {
    version: 10,
    description: "Add retired_at column to learnings for soft-archive (curate phase 1)",
    up_sql: r#"
        ALTER TABLE learnings ADD COLUMN retired_at TEXT DEFAULT NULL;

        -- Update schema version
        UPDATE global_state SET schema_version = 10 WHERE id = 1;
    "#,
    down_sql: r#"
        -- SQLite doesn't support DROP COLUMN before 3.35.0,
        -- but we can safely leave the column in place.
        -- NULL columns are equivalent to not existing for our code.

        -- Update schema version back to 9
        UPDATE global_state SET schema_version = 9 WHERE id = 1;
    "#,
};
