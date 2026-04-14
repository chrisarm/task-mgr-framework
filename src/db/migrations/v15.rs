//! Migration 15: Create learning_embeddings table for vector similarity search.
//!
//! ## Changes
//! - Creates `learning_embeddings` table with:
//!   - `learning_id` INTEGER PRIMARY KEY (FK to learnings.id, ON DELETE CASCADE)
//!   - `model` TEXT NOT NULL — embedding model name
//!   - `dimensions` INTEGER NOT NULL — vector dimensionality
//!   - `embedding` BLOB NOT NULL — little-endian f32 vector
//!   - `created_at` TEXT NOT NULL DEFAULT (datetime('now'))
//!
//! ## Semantics
//! - One embedding per learning (learning_id is PK, not just FK)
//! - Re-embedding replaces via INSERT OR REPLACE
//! - Cascade delete removes embeddings when learning is deleted

use super::Migration;

/// Migration 15: Create learning_embeddings table.
pub static MIGRATION: Migration = Migration {
    version: 15,
    description: "Create learning_embeddings table for vector similarity search",
    up_sql: r#"
        CREATE TABLE learning_embeddings (
            learning_id INTEGER PRIMARY KEY REFERENCES learnings(id) ON DELETE CASCADE,
            model TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            embedding BLOB NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX idx_learning_embeddings_model ON learning_embeddings(model);
        UPDATE global_state SET schema_version = 15 WHERE id = 1;
    "#,
    down_sql: r#"
        DROP INDEX IF EXISTS idx_learning_embeddings_model;
        DROP TABLE IF EXISTS learning_embeddings;
        UPDATE global_state SET schema_version = 14 WHERE id = 1;
    "#,
};

#[cfg(test)]
mod tests {
    use crate::db::migrations::{
        get_schema_version, run_migrations, CURRENT_SCHEMA_VERSION, MIGRATIONS,
    };
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

    /// After running all migrations, v15 artifacts must be present.
    #[test]
    fn test_v15_migration_was_applied() {
        let (_temp_dir, conn) = setup_migrated_db();
        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 15,
            "DB schema_version must be >= 15 after running migrations"
        );
    }

    /// learning_embeddings table must exist with correct columns.
    #[test]
    fn test_v15_learning_embeddings_table_exists() {
        let (_temp_dir, conn) = setup_migrated_db();

        let expected_columns = [
            "learning_id",
            "model",
            "dimensions",
            "embedding",
            "created_at",
        ];

        for col in expected_columns {
            let exists: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) > 0 FROM pragma_table_info('learning_embeddings') WHERE name = '{col}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                exists,
                "learning_embeddings.{col} column must exist after v15 migration"
            );
        }
    }

    /// Index on model column must exist.
    #[test]
    fn test_v15_model_index_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_learning_embeddings_model'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "idx_learning_embeddings_model index must exist after v15 migration"
        );
    }

    /// INSERT OR REPLACE must work for re-embedding.
    #[test]
    fn test_v15_insert_or_replace_works() {
        let (_temp_dir, conn) = setup_migrated_db();

        // Insert a learning first
        conn.execute(
            "INSERT INTO learnings (id, title, content, outcome) VALUES (1, 'Test', 'content', 'pattern')",
            [],
        )
        .unwrap();

        // Insert an embedding
        conn.execute(
            "INSERT INTO learning_embeddings (learning_id, model, dimensions, embedding) VALUES (1, 'test-model', 3, X'000000000000803F00000040')",
            [],
        )
        .unwrap();

        // Replace the embedding
        conn.execute(
            "INSERT OR REPLACE INTO learning_embeddings (learning_id, model, dimensions, embedding) VALUES (1, 'new-model', 4, X'000000000000803F0000004000004040')",
            [],
        )
        .unwrap();

        let model: String = conn
            .query_row(
                "SELECT model FROM learning_embeddings WHERE learning_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(model, "new-model", "INSERT OR REPLACE must update the row");
    }

    /// Cascade delete must remove embedding when learning is deleted.
    #[test]
    fn test_v15_cascade_delete() {
        let (_temp_dir, conn) = setup_migrated_db();

        // Enable foreign keys (required for CASCADE)
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();

        conn.execute(
            "INSERT INTO learnings (id, title, content, outcome) VALUES (1, 'Test', 'content', 'pattern')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO learning_embeddings (learning_id, model, dimensions, embedding) VALUES (1, 'test-model', 3, X'000000000000803F')",
            [],
        )
        .unwrap();

        // Delete the learning
        conn.execute("DELETE FROM learnings WHERE id = 1", [])
            .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_embeddings WHERE learning_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "Cascade delete must remove embedding");
    }

    /// v15 down migration must revert schema to version 14 and remove table.
    #[test]
    fn test_v15_migration_down() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        // Run v15 down migration directly
        let v15 = MIGRATIONS.iter().find(|m| m.version == 15).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v15.down_sql).unwrap();
        tx.commit().unwrap();

        // Schema version must revert to 14
        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 14,
            "schema_version must revert to 14 after v15 down migration"
        );

        // Table must be removed
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='learning_embeddings'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !exists,
            "learning_embeddings table must be removed after v15 down migration"
        );
    }
}
