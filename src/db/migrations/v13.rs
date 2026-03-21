//! Migration 13: Add per-task retry tracking columns for loop reliability.
//!
//! ## Changes (implemented by FEAT-001)
//! - `tasks`: ADD COLUMN `max_retries INTEGER NOT NULL DEFAULT 3`
//! - `tasks`: ADD COLUMN `consecutive_failures INTEGER NOT NULL DEFAULT 0`
//! - `prd_metadata`: ADD COLUMN `default_max_retries INTEGER`
//!
//! ## Semantics
//! - `max_retries = 0` disables auto-blocking (task retries indefinitely)
//! - `consecutive_failures` resets to 0 on any successful iteration
//! - Per-task `max_retries` overrides PRD-level `default_max_retries`
//! - `default_max_retries` on `prd_metadata` sets the PRD-wide default (itself defaulting to 3)

use super::Migration;

/// Migration 13: Add per-task retry tracking columns.
///
/// NOTE: This is a stub migration that only bumps the schema version.
/// The actual ALTER TABLE statements are deferred to FEAT-001.
pub static MIGRATION: Migration = Migration {
    version: 13,
    description: "Add max_retries and consecutive_failures for per-task retry limits",
    // TODO(FEAT-001): Replace with actual ALTER TABLE statements:
    //   ALTER TABLE tasks ADD COLUMN max_retries INTEGER NOT NULL DEFAULT 3;
    //   ALTER TABLE tasks ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0;
    //   ALTER TABLE prd_metadata ADD COLUMN default_max_retries INTEGER;
    up_sql: r#"
        -- Placeholder: schema changes deferred to FEAT-001
        UPDATE global_state SET schema_version = 13 WHERE id = 1;
    "#,
    // TODO(FEAT-001): SQLite pre-3.35 doesn't support DROP COLUMN directly.
    // The down migration will need to recreate tasks and prd_metadata without the new columns.
    down_sql: r#"
        -- Placeholder: column removal deferred to FEAT-001
        UPDATE global_state SET schema_version = 12 WHERE id = 1;
    "#,
};

#[cfg(test)]
mod tests {
    use crate::db::migrations::{
        get_schema_version, migrate_down, migrate_up, run_migrations, CURRENT_SCHEMA_VERSION,
        MIGRATIONS,
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

    /// Schema version must be 13 after full migration run.
    /// This is the ONE active test — all others are #[ignore] pending FEAT-001.
    #[test]
    fn test_v13_current_schema_version_is_13() {
        let (_temp_dir, conn) = setup_migrated_db();
        assert_eq!(
            CURRENT_SCHEMA_VERSION, 13,
            "CURRENT_SCHEMA_VERSION constant must be 13"
        );
        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, 13, "DB schema_version must be 13 after migration");
    }

    // =========================================================================
    // Tests below verify what FEAT-001 must implement.
    // They are #[ignore] because the stub migration doesn't add DB columns yet.
    // Remove #[ignore] once FEAT-001 implements the full ALTER TABLE statements.
    // =========================================================================

