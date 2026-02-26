//! Migration 8: FTS5 tag indexing (B4/FR-007)
//!
//! This migration extends the learnings FTS5 index to include tag content,
//! enabling full-text search over learning tags.
//!
//! Changes:
//! 1. Adds `tags_text TEXT NOT NULL DEFAULT ''` column to `learnings` table
//! 2. Rebuilds `learnings_fts` virtual table with 3 columns: title, content, tags_text
//! 3. Updates learnings_ai/ad/au triggers to include tags_text
//! 4. Adds learning_tags_ai/ad triggers to sync tags_text when tags are added/removed
//! 5. Backfills tags_text + FTS5 index for existing learnings
//!
//! FTS5 tokenizes hyphens using the default `ascii` tokenizer, so a tag like
//! `chrono-date-handling` becomes FTS5 tokens `chrono`, `date`, `handling`.
//! Searching for `chrono` will match a learning tagged `chrono-date-handling`.
//!
//! ## FTS5 population strategy (no `rebuild`, no trigger-based tombstones)
//!
//! Two pitfalls to avoid when rebuilding FTS5 within a single SQLite transaction:
//!
//! 1. `INSERT INTO learnings_fts(learnings_fts) VALUES('rebuild')` after
//!    DROP+CREATE causes SQLITE_CORRUPT_VTAB (267) because FTS5 shadow table
//!    pages get reused and the FTS5 cookie becomes inconsistent.
//!
//! 2. Firing `learnings_au` (delete+insert trigger) on a fresh empty FTS5 index
//!    creates orphaned tombstone entries that also cause SQLITE_CORRUPT_VTAB.
//!
//! Safe approach:
//! - Backfill `tags_text` via plain UPDATE (triggers not yet created → no FTS5 writes)
//! - Populate FTS5 via direct `INSERT ... SELECT` (no tombstones, no rebuild)
//! - Create the triggers AFTER the FTS5 is cleanly populated
//!
//! Trigger chain for future tag changes:
//!   INSERT/DELETE on learning_tags
//!     → learning_tags_ai/ad fires → UPDATE learnings SET tags_text = ...
//!       → learnings_au fires → FTS5 index updated via delete+insert

use super::Migration;

