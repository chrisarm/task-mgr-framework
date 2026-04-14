//! Migration 14: Add archived_at columns and indexes for soft-archive support.
//!
//! ## Changes
//! - `tasks`: ADD COLUMN `archived_at TEXT DEFAULT NULL`
//! - `runs`: ADD COLUMN `archived_at TEXT DEFAULT NULL`
//! - `run_tasks`: ADD COLUMN `archived_at TEXT DEFAULT NULL`
//! - `key_decisions`: ADD COLUMN `archived_at TEXT DEFAULT NULL`
//! - Creates index on `archived_at` for each of the four tables.
//!
//! ## Semantics
//! - `archived_at IS NULL` means the row is active (not archived)
//! - `archived_at IS NOT NULL` means the row is soft-deleted / archived
//! - Active queries must filter with `AND <table>.archived_at IS NULL`
//! - `--include-archived` flag bypasses this filter for list/history commands

use super::Migration;

/// Migration 14: Add archived_at columns and indexes for soft-archive support.
pub static MIGRATION: Migration = Migration {
    version: 14,
    description: "Add archived_at columns and indexes for soft-archive support",
    up_sql: r#"
        ALTER TABLE tasks ADD COLUMN archived_at TEXT DEFAULT NULL;
        ALTER TABLE runs ADD COLUMN archived_at TEXT DEFAULT NULL;
        ALTER TABLE run_tasks ADD COLUMN archived_at TEXT DEFAULT NULL;
        ALTER TABLE key_decisions ADD COLUMN archived_at TEXT DEFAULT NULL;
        CREATE INDEX idx_tasks_archived_at ON tasks(archived_at);
        CREATE INDEX idx_runs_archived_at ON runs(archived_at);
        CREATE INDEX idx_run_tasks_archived_at ON run_tasks(archived_at);
        CREATE INDEX idx_key_decisions_archived_at ON key_decisions(archived_at);
        UPDATE global_state SET schema_version = 14 WHERE id = 1;
    "#,
    // DROP COLUMN requires SQLite >= 3.35.0. rusqlite 0.31 bundles SQLite 3.45+.
    down_sql: r#"
        DROP INDEX IF EXISTS idx_tasks_archived_at;
        DROP INDEX IF EXISTS idx_runs_archived_at;
        DROP INDEX IF EXISTS idx_run_tasks_archived_at;
        DROP INDEX IF EXISTS idx_key_decisions_archived_at;
        ALTER TABLE tasks DROP COLUMN archived_at;
        ALTER TABLE runs DROP COLUMN archived_at;
        ALTER TABLE run_tasks DROP COLUMN archived_at;
        ALTER TABLE key_decisions DROP COLUMN archived_at;
        UPDATE global_state SET schema_version = 13 WHERE id = 1;
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

    /// Schema version must be at least 14 after v14 migration runs.
    /// (CURRENT_SCHEMA_VERSION reflects the latest migration, not v14 specifically.)
    #[test]
    fn test_v14_migration_was_applied() {
        let (_temp_dir, conn) = setup_migrated_db();
        assert!(
            CURRENT_SCHEMA_VERSION >= 14,
            "CURRENT_SCHEMA_VERSION must be >= 14"
        );
        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 14,
            "DB schema_version must be >= 14 after migrations run"
        );
    }

    /// After v14 up, tasks.archived_at column must exist.
    #[test]
    fn test_v14_tasks_archived_at_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('tasks') WHERE name = 'archived_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "tasks.archived_at column must exist after v14 migration"
        );
    }

    /// After v14 up, runs.archived_at column must exist.
    #[test]
    fn test_v14_runs_archived_at_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('runs') WHERE name = 'archived_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "runs.archived_at column must exist after v14 migration"
        );
    }

    /// After v14 up, run_tasks.archived_at column must exist.
    #[test]
    fn test_v14_run_tasks_archived_at_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('run_tasks') WHERE name = 'archived_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "run_tasks.archived_at column must exist after v14 migration"
        );
    }

    /// After v14 up, key_decisions.archived_at column must exist.
    #[test]
    fn test_v14_key_decisions_archived_at_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('key_decisions') WHERE name = 'archived_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "key_decisions.archived_at column must exist after v14 migration"
        );
    }

    /// New tasks inserted after v14 must have archived_at = NULL by default.
    #[test]
    fn test_v14_archived_at_defaults_to_null_on_tasks() {
        let (_temp_dir, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'Test Task', 'todo')",
            [],
        )
        .unwrap();
        let archived_at: Option<String> = conn
            .query_row(
                "SELECT archived_at FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archived_at, None, "tasks.archived_at must default to NULL");
    }

    /// All 4 archived_at indexes must exist in sqlite_master after v14 migration.
    #[test]
    fn test_v14_all_archived_at_indexes_exist() {
        let (_temp_dir, conn) = setup_migrated_db();

        let expected_indexes = [
            "idx_tasks_archived_at",
            "idx_runs_archived_at",
            "idx_run_tasks_archived_at",
            "idx_key_decisions_archived_at",
        ];

        for index_name in expected_indexes {
            let sql = format!(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='{index_name}'"
            );
            let exists: bool = conn.query_row(&sql, [], |row| row.get(0)).unwrap_or(false);
            assert!(
                exists,
                "Index {index_name} must exist in sqlite_master after v14 migration"
            );
        }
    }

    /// v14 down migration must revert schema to version 13 and remove archived_at columns.
    #[test]
    fn test_v14_migration_down_removes_columns_and_reverts_to_v13() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        // Run v14 down migration directly (like v12 pattern — avoids dependency on migrate_down)
        let v14 = MIGRATIONS.iter().find(|m| m.version == 14).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v14.down_sql).unwrap();
        tx.commit().unwrap();

        // Schema version must revert to 13
        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 13,
            "schema_version must revert to 13 after v14 down migration"
        );

        // archived_at columns must be removed from all 4 tables
        for table in ["tasks", "runs", "run_tasks", "key_decisions"] {
            let sql = format!(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('{table}') WHERE name = 'archived_at'"
            );
            let exists: bool = conn.query_row(&sql, [], |row| row.get(0)).unwrap_or(false);
            assert!(
                !exists,
                "{table}.archived_at must be removed after v14 down migration"
            );
        }
    }
}
