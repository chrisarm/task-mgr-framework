//! Migration 22: Create enrich_examined table so `curate enrich` skips
//! learnings the LLM has already answered for.
//!
//! ## Changes
//! - Creates `enrich_examined` (`learning_id` PK, `examined_at`).
//!
//! ## Semantics
//! - A row means an enrich LLM batch returned an answer for this learning and
//!   the answer was applied. Metadata NULL-ness cannot carry this: "no error
//!   patterns apply" leaves `applies_to_errors` NULL, and `edit_learning`
//!   normalizes empty lists back to NULL.
//! - `ON DELETE CASCADE` so a reused rowid never inherits a stale marker.

use super::Migration;

/// Migration 22: Create enrich_examined table.
pub static MIGRATION: Migration = Migration {
    version: 22,
    description: "Create enrich_examined table for persisting curate enrich passes",
    up_sql: r#"
        CREATE TABLE enrich_examined (
            learning_id INTEGER PRIMARY KEY REFERENCES learnings(id) ON DELETE CASCADE,
            examined_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        UPDATE global_state SET schema_version = 22 WHERE id = 1;
    "#,
    down_sql: r#"
        DROP TABLE IF EXISTS enrich_examined;
        UPDATE global_state SET schema_version = 21 WHERE id = 1;
    "#,
};

#[cfg(test)]
mod tests {
    use crate::db::migrations::{CURRENT_SCHEMA_VERSION, get_schema_version, run_migrations};
    use crate::db::{create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_migrated_db() -> (TempDir, rusqlite::Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    #[test]
    fn test_v22_table_exists_at_current_version() {
        const _: () = assert!(CURRENT_SCHEMA_VERSION >= 22);
        let (_tmp, conn) = setup_migrated_db();
        assert!(get_schema_version(&conn).unwrap() >= 22);
        for col in ["learning_id", "examined_at"] {
            let exists: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) > 0 FROM pragma_table_info('enrich_examined') WHERE name = '{col}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "enrich_examined.{col} must exist after v22");
        }
    }

    #[test]
    fn test_v22_marker_requires_existing_learning() {
        let (_tmp, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO enrich_examined (learning_id) VALUES (999999)",
            [],
        )
        .expect_err("marker for a missing learning must violate the FK");
    }
}