/// Migration 8: Add FTS5 tag indexing via tags_text column
pub static MIGRATION: Migration = Migration {
    version: 8,
    description: "Add FTS5 tag indexing via tags_text column (B4/FR-007)",
    up_sql: r#"
        -- Step 1: Add tags_text column to learnings (empty string default, NOT NULL)
        ALTER TABLE learnings ADD COLUMN tags_text TEXT NOT NULL DEFAULT '';

        -- Step 2: Drop existing learnings_fts triggers and table so we can rebuild
        -- with the new 3-column schema (title, content, tags_text).
        -- All triggers are dropped first so no FTS5 writes happen during backfill.
        DROP TRIGGER IF EXISTS learnings_ai;
        DROP TRIGGER IF EXISTS learnings_ad;
        DROP TRIGGER IF EXISTS learnings_au;
        DROP TABLE IF EXISTS learnings_fts;

        -- Step 3: Recreate FTS5 virtual table with 3 columns including tags_text.
        -- External content mode: FTS5 reads from the learnings table via content_rowid.
        CREATE VIRTUAL TABLE IF NOT EXISTS learnings_fts USING fts5(
            title,
            content,
            tags_text,
            content=learnings,
            content_rowid=id
        );

        -- Step 4: Backfill tags_text for all existing learnings.
        -- Triggers are not yet created, so this plain UPDATE writes only to learnings
        -- and does NOT touch FTS5 (avoids orphaned tombstone entries).
        UPDATE learnings
        SET tags_text = COALESCE((
            SELECT group_concat(tag, ' ')
            FROM learning_tags
            WHERE learning_id = learnings.id
        ), '');

        -- Step 5: Populate FTS5 via direct INSERT SELECT (no rebuild, no tombstones).
        -- After DROP+CREATE within one transaction, 'rebuild' causes SQLITE_CORRUPT_VTAB
        -- due to FTS5 shadow page reuse. A direct insert bypasses that internal path.
        INSERT INTO learnings_fts(rowid, title, content, tags_text)
        SELECT id, title, content, tags_text FROM learnings;

        -- Step 6: Create updated learnings triggers (now include tags_text).
        -- These are created AFTER FTS5 is populated to prevent tombstone issues.

        -- After INSERT: add new learning to FTS index
        CREATE TRIGGER IF NOT EXISTS learnings_ai AFTER INSERT ON learnings BEGIN
            INSERT INTO learnings_fts(rowid, title, content, tags_text)
            VALUES (new.id, new.title, new.content, new.tags_text);
        END;

        -- After DELETE: remove learning from FTS index
        CREATE TRIGGER IF NOT EXISTS learnings_ad AFTER DELETE ON learnings BEGIN
            INSERT INTO learnings_fts(learnings_fts, rowid, title, content, tags_text)
            VALUES ('delete', old.id, old.title, old.content, old.tags_text);
        END;

        -- After UPDATE: update FTS index (delete old entry, insert new entry)
        CREATE TRIGGER IF NOT EXISTS learnings_au AFTER UPDATE ON learnings BEGIN
            INSERT INTO learnings_fts(learnings_fts, rowid, title, content, tags_text)
            VALUES ('delete', old.id, old.title, old.content, old.tags_text);
            INSERT INTO learnings_fts(rowid, title, content, tags_text)
            VALUES (new.id, new.title, new.content, new.tags_text);
        END;

        -- Step 7: Add triggers on learning_tags to keep tags_text in sync.
        -- When a tag is inserted: rebuild tags_text for the parent learning.
        -- This UPDATE on learnings cascades to learnings_au, which updates FTS5.
        CREATE TRIGGER IF NOT EXISTS learning_tags_ai AFTER INSERT ON learning_tags BEGIN
            UPDATE learnings
            SET tags_text = COALESCE((
                SELECT group_concat(tag, ' ')
                FROM learning_tags
                WHERE learning_id = NEW.learning_id
            ), '')
            WHERE id = NEW.learning_id;
        END;

        -- When a tag is deleted: rebuild tags_text for the parent learning.
        -- This UPDATE on learnings cascades to learnings_au, which updates FTS5.
        CREATE TRIGGER IF NOT EXISTS learning_tags_ad AFTER DELETE ON learning_tags BEGIN
            UPDATE learnings
            SET tags_text = COALESCE((
                SELECT group_concat(tag, ' ')
                FROM learning_tags
                WHERE learning_id = OLD.learning_id
            ), '')
            WHERE id = OLD.learning_id;
        END;

        -- Update schema version
        UPDATE global_state SET schema_version = 8 WHERE id = 1;
    "#,
    down_sql: r#"
        -- Drop the new learning_tags sync triggers
        DROP TRIGGER IF EXISTS learning_tags_ai;
        DROP TRIGGER IF EXISTS learning_tags_ad;

        -- Drop the 3-column FTS5 table and its triggers
        DROP TRIGGER IF EXISTS learnings_ai;
        DROP TRIGGER IF EXISTS learnings_ad;
        DROP TRIGGER IF EXISTS learnings_au;
        DROP TABLE IF EXISTS learnings_fts;

        -- Restore 2-column FTS5 table (as it was in v3, without tags_text)
        CREATE VIRTUAL TABLE IF NOT EXISTS learnings_fts USING fts5(
            title,
            content,
            content=learnings,
            content_rowid=id
        );

        -- Repopulate FTS5 (2-column) from existing learnings via direct INSERT SELECT.
        -- Same rationale as the UP migration: direct INSERT avoids rebuild corruption.
        INSERT INTO learnings_fts(rowid, title, content)
        SELECT id, title, content FROM learnings;

        -- Restore 2-column learnings triggers (created after FTS5 is populated)
        CREATE TRIGGER IF NOT EXISTS learnings_ai AFTER INSERT ON learnings BEGIN
            INSERT INTO learnings_fts(rowid, title, content)
            VALUES (new.id, new.title, new.content);
        END;

        CREATE TRIGGER IF NOT EXISTS learnings_ad AFTER DELETE ON learnings BEGIN
            INSERT INTO learnings_fts(learnings_fts, rowid, title, content)
            VALUES ('delete', old.id, old.title, old.content);
        END;

        CREATE TRIGGER IF NOT EXISTS learnings_au AFTER UPDATE ON learnings BEGIN
            INSERT INTO learnings_fts(learnings_fts, rowid, title, content)
            VALUES ('delete', old.id, old.title, old.content);
            INSERT INTO learnings_fts(rowid, title, content)
            VALUES (new.id, new.title, new.content);
        END;

        -- Note: SQLite has limited DROP COLUMN support; tags_text column remains
        -- in learnings table but is no longer maintained by triggers.

        -- Update schema version back to 7
        UPDATE global_state SET schema_version = 7 WHERE id = 1;
    "#,
};