    /// After v13 up, `tasks.max_retries` column must exist.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_tasks_max_retries_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('tasks') WHERE name = 'max_retries'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "tasks.max_retries column must exist after v13 migration"
        );
    }

    /// After v13 up, `tasks.consecutive_failures` column must exist.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_tasks_consecutive_failures_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('tasks') WHERE name = 'consecutive_failures'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "tasks.consecutive_failures column must exist after v13 migration"
        );
    }

    /// After v13 up, `prd_metadata.default_max_retries` column must exist.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_prd_metadata_default_max_retries_column_exists() {
        let (_temp_dir, conn) = setup_migrated_db();

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('prd_metadata') WHERE name = 'default_max_retries'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "prd_metadata.default_max_retries column must exist after v13 migration"
        );
    }

    /// New tasks inserted after v13 must default to max_retries=3.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_tasks_max_retries_default_is_3() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'Test Task', 'todo')",
            [],
        )
        .unwrap();

        let max_retries: i64 = conn
            .query_row(
                "SELECT max_retries FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(max_retries, 3, "tasks.max_retries must default to 3");
    }

    /// New tasks inserted after v13 must default to consecutive_failures=0.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_tasks_consecutive_failures_default_is_0() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'Test Task', 'todo')",
            [],
        )
        .unwrap();

        let consecutive_failures: i64 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            consecutive_failures, 0,
            "tasks.consecutive_failures must default to 0"
        );
    }

    /// Pre-existing tasks (inserted before v13) must have max_retries=3 after migration.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_existing_tasks_get_default_max_retries() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();

        // Insert a task at v12 (before v13 adds the column)
        for _ in 0..12 {
            migrate_up(&mut conn).unwrap();
        }
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES ('US-OLD', 'Old Task', 'todo')",
            [],
        )
        .unwrap();

        // Apply v13
        migrate_up(&mut conn).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), 13);

        // Existing task must now have max_retries=3 (column default backfill)
        let max_retries: i64 = conn
            .query_row(
                "SELECT max_retries FROM tasks WHERE id = 'US-OLD'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            max_retries, 3,
            "Pre-existing tasks must get max_retries=3 after v13 migration"
        );
    }

    /// max_retries=0 is valid and means auto-blocking is disabled.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_max_retries_zero_is_valid() {
        let (_temp_dir, conn) = setup_migrated_db();

        // max_retries=0 disables auto-block — must be writable
        conn.execute(
            "INSERT INTO tasks (id, title, status, max_retries) VALUES ('US-NOLIMIT', 'No Limit Task', 'todo', 0)",
            [],
        )
        .unwrap();

        let max_retries: i64 = conn
            .query_row(
                "SELECT max_retries FROM tasks WHERE id = 'US-NOLIMIT'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            max_retries, 0,
            "max_retries=0 must be writable (disables auto-block)"
        );
    }

    /// consecutive_failures can be written and read back.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_consecutive_failures_writable() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('US-FAIL', 'Failing Task', 'todo', 2)",
            [],
        )
        .unwrap();

        let failures: i64 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'US-FAIL'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failures, 2, "consecutive_failures must be writable");
    }

    /// Per-task max_retries is independent of any other task's consecutive_failures.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_consecutive_failures_is_per_task() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('US-A', 'Task A', 'todo', 5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('US-B', 'Task B', 'todo', 0)",
            [],
        )
        .unwrap();

        let (fa, fb): (i64, i64) = conn
            .query_row(
                "SELECT
                    (SELECT consecutive_failures FROM tasks WHERE id = 'US-A'),
                    (SELECT consecutive_failures FROM tasks WHERE id = 'US-B')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(fa, 5, "US-A consecutive_failures must be independent");
        assert_eq!(fb, 0, "US-B consecutive_failures must be independent");
    }

    /// prd_metadata.default_max_retries accepts NULL (no override).
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_prd_metadata_default_max_retries_nullable() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO prd_metadata (id, project) VALUES (1, 'test-prd')",
            [],
        )
        .unwrap();

        let default_max_retries: Option<i64> = conn
            .query_row(
                "SELECT default_max_retries FROM prd_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        // NULL means "use the system default of 3"
        assert_eq!(
            default_max_retries, None,
            "prd_metadata.default_max_retries must default to NULL"
        );
    }

    /// prd_metadata.default_max_retries can be set to override the system default.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_prd_metadata_default_max_retries_writable() {
        let (_temp_dir, conn) = setup_migrated_db();

        conn.execute(
            "INSERT INTO prd_metadata (id, project, default_max_retries) VALUES (1, 'test-prd', 5)",
            [],
        )
        .unwrap();

        let default_max_retries: Option<i64> = conn
            .query_row(
                "SELECT default_max_retries FROM prd_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(
            default_max_retries,
            Some(5),
            "prd_metadata.default_max_retries=5 must be writable"
        );
    }

    /// v13 down migration must revert schema to version 12.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_migration_down_reverts_to_v12() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), 13);

        // Run v13 down migration
        migrate_down(&mut conn).unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 12,
            "schema_version must revert to 12 after v13 down"
        );
    }

    /// v13 down migration must remove max_retries from tasks.
    #[test]
    #[ignore = "FEAT-001: stub migration doesn't add DB columns yet"]
    fn test_v13_migration_down_removes_tasks_columns() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        // Apply v13 down
        let v13 = MIGRATIONS.iter().find(|m| m.version == 13).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v13.down_sql).unwrap();
        tx.commit().unwrap();

        // max_retries column must not exist after downgrade
        let max_retries_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('tasks') WHERE name = 'max_retries'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        assert!(
            !max_retries_exists,
            "tasks.max_retries must be removed after v13 down"
        );
    }

    /// Running migrations twice on the same DB is idempotent (no errors).
    #[test]
    fn test_v13_migration_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();

        run_migrations(&mut conn).unwrap();
        let result = run_migrations(&mut conn).unwrap();

        // Second run applies no migrations
        assert!(
            result.applied.is_empty(),
            "Second migration run must be a no-op"
        );
        assert_eq!(get_schema_version(&conn).unwrap(), 13);
    }
}
