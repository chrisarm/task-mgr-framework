//! Migration 20: Add provider stamping columns (tasks.completed_by_provider,
//! run_tasks.provider, run_tasks.model) — columns only, NO review_rounds table
//! (deferred to cascade PRD).
//!
//! ## Changes
//! - `tasks`: ADD COLUMN `completed_by_provider TEXT`
//! - `run_tasks`: ADD COLUMN `provider TEXT`, ADD COLUMN `model TEXT`
//!
//! ## Semantics
//! - `completed_by_provider` stores lowercase provider via `Provider::as_str`
//!   ('claude'/'grok'/'codex'), **never** a model string. Set exclusively in
//!   `process_iteration_output` completion arm (single home for sequential +
//!   wave).
//! - `run_tasks.provider` / `model` store the effective (runner, model) pair
//!   for that (run, task, iteration) attempt.
//! - Historical rows and in-flight (non-done) tasks remain NULL.
//! - Down migration follows version-only convention (learning #348): columns
//!   intentionally left; only schema_version is reverted.
//!
//! Stamping reads `effective_runner` / `effective_model` already present on
//! `IterationResult` / `SlotResult` and threaded through `ProcessingParams`.

use super::Migration;

/// Migration 20: provider stamping columns for audit + cascade prerequisite.
pub static MIGRATION: Migration = Migration {
    version: 20,
    description: "Add completed_by_provider to tasks and provider/model to run_tasks",
    up_sql: r#"
        ALTER TABLE tasks ADD COLUMN completed_by_provider TEXT;
        ALTER TABLE run_tasks ADD COLUMN provider TEXT;
        ALTER TABLE run_tasks ADD COLUMN model TEXT;
        UPDATE global_state SET schema_version = 20 WHERE id = 1;
    "#,
    // Version-only down per learning #348 (SQLite column retention convention).
    // No DROP COLUMN; columns stay (harmless for older code, NULL for historical).
    down_sql: r#"
        UPDATE global_state SET schema_version = 19 WHERE id = 1;
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
    fn test_v20_schema_version_is_at_least_20() {
        let (_temp_dir, conn) = setup_migrated_db();
        const _: () = assert!(
            CURRENT_SCHEMA_VERSION >= 20,
            "CURRENT_SCHEMA_VERSION must be at least 20"
        );
        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 20,
            "DB schema_version must be at least 20 after migration"
        );
    }

    #[test]
    fn test_v20_tasks_completed_by_provider_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();
        assert!(
            column_exists(&conn, "tasks", "completed_by_provider"),
            "tasks.completed_by_provider column must exist after v20 migration"
        );
    }

    #[test]
    fn test_v20_run_tasks_provider_and_model_columns_exist() {
        let (_temp_dir, conn) = setup_migrated_db();
        assert!(
            column_exists(&conn, "run_tasks", "provider"),
            "run_tasks.provider column must exist after v20 migration"
        );
        assert!(
            column_exists(&conn, "run_tasks", "model"),
            "run_tasks.model column must exist after v20 migration"
        );
    }

    #[test]
    fn test_v20_new_columns_default_to_null() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('T-001', 'Test Task', 'todo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-42', 'active')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, iteration, status) VALUES ('run-42', 'T-001', 3, 'started')",
            [],
        )
        .unwrap();

        let cbp: Option<String> = conn
            .query_row(
                "SELECT completed_by_provider FROM tasks WHERE id = 'T-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cbp, None, "completed_by_provider must default to NULL");

        let (prov, mdl): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT provider, model FROM run_tasks WHERE run_id = 'run-42' AND task_id = 'T-001' AND iteration = 3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(prov, None, "run_tasks.provider must default to NULL");
        assert_eq!(mdl, None, "run_tasks.model must default to NULL");
    }

    #[test]
    fn test_v20_migration_down_reverts_version_only_leaves_columns() {
        // Per learning #348: down leaves added columns; only reverts schema_version.
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        let v20 = MIGRATIONS.iter().find(|m| m.version == 20).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v20.down_sql).unwrap();
        tx.commit().unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 19,
            "schema_version must revert to 19 after v20 down migration"
        );

        // Columns must still exist (convention: no DROP in down for added columns).
        assert!(
            column_exists(&conn, "tasks", "completed_by_provider"),
            "tasks.completed_by_provider must remain after down (version-only convention)"
        );
        assert!(
            column_exists(&conn, "run_tasks", "provider"),
            "run_tasks.provider must remain after down"
        );
        assert!(
            column_exists(&conn, "run_tasks", "model"),
            "run_tasks.model must remain after down"
        );
    }
}
