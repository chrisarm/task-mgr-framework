//! Migration 21: Multi-model embedding rows (composite PK).
//!
//! ## Changes
//! - Rebuilds `learning_embeddings` so the primary key is `(learning_id, model)`
//!   instead of `learning_id` alone.
//!
//! ## Semantics
//! - One row per (learning, embedding model). Catalog models (Jina, Nemotron, …)
//!   and raw `embeddingModel` strings can coexist for the same learning.
//! - `INSERT OR REPLACE` / re-embed for model A never deletes rows for model B.
//! - Active recall/curate paths still filter by the configured model only.
//! - Switching profiles needs only a gap-fill embed for the newly active model
//!   (`task-mgr curate embed` without `--force`), not a full corpus re-embed of
//!   learnings that already have vectors for that model.
//! - `ON DELETE CASCADE` from `learnings` still removes all model rows for a
//!   deleted learning.
//!
//! ## Down
//! - Collapses back to one row per learning (keeps the newest `created_at` per
//!   `learning_id`; created_at ties broken by lexicographically greatest
//!   `model`) and restores the v15 PK shape. The `created_at || '|' || model`
//!   concat key is safe because created_at always comes from the column
//!   default `datetime('now')` (fixed-width, never contains '|').

use super::Migration;

/// Migration 21: composite PK for multi-model embedding storage.
pub static MIGRATION: Migration = Migration {
    version: 21,
    description: "Multi-model learning_embeddings (PK learning_id + model)",
    up_sql: r#"
        CREATE TABLE learning_embeddings_new (
            learning_id INTEGER NOT NULL REFERENCES learnings(id) ON DELETE CASCADE,
            model TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            embedding BLOB NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (learning_id, model)
        );
        INSERT INTO learning_embeddings_new (
            learning_id, model, dimensions, embedding, created_at
        )
        SELECT learning_id, model, dimensions, embedding, created_at
        FROM learning_embeddings;
        DROP TABLE learning_embeddings;
        ALTER TABLE learning_embeddings_new RENAME TO learning_embeddings;
        CREATE INDEX idx_learning_embeddings_model ON learning_embeddings(model);
        UPDATE global_state SET schema_version = 21 WHERE id = 1;
    "#,
    down_sql: r#"
        CREATE TABLE learning_embeddings_new (
            learning_id INTEGER PRIMARY KEY REFERENCES learnings(id) ON DELETE CASCADE,
            model TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            embedding BLOB NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        -- One row per learning: newest created_at wins; on a created_at tie the
        -- lexicographically greatest model wins (MAX over the concat key).
        INSERT INTO learning_embeddings_new (
            learning_id, model, dimensions, embedding, created_at
        )
        SELECT le.learning_id, le.model, le.dimensions, le.embedding, le.created_at
        FROM learning_embeddings le
        INNER JOIN (
            SELECT learning_id,
                   MAX(created_at || '|' || model) AS pick
            FROM learning_embeddings
            GROUP BY learning_id
        ) best
          ON best.learning_id = le.learning_id
         AND (le.created_at || '|' || le.model) = best.pick;
        DROP TABLE learning_embeddings;
        ALTER TABLE learning_embeddings_new RENAME TO learning_embeddings;
        CREATE INDEX idx_learning_embeddings_model ON learning_embeddings(model);
        UPDATE global_state SET schema_version = 20 WHERE id = 1;
    "#,
};

#[cfg(test)]
mod tests {
    use crate::db::migrations::{
        CURRENT_SCHEMA_VERSION, MIGRATIONS, get_schema_version, run_migrations,
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

    fn pk_columns(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM pragma_table_info('learning_embeddings')
                 WHERE pk > 0
                 ORDER BY pk ASC",
            )
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn test_v21_schema_version() {
        let (_temp_dir, conn) = setup_migrated_db();
        const _: () = assert!(
            CURRENT_SCHEMA_VERSION >= 21,
            "CURRENT_SCHEMA_VERSION must be at least 21"
        );
        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 21,
            "DB schema_version must be at least 21 after migration, got {version}"
        );
    }

    #[test]
    fn test_v21_composite_primary_key() {
        let (_temp_dir, conn) = setup_migrated_db();
        let cols = pk_columns(&conn);
        assert_eq!(
            cols,
            vec!["learning_id".to_string(), "model".to_string()],
            "PK must be (learning_id, model)"
        );
    }

    #[test]
    fn test_v21_two_models_coexist_for_same_learning() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO learnings (id, title, content, outcome)
             VALUES (1, 'Test', 'content', 'pattern')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO learning_embeddings (learning_id, model, dimensions, embedding)
             VALUES (1, 'model-a', 2, X'0000803F00000040')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO learning_embeddings (learning_id, model, dimensions, embedding)
             VALUES (1, 'model-b', 3, X'0000803F0000004000004040')",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_embeddings WHERE learning_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "both model rows must coexist");

        // Same-model replace must not touch the other model.
        conn.execute(
            "INSERT OR REPLACE INTO learning_embeddings
             (learning_id, model, dimensions, embedding)
             VALUES (1, 'model-a', 2, X'0000004000008040')",
            [],
        )
        .unwrap();

        let count_a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_embeddings
                 WHERE learning_id = 1 AND model = 'model-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let count_b: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_embeddings
                 WHERE learning_id = 1 AND model = 'model-b'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count_a, 1);
        assert_eq!(count_b, 1, "model-b row must survive model-a replace");
    }

    #[test]
    fn test_v21_cascade_deletes_all_model_rows() {
        let (_temp_dir, conn) = setup_migrated_db();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();

        conn.execute(
            "INSERT INTO learnings (id, title, content, outcome)
             VALUES (1, 'Test', 'content', 'pattern')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO learning_embeddings (learning_id, model, dimensions, embedding)
             VALUES (1, 'model-a', 1, X'0000803F')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO learning_embeddings (learning_id, model, dimensions, embedding)
             VALUES (1, 'model-b', 1, X'00000040')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM learnings WHERE id = 1", [])
            .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_embeddings WHERE learning_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_v21_migration_down_collapses_to_one_row_per_learning() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO learnings (id, title, content, outcome)
             VALUES (1, 'Test', 'content', 'pattern')",
            [],
        )
        .unwrap();
        // Older then newer (lexicographic datetime works for ISO-ish sqlite now).
        conn.execute(
            "INSERT INTO learning_embeddings
             (learning_id, model, dimensions, embedding, created_at)
             VALUES (1, 'old-model', 1, X'0000803F', '2020-01-01 00:00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO learning_embeddings
             (learning_id, model, dimensions, embedding, created_at)
             VALUES (1, 'new-model', 1, X'00000040', '2024-06-01 00:00:00')",
            [],
        )
        .unwrap();

        let v21 = MIGRATIONS.iter().find(|m| m.version == 21).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v21.down_sql).unwrap();
        tx.commit().unwrap();

        assert_eq!(get_schema_version(&conn).unwrap(), 20);
        assert_eq!(
            pk_columns(&conn),
            vec!["learning_id".to_string()],
            "down must restore learning_id-only PK"
        );

        let (model, count): (String, i64) = conn
            .query_row(
                "SELECT model, (SELECT COUNT(*) FROM learning_embeddings)
                 FROM learning_embeddings WHERE learning_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(model, "new-model", "newest created_at row must be kept");
    }

    #[test]
    fn test_v21_up_preserves_existing_rows() {
        // Stop just before v21, insert a row, then apply v21 and confirm survival.
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();

        for m in MIGRATIONS.iter().filter(|m| m.version <= 20) {
            let tx = conn.transaction().unwrap();
            tx.execute_batch(m.up_sql).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(get_schema_version(&conn).unwrap(), 20);

        conn.execute(
            "INSERT INTO learnings (id, title, content, outcome)
             VALUES (7, 'Keep me', 'body', 'pattern')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO learning_embeddings (learning_id, model, dimensions, embedding)
             VALUES (7, 'jina-like', 2, X'0000803F00000040')",
            [],
        )
        .unwrap();

        let v21 = MIGRATIONS.iter().find(|m| m.version == 21).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v21.up_sql).unwrap();
        tx.commit().unwrap();

        let (model, dims): (String, i64) = conn
            .query_row(
                "SELECT model, dimensions FROM learning_embeddings WHERE learning_id = 7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(model, "jina-like");
        assert_eq!(dims, 2);
        assert_eq!(
            pk_columns(&conn),
            vec!["learning_id".to_string(), "model".to_string()]
        );
    }
}
