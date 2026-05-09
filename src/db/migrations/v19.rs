//! Migration 19: Add claims_shared_infra column to tasks for FEAT-003 buildy heuristic.
//!
//! ## Changes
//! - `tasks`: ADD COLUMN `claims_shared_infra INTEGER DEFAULT NULL`
//!
//! ## Semantics
//! - `NULL` (default) — fall through to implicit-overlap path detection and the
//!   buildy-prefix heuristic in `select_parallel_group`.
//! - `1` — task explicitly forces the synthetic shared-infra slot claim, even if
//!   its files / id would not otherwise trigger detection.
//! - `0` — task explicitly opts OUT of the buildy heuristic and any implicit
//!   path-based shared-infra claim. Useful for FEAT/REFACTOR tasks the operator
//!   knows are safe to parallelize against another buildy task.

use super::Migration;

/// Migration 19: Add claims_shared_infra column to tasks.
pub static MIGRATION: Migration = Migration {
    version: 19,
    description: "Add claims_shared_infra column to tasks",
    up_sql: r#"
        ALTER TABLE tasks ADD COLUMN claims_shared_infra INTEGER DEFAULT NULL;
        UPDATE global_state SET schema_version = 19 WHERE id = 1;
    "#,
    // DROP COLUMN requires SQLite >= 3.35.0. rusqlite 0.31 bundles SQLite 3.45+.
    down_sql: r#"
        ALTER TABLE tasks DROP COLUMN claims_shared_infra;
        UPDATE global_state SET schema_version = 18 WHERE id = 1;
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

    #[test]
    fn test_v19_schema_version_is_at_least_19() {
        let (_temp_dir, conn) = setup_migrated_db();
        const _: () = assert!(
            CURRENT_SCHEMA_VERSION >= 19,
            "CURRENT_SCHEMA_VERSION must be at least 19"
        );
        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 19,
            "DB schema_version must be at least 19 after migration"
        );
    }

    #[test]
    fn test_v19_tasks_claims_shared_infra_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        assert!(
            column_exists(&conn, "tasks", "claims_shared_infra"),
            "tasks.claims_shared_infra column must exist after v19 migration"
        );
    }

    #[test]
    fn test_v19_claims_shared_infra_defaults_to_null() {
        let (_temp_dir, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'Test Task', 'todo')",
            [],
        )
        .unwrap();
        let value: Option<i64> = conn
            .query_row(
                "SELECT claims_shared_infra FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            value, None,
            "tasks.claims_shared_infra must default to NULL"
        );
    }

    #[test]
    fn test_v19_claims_shared_infra_accepts_zero_and_one() {
        let (_temp_dir, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, claims_shared_infra) VALUES ('A', 'A', 'todo', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status, claims_shared_infra) VALUES ('B', 'B', 'todo', 0)",
            [],
        )
        .unwrap();

        let a: i64 = conn
            .query_row(
                "SELECT claims_shared_infra FROM tasks WHERE id = 'A'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let b: i64 = conn
            .query_row(
                "SELECT claims_shared_infra FROM tasks WHERE id = 'B'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a, 1);
        assert_eq!(b, 0);
    }

    #[test]
    fn test_v19_migration_down_removes_column_and_reverts_to_v18() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        let v19 = MIGRATIONS.iter().find(|m| m.version == 19).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v19.down_sql).unwrap();
        tx.commit().unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 18,
            "schema_version must revert to 18 after v19 down migration"
        );

        assert!(
            !column_exists(&conn, "tasks", "claims_shared_infra"),
            "tasks.claims_shared_infra must be removed after v19 down migration"
        );
    }
}
