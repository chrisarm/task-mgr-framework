//! Migration 8: FTS5 tag indexing (B4/FR-007)
//!
//! This migration extends the learnings FTS5 index to include tag content,
//! enabling full-text search over learning tags.
//!
//! Full implementation must:
//! 1. Add `tags_text TEXT` column to `learnings` table
//! 2. Rebuild `learnings_fts` virtual table to include `tags_text`
//! 3. Update INSERT/DELETE/UPDATE triggers to keep `tags_text` in sync
//! 4. Add triggers on `learning_tags` to update `tags_text` when tags are added/removed
//! 5. Populate `tags_text` from existing `learning_tags` rows
//!
//! FTS5 tokenizes hyphens using the default `ascii` tokenizer, so a tag like
//! `chrono-date-handling` becomes FTS5 tokens `chrono`, `date`, `handling`.
//! Searching for `chrono` will match a learning tagged `chrono-date-handling`.

use super::Migration;

/// Migration 8: Add FTS5 tag indexing via tags_text column
pub static MIGRATION: Migration = Migration {
    version: 8,
    description: "Add FTS5 tag indexing via tags_text column (B4/FR-007)",
    up_sql: r#"
        -- TODO (FEAT task): Add tags_text column to learnings
        --   ALTER TABLE learnings ADD COLUMN tags_text TEXT NOT NULL DEFAULT '';
        -- TODO (FEAT task): Rebuild learnings_fts to include tags_text column
        -- TODO (FEAT task): Update learnings_ai/ad/au triggers to include tags_text
        -- TODO (FEAT task): Add learning_tags_ai/ad triggers to update tags_text
        -- TODO (FEAT task): Populate tags_text from existing learning_tags rows

        -- Update schema version
        UPDATE global_state SET schema_version = 8 WHERE id = 1;
    "#,
    down_sql: r#"
        -- TODO (FEAT task): Remove learning_tags triggers
        -- TODO (FEAT task): Restore learnings_fts without tags_text
        -- TODO (FEAT task): Restore learnings_ai/ad/au triggers (without tags_text)
        -- TODO (FEAT task): Drop tags_text column (SQLite 3.35+ only; leave NULL otherwise)

        -- Update schema version back to 7
        UPDATE global_state SET schema_version = 7 WHERE id = 1;
    "#,
};
