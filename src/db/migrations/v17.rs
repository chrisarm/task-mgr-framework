//! Migration 17: Create learning_supersessions table for tracking replacements.
//!
//! ## Changes
//! - Creates `learning_supersessions` table with:
//!   - `id` INTEGER PRIMARY KEY AUTOINCREMENT
//!   - `old_learning_id` INTEGER NOT NULL (FK to learnings.id, ON DELETE CASCADE)
//!   - `new_learning_id` INTEGER NOT NULL (FK to learnings.id, ON DELETE CASCADE)
//!   - `reason` TEXT — optional explanation for the replacement
//!   - `created_at` TEXT NOT NULL DEFAULT (datetime('now'))
//! - UNIQUE(old_learning_id, new_learning_id) prevents duplicate supersession rows.
//! - idx_supersessions_old / idx_supersessions_new speed up filter-and-annotate queries.
//!
//! ## Semantics
//! - A row means `new_learning_id` supersedes (replaces) `old_learning_id`.
//! - Recall must exclude superseded learnings by default (FEAT-005).
//! - Self-supersession (old == new) is enforced at the application layer, not via
//!   a CHECK constraint, so the CLI can return a human-readable error message.

use super::Migration;

/// Migration 17: Create learning_supersessions table.
pub static MIGRATION: Migration = Migration {
    version: 17,
    description: "Create learning_supersessions table for tracking learning replacements",
    up_sql: r#"
        CREATE TABLE learning_supersessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            old_learning_id INTEGER NOT NULL REFERENCES learnings(id) ON DELETE CASCADE,
            new_learning_id INTEGER NOT NULL REFERENCES learnings(id) ON DELETE CASCADE,
            reason TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(old_learning_id, new_learning_id)
        );
        CREATE INDEX idx_supersessions_old ON learning_supersessions(old_learning_id);
        CREATE INDEX idx_supersessions_new ON learning_supersessions(new_learning_id);
        UPDATE global_state SET schema_version = 17 WHERE id = 1;
    "#,
    down_sql: r#"
        DROP INDEX IF EXISTS idx_supersessions_new;
        DROP INDEX IF EXISTS idx_supersessions_old;
        DROP TABLE IF EXISTS learning_supersessions;
        UPDATE global_state SET schema_version = 16 WHERE id = 1;
    "#,
};

#[cfg(test)]
mod tests {
    use crate::db::migrations::{MIGRATIONS, get_schema_version, run_migrations};
    use crate::db::{create_schema, open_connection};
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn setup_migrated_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_learning(conn: &Connection, id: i64, title: &str) {
        conn.execute(
            "INSERT INTO learnings (id, title, content, outcome) VALUES (?1, ?2, 'content', 'pattern')",
            rusqlite::params![id, title],
        )
        .unwrap();
    }

    /// AC: learning_supersessions table exists after migration v17.
    #[test]
    fn test_v17_learning_supersessions_table_exists() {
        let (_temp_dir, conn) = setup_migrated_db();

        // Schema version bumped to >= 17
        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 17,
            "DB schema_version must be >= 17 after running migrations, got {version}"
        );

        let expected_columns = [
            "id",
            "old_learning_id",
            "new_learning_id",
            "reason",
            "created_at",
        ];
        for col in expected_columns {
            let exists: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) > 0 FROM pragma_table_info('learning_supersessions') WHERE name = '{col}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                exists,
                "learning_supersessions.{col} column must exist after v17 migration"
            );
        }
    }

    /// AC: indexes on old_learning_id and new_learning_id exist.
    #[test]
    fn test_v17_indexes_exist() {
        let (_temp_dir, conn) = setup_migrated_db();

        for idx in ["idx_supersessions_old", "idx_supersessions_new"] {
            let exists: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='{idx}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "{idx} index must exist after v17 migration");
        }
    }

    /// AC: UNIQUE constraint prevents duplicate (old_id, new_id) pairs.
    #[test]
    fn test_v17_unique_constraint_prevents_duplicates() {
        let (_temp_dir, conn) = setup_migrated_db();
        insert_learning(&conn, 1, "Old");
        insert_learning(&conn, 2, "New");

        conn.execute(
            "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (1, 2)",
            [],
        )
        .unwrap();

        // Same pair must fail
        let dup = conn.execute(
            "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (1, 2)",
            [],
        );
        assert!(
            dup.is_err(),
            "UNIQUE(old_learning_id, new_learning_id) must reject duplicate pair"
        );

        // Different pair with same old_id is allowed (supersession chain / split)
        insert_learning(&conn, 3, "Another new");
        let different = conn.execute(
            "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (1, 3)",
            [],
        );
        assert!(
            different.is_ok(),
            "UNIQUE is on the pair — different new_id with same old_id must be allowed"
        );
    }

    /// AC: ON DELETE CASCADE removes supersession row when the OLD learning is deleted.
    #[test]
    fn test_v17_cascade_delete_from_old_learning() {
        let (_temp_dir, conn) = setup_migrated_db();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        insert_learning(&conn, 1, "Old");
        insert_learning(&conn, 2, "New");

        conn.execute(
            "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (1, 2)",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM learnings WHERE id = 1", [])
            .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_supersessions WHERE old_learning_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "deleting the old learning must cascade-remove its supersession rows"
        );
    }

    /// AC: ON DELETE CASCADE removes supersession row when the NEW learning is deleted.
    #[test]
    fn test_v17_cascade_delete_from_new_learning() {
        let (_temp_dir, conn) = setup_migrated_db();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        insert_learning(&conn, 1, "Old");
        insert_learning(&conn, 2, "New");

        conn.execute(
            "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (1, 2)",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM learnings WHERE id = 2", [])
            .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_supersessions WHERE new_learning_id = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "deleting the new learning must cascade-remove its supersession rows"
        );
    }

    /// AC: v17 down migration drops table and reverts to schema version 16.
    #[test]
    fn test_v17_migration_down() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        let v17 = MIGRATIONS.iter().find(|m| m.version == 17).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v17.down_sql).unwrap();
        tx.commit().unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 16,
            "schema_version must revert to 16 after v17 down migration"
        );

        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='learning_supersessions'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !table_exists,
            "learning_supersessions table must be removed after v17 down migration"
        );

        for idx in ["idx_supersessions_old", "idx_supersessions_new"] {
            let idx_exists: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='{idx}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                !idx_exists,
                "{idx} must be removed after v17 down migration"
            );
        }
    }
}
