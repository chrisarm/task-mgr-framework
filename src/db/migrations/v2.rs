//! Migration 2: Add UCB bandit columns to learnings table
//!
//! Adds sliding-window UCB (Upper Confidence Bound) columns for bandit-style
//! ranking of learnings. These columns track how often a learning has been
//! shown and applied within a sliding window of iterations.

use super::Migration;

/// Migration 2: Add UCB bandit columns for sliding-window ranking
pub static MIGRATION: Migration = Migration {
    version: 2,
    description: "Add sliding-window UCB columns to learnings for bandit ranking",
    up_sql: r#"
        -- Add columns for sliding-window UCB algorithm
        ALTER TABLE learnings ADD COLUMN window_shown INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE learnings ADD COLUMN window_applied INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE learnings ADD COLUMN window_start_iteration INTEGER;
        -- Update schema version
        UPDATE global_state SET schema_version = 2 WHERE id = 1;
    "#,
    down_sql: r#"
        -- SQLite doesn't support DROP COLUMN directly, so we recreate the table
        CREATE TABLE learnings_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
            run_id TEXT REFERENCES runs(run_id) ON DELETE SET NULL,
            outcome TEXT NOT NULL
                CHECK(outcome IN ('failure', 'success', 'workaround', 'pattern')),
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            root_cause TEXT,
            solution TEXT,
            applies_to_files TEXT,
            applies_to_task_types TEXT,
            applies_to_errors TEXT,
            confidence TEXT NOT NULL DEFAULT 'medium'
                CHECK(confidence IN ('high', 'medium', 'low')),
            times_shown INTEGER NOT NULL DEFAULT 0,
            times_applied INTEGER NOT NULL DEFAULT 0,
            last_shown_at TEXT,
            last_applied_at TEXT
        );
        INSERT INTO learnings_new (
            id, created_at, task_id, run_id, outcome, title, content,
            root_cause, solution, applies_to_files, applies_to_task_types,
            applies_to_errors, confidence, times_shown, times_applied,
            last_shown_at, last_applied_at
        )
        SELECT
            id, created_at, task_id, run_id, outcome, title, content,
            root_cause, solution, applies_to_files, applies_to_task_types,
            applies_to_errors, confidence, times_shown, times_applied,
            last_shown_at, last_applied_at
        FROM learnings;
        DROP TABLE learnings;
        ALTER TABLE learnings_new RENAME TO learnings;
        -- Recreate indexes
        CREATE INDEX IF NOT EXISTS idx_learnings_outcome ON learnings(outcome);
        CREATE INDEX IF NOT EXISTS idx_learnings_task_id ON learnings(task_id);
        CREATE INDEX IF NOT EXISTS idx_learnings_created_at ON learnings(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_learnings_run_id ON learnings(run_id);
    "#,
};
