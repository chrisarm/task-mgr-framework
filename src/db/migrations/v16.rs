//! Migration 16: Add requires_human and human_review_timeout columns to tasks.
//!
//! ## Changes
//! - `tasks`: ADD COLUMN `requires_human INTEGER DEFAULT 0`
//! - `tasks`: ADD COLUMN `human_review_timeout INTEGER DEFAULT NULL`
//!
//! ## Semantics
//! - `requires_human = 1` means the loop must pause after this task completes for human input
//! - `requires_human = 0` (default) means the loop runs through without pause
//! - `human_review_timeout` is an optional wait timeout in seconds (NULL = no timeout)

use super::Migration;

/// Migration 16: Add human review fields to tasks.
pub static MIGRATION: Migration = Migration {
    version: 16,
    description: "Add requires_human and human_review_timeout columns to tasks",
    up_sql: r#"
        ALTER TABLE tasks ADD COLUMN requires_human INTEGER DEFAULT 0;
        ALTER TABLE tasks ADD COLUMN human_review_timeout INTEGER DEFAULT NULL;
        UPDATE global_state SET schema_version = 16 WHERE id = 1;
    "#,
    // DROP COLUMN requires SQLite >= 3.35.0. rusqlite 0.31 bundles SQLite 3.45+.
    down_sql: r#"
        ALTER TABLE tasks DROP COLUMN requires_human;
        ALTER TABLE tasks DROP COLUMN human_review_timeout;
        UPDATE global_state SET schema_version = 15 WHERE id = 1;
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

    fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
        let sql = format!("SELECT COUNT(*) > 0 FROM pragma_table_info('{table}') WHERE name = ?");
        conn.query_row(&sql, [column], |row| row.get(0))
            .unwrap_or(false)
    }

    /// Schema version must be at least 16 after full migration run.
    #[test]
    fn test_v16_schema_version_is_16() {
        let (_temp_dir, conn) = setup_migrated_db();
        assert!(
            CURRENT_SCHEMA_VERSION >= 16,
            "CURRENT_SCHEMA_VERSION must be at least 16"
        );
        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 16,
            "DB schema_version must be at least 16 after migration"
        );
    }

    /// After v16 up, tasks.requires_human column must exist with INTEGER type.
    #[test]
    fn test_v16_tasks_requires_human_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        assert!(
            column_exists(&conn, "tasks", "requires_human"),
            "tasks.requires_human column must exist after v16 migration"
        );
    }

    /// After v16 up, tasks.human_review_timeout column must exist.
    #[test]
    fn test_v16_tasks_human_review_timeout_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        assert!(
            column_exists(&conn, "tasks", "human_review_timeout"),
            "tasks.human_review_timeout column must exist after v16 migration"
        );
    }

    /// New tasks inserted after v16 must have requires_human = 0 by default.
    #[test]
    fn test_v16_requires_human_defaults_to_zero() {
        let (_temp_dir, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'Test Task', 'todo')",
            [],
        )
        .unwrap();
        let requires_human: i32 = conn
            .query_row(
                "SELECT requires_human FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(requires_human, 0, "tasks.requires_human must default to 0");
    }

    /// New tasks inserted after v16 must have human_review_timeout = NULL by default.
    #[test]
    fn test_v16_human_review_timeout_defaults_to_null() {
        let (_temp_dir, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'Test Task', 'todo')",
            [],
        )
        .unwrap();
        let timeout: Option<i64> = conn
            .query_row(
                "SELECT human_review_timeout FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            timeout, None,
            "tasks.human_review_timeout must default to NULL"
        );
    }

    /// v16 down migration must revert schema to version 15 and remove new columns.
    #[test]
    fn test_v16_migration_down_removes_columns_and_reverts_to_v15() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        // Run v16 down migration directly (same pattern as v14 test)
        let v16 = MIGRATIONS.iter().find(|m| m.version == 16).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v16.down_sql).unwrap();
        tx.commit().unwrap();

        // Schema version must revert to 15
        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 15,
            "schema_version must revert to 15 after v16 down migration"
        );

        // requires_human column must be removed
        assert!(
            !column_exists(&conn, "tasks", "requires_human"),
            "tasks.requires_human must be removed after v16 down migration"
        );

        // human_review_timeout column must be removed
        assert!(
            !column_exists(&conn, "tasks", "human_review_timeout"),
            "tasks.human_review_timeout must be removed after v16 down migration"
        );
    }
}
