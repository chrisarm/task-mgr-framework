//! Migration 12: Add key_decisions table
//!
//! Adds `key_decisions` table to store architectural decision points
//! flagged by Claude during loop iterations via `<key-decision>` XML tags.

use super::Migration;

/// Migration 12: Add key_decisions table for tracking architectural decision points
pub static MIGRATION: Migration = Migration {
    version: 12,
    description: "Add key_decisions table for tracking architectural decision points",
    up_sql: r#"
        CREATE TABLE key_decisions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL REFERENCES runs(run_id),
            task_id TEXT REFERENCES tasks(id),
            iteration INTEGER NOT NULL,
            title TEXT NOT NULL,
            description TEXT NOT NULL,
            options TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending', 'resolved', 'deferred')),
            resolution TEXT,
            resolved_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX idx_key_decisions_run_id ON key_decisions(run_id);
        CREATE INDEX idx_key_decisions_status ON key_decisions(status);

        -- Update schema version
        UPDATE global_state SET schema_version = 12 WHERE id = 1;
    "#,
    down_sql: r#"
        DROP INDEX IF EXISTS idx_key_decisions_status;
        DROP INDEX IF EXISTS idx_key_decisions_run_id;
        DROP TABLE IF EXISTS key_decisions;

        -- Update schema version back to 11
        UPDATE global_state SET schema_version = 11 WHERE id = 1;
    "#,
};

#[cfg(test)]
mod tests {
    use crate::db::migrations::{MIGRATIONS, run_migrations};
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

    #[test]
    fn test_v12_migration_up_creates_table() {
        let (_temp_dir, conn) = setup_migrated_db();

        // Insert a run to satisfy FK constraint
        conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
            .unwrap();

        // Should be able to INSERT a row into key_decisions
        conn.execute(
            "INSERT INTO key_decisions (run_id, task_id, iteration, title, description, options)
             VALUES ('run-001', NULL, 1, 'Test Decision', 'A test description', '[\"opt1\",\"opt2\"]')",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM key_decisions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_v12_migration_down_drops_table() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        // Run the v12 down migration manually
        let v12 = MIGRATIONS.iter().find(|m| m.version == 12).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v12.down_sql).unwrap();
        tx.commit().unwrap();

        // Table should no longer exist
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='key_decisions'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        assert!(!table_exists);
    }

    #[test]
    fn test_v12_options_column_is_text() {
        let (_temp_dir, conn) = setup_migrated_db();

        // options column stores JSON as TEXT (SQLite has no JSON type)
        conn.execute("INSERT INTO runs (run_id) VALUES ('run-002')", [])
            .unwrap();
        let json_value = r#"[{"label":"Option A","description":"First option"},{"label":"Option B","description":"Second option"}]"#;
        conn.execute(
            "INSERT INTO key_decisions (run_id, iteration, title, description, options)
             VALUES ('run-002', 1, 'Arch Decision', 'Choose approach', ?1)",
            [json_value],
        )
        .unwrap();

        let stored: String = conn
            .query_row(
                "SELECT options FROM key_decisions WHERE run_id = 'run-002'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, json_value);
    }
}
