//! Migration 3: Add FTS5 full-text search for learnings
//!
//! Creates an FTS5 virtual table for full-text search on learnings content.
//! Uses external content mode to avoid data duplication - the FTS index
//! references data in the learnings table rather than storing its own copy.
//!
//! Includes triggers to keep the FTS index synchronized with the learnings table.

use super::Migration;

/// Migration 3: Add FTS5 full-text search virtual table for learnings
pub static MIGRATION: Migration = Migration {
    version: 3,
    description: "Add FTS5 full-text search virtual table for learnings",
    up_sql: r#"
        -- Create FTS5 virtual table for full-text search on learnings
        -- We index title and content for BM25 scoring
        -- Note: content=learnings tells FTS5 to look up content from the learnings table
        -- content_rowid=id tells FTS5 to use the id column as the rowid
        CREATE VIRTUAL TABLE IF NOT EXISTS learnings_fts USING fts5(
            title,
            content,
            content=learnings,
            content_rowid=id
        );

        -- Triggers to keep FTS index in sync with learnings table
        -- After INSERT: add new learning to FTS index
        CREATE TRIGGER IF NOT EXISTS learnings_ai AFTER INSERT ON learnings BEGIN
            INSERT INTO learnings_fts(rowid, title, content)
            VALUES (new.id, new.title, new.content);
        END;

        -- After DELETE: remove learning from FTS index
        CREATE TRIGGER IF NOT EXISTS learnings_ad AFTER DELETE ON learnings BEGIN
            INSERT INTO learnings_fts(learnings_fts, rowid, title, content)
            VALUES ('delete', old.id, old.title, old.content);
        END;

        -- After UPDATE: update FTS index when title or content changes
        CREATE TRIGGER IF NOT EXISTS learnings_au AFTER UPDATE ON learnings BEGIN
            INSERT INTO learnings_fts(learnings_fts, rowid, title, content)
            VALUES ('delete', old.id, old.title, old.content);
            INSERT INTO learnings_fts(rowid, title, content)
            VALUES (new.id, new.title, new.content);
        END;

        -- Populate FTS index from existing learnings using rebuild command
        -- This is the proper way to populate an external content FTS5 table
        INSERT INTO learnings_fts(learnings_fts) VALUES('rebuild');

        -- Update schema version
        UPDATE global_state SET schema_version = 3 WHERE id = 1;
    "#,
    down_sql: r#"
        -- Drop triggers first
        DROP TRIGGER IF EXISTS learnings_ai;
        DROP TRIGGER IF EXISTS learnings_ad;
        DROP TRIGGER IF EXISTS learnings_au;

        -- Drop FTS virtual table
        DROP TABLE IF EXISTS learnings_fts;

        -- Update schema version back to 2
        UPDATE global_state SET schema_version = 2 WHERE id = 1;
    "#,
};
