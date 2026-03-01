//! Migration 9: Remove singleton CHECK(id=1) from prd_metadata, add AUTOINCREMENT
//! and UNIQUE constraint on task_prefix.
//!
//! This enables multiple PRD sessions to coexist in a single database, each
//! identified by a unique task_prefix (e.g., "P1", "SS").
//!
//! Changes:
//! 1. Recreates prd_metadata without CHECK(id=1) and with AUTOINCREMENT
//! 2. Adds UNIQUE constraint on task_prefix column
//! 3. Preserves all existing data
//!
//! Down migration restores the singleton constraint, keeping only the first row.

use super::Migration;

/// Migration 9: Remove prd_metadata singleton constraint, add task_prefix UNIQUE
pub static MIGRATION: Migration = Migration {
    version: 9,
    description: "Remove prd_metadata singleton constraint; add task_prefix UNIQUE (SS/FR-001)",
    up_sql: r#"
        -- Recreate prd_metadata without CHECK(id=1), with AUTOINCREMENT and UNIQUE task_prefix
        CREATE TABLE prd_metadata_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project TEXT NOT NULL,
            branch_name TEXT,
            description TEXT,
            priority_philosophy TEXT,
            global_acceptance_criteria TEXT,
            review_guidelines TEXT,
            raw_json TEXT,
            imported_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            external_git_repo TEXT,
            task_prefix TEXT UNIQUE,
            default_model TEXT
        );

        -- Preserve all existing rows
        INSERT INTO prd_metadata_new
            SELECT id, project, branch_name, description,
                   priority_philosophy, global_acceptance_criteria, review_guidelines,
                   raw_json, imported_at, updated_at, external_git_repo, task_prefix, default_model
            FROM prd_metadata;

        DROP TABLE prd_metadata;
        ALTER TABLE prd_metadata_new RENAME TO prd_metadata;

        UPDATE global_state SET schema_version = 9 WHERE id = 1;
    "#,
    down_sql: r#"
        -- Recreate prd_metadata with CHECK(id=1) singleton constraint
        CREATE TABLE prd_metadata_new (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            project TEXT NOT NULL,
            branch_name TEXT,
            description TEXT,
            priority_philosophy TEXT,
            global_acceptance_criteria TEXT,
            review_guidelines TEXT,
            raw_json TEXT,
            imported_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            external_git_repo TEXT,
            task_prefix TEXT,
            default_model TEXT
        );

        -- Restore only the first row (singleton constraint requires id=1)
        INSERT INTO prd_metadata_new
            SELECT id, project, branch_name, description,
                   priority_philosophy, global_acceptance_criteria, review_guidelines,
                   raw_json, imported_at, updated_at, external_git_repo, task_prefix, default_model
            FROM prd_metadata
            ORDER BY id ASC
            LIMIT 1;

        DROP TABLE prd_metadata;
        ALTER TABLE prd_metadata_new RENAME TO prd_metadata;

        UPDATE global_state SET schema_version = 8 WHERE id = 1;
    "#,
};
