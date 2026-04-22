//! Migration 18: Add slot column to run_tasks for parallel execution tracking.
//!
//! ## Changes
//! - ALTER TABLE run_tasks ADD COLUMN slot INTEGER NOT NULL DEFAULT 0
//!
//! ## Semantics
//! - slot=0 is the main (sequential) slot; slot>0 identifies a parallel worker slot.
//! - Existing rows get slot=0 via the DEFAULT, preserving backward compatibility.
//! - SQLite cannot drop columns, so the down migration only reverts schema_version.

use super::Migration;

/// Migration 18: Add slot column to run_tasks.
pub static MIGRATION: Migration = Migration {
    version: 18,
    description: "Add slot column to run_tasks for parallel execution tracking",
    up_sql: r#"
        ALTER TABLE run_tasks ADD COLUMN slot INTEGER NOT NULL DEFAULT 0;
        UPDATE global_state SET schema_version = 18 WHERE id = 1;
    "#,
    down_sql: r#"
        UPDATE global_state SET schema_version = 17 WHERE id = 1;
    "#,
};

#[cfg(test)]
mod tests {
    use crate::db::migrations::{MIGRATIONS, get_schema_version, migrate_down, run_migrations};
    use crate::db::{create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_migrated_db() -> (TempDir, rusqlite::Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    /// AC: schema_version is >= 18 after running all migrations.
    #[test]
    fn test_v18_schema_version() {
        let (_temp_dir, conn) = setup_migrated_db();

        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 18,
            "schema_version must be >= 18 after running migrations, got {version}"
        );
    }

    /// AC: slot column exists on run_tasks with correct default.
    #[test]
    fn test_v18_slot_column_exists_with_default() {
        let (_temp_dir, conn) = setup_migrated_db();

        // Column must exist
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('run_tasks') WHERE name = 'slot'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "run_tasks.slot column must exist after v18 migration"
        );

        // Insert a run_tasks row without specifying slot — must default to 0
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('r1', 'active')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('T-001', 'Test', 'todo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('r1', 'T-001', 1)",
            [],
        )
        .unwrap();

        let slot: i64 = conn
            .query_row(
                "SELECT slot FROM run_tasks WHERE run_id = 'r1' AND task_id = 'T-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(slot, 0, "slot must default to 0 when not specified");
    }

    /// AC: slot column is writable with non-zero values.
    #[test]
    fn test_v18_slot_column_writable() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('r2', 'active')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('T-002', 'Test2', 'todo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, iteration, slot) VALUES ('r2', 'T-002', 1, 3)",
            [],
        )
        .unwrap();

        let slot: i64 = conn
            .query_row(
                "SELECT slot FROM run_tasks WHERE run_id = 'r2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(slot, 3, "slot must store the provided value");
    }

    /// AC: down migration reverts schema_version to 17.
    #[test]
    fn test_v18_migration_down() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        assert!(
            MIGRATIONS.iter().any(|m| m.version == 18),
            "v18 must be registered in MIGRATIONS"
        );

        migrate_down(&mut conn).unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 17,
            "schema_version must revert to 17 after v18 down migration"
        );

        // Column still exists (SQLite cannot drop columns), which is expected
        let col_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('run_tasks') WHERE name = 'slot'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            col_exists,
            "slot column remains after down migration (SQLite cannot drop columns)"
        );
    }
}
