//! Tests for the database migration system.

use super::*;
use crate::db::{create_schema, open_connection};
use tempfile::TempDir;

fn setup_db() -> (TempDir, Connection) {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    (temp_dir, conn)
}

#[test]
fn test_get_schema_version_pre_migration() {
    let (_temp_dir, conn) = setup_db();

    // Fresh database has version 0
    let version = get_schema_version(&conn).unwrap();
    assert_eq!(version, 0);
}

#[test]
fn test_run_migrations_from_fresh_db() {
    let (_temp_dir, mut conn) = setup_db();

    // Run all migrations
    let result = run_migrations(&mut conn).unwrap();

    assert_eq!(result.from_version, 0);
    assert_eq!(result.to_version, CURRENT_SCHEMA_VERSION);
    assert!(!result.applied.is_empty());

    // Verify version was updated
    let version = get_schema_version(&conn).unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn test_run_migrations_idempotent() {
    let (_temp_dir, mut conn) = setup_db();

    // Run migrations twice
    run_migrations(&mut conn).unwrap();
    let result = run_migrations(&mut conn).unwrap();

    // Second run should apply nothing
    assert!(result.applied.is_empty());
    assert_eq!(result.from_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(result.to_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn test_migrate_up_single() {
    let (_temp_dir, mut conn) = setup_db();

    // Migrate up one step at a time
    let result = migrate_up(&mut conn).unwrap();

    assert_eq!(result.from_version, 0);
    assert_eq!(result.to_version, 1);
    assert_eq!(result.applied.len(), 1);
}

#[test]
fn test_migrate_down_single() {
    let (_temp_dir, mut conn) = setup_db();

    // First apply migrations
    run_migrations(&mut conn).unwrap();

    // Then migrate down
    let result = migrate_down(&mut conn).unwrap();

    assert_eq!(result.from_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(result.to_version, CURRENT_SCHEMA_VERSION - 1);
    assert_eq!(result.applied.len(), 1);
}

#[test]
fn test_migrate_down_from_zero() {
    let (_temp_dir, mut conn) = setup_db();

    // Try to migrate down from version 0
    let result = migrate_down(&mut conn).unwrap();

    // Should be a no-op
    assert_eq!(result.from_version, 0);
    assert_eq!(result.to_version, 0);
    assert!(result.applied.is_empty());
}

#[test]
fn test_migration_status_fresh_db() {
    let (_temp_dir, conn) = setup_db();

    let status = get_migration_status(&conn).unwrap();

    assert_eq!(status.current_version, 0);
    assert_eq!(status.target_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(status.pending_count, MIGRATIONS.len());
    assert!(status.applied.is_empty());
    assert_eq!(status.pending.len(), MIGRATIONS.len());
}

#[test]
fn test_migration_status_after_migrations() {
    let (_temp_dir, mut conn) = setup_db();

    run_migrations(&mut conn).unwrap();

    let status = get_migration_status(&conn).unwrap();

    assert_eq!(status.current_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(status.target_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(status.pending_count, 0);
    assert_eq!(status.applied.len(), MIGRATIONS.len());
    assert!(status.pending.is_empty());
}

#[test]
fn test_migration_adds_schema_version_column() {
    let (_temp_dir, mut conn) = setup_db();

    // Before migration, column doesn't exist
    let exists_before: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('global_state') WHERE name = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!exists_before);

    // Apply migrations
    run_migrations(&mut conn).unwrap();

    // After migration, column exists
    let exists_after: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('global_state') WHERE name = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(exists_after);
}

#[test]
fn test_migrate_down_removes_schema_version_column() {
    let (_temp_dir, mut conn) = setup_db();

    // Apply migrations
    run_migrations(&mut conn).unwrap();

    // Column should exist
    let exists_before: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('global_state') WHERE name = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(exists_before);

    // Revert all migrations (v2 -> v1, then v1 -> v0)
    // We have 2 migrations now, so need to migrate down twice
    for _ in 0..CURRENT_SCHEMA_VERSION {
        migrate_down(&mut conn).unwrap();
    }

    // Column should be gone
    let exists_after: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('global_state') WHERE name = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!exists_after);
}

#[test]
fn test_migration_preserves_existing_data() {
    let (_temp_dir, mut conn) = setup_db();

    // Set some data in global_state before migration
    conn.execute(
        "UPDATE global_state SET iteration_counter = 42, last_task_id = 'US-001' WHERE id = 1",
        [],
    )
    .unwrap();

    // Apply migrations
    run_migrations(&mut conn).unwrap();

    // Verify data was preserved
    let (counter, task_id): (i64, Option<String>) = conn
        .query_row(
            "SELECT iteration_counter, last_task_id FROM global_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(counter, 42);
    assert_eq!(task_id, Some("US-001".to_string()));
}

// ========== FTS5 Migration Tests ==========

#[test]
fn test_migration_creates_fts5_table() {
    let (_temp_dir, mut conn) = setup_db();

    // Before migration, FTS5 table doesn't exist
    let exists_before: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='learnings_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!exists_before);

    // Apply migrations
    run_migrations(&mut conn).unwrap();

    // After migration, FTS5 table exists
    let exists_after: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='learnings_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(exists_after);
}

#[test]
fn test_migration_creates_fts5_triggers() {
    let (_temp_dir, mut conn) = setup_db();

    // Apply migrations
    run_migrations(&mut conn).unwrap();

    // Check triggers exist
    let triggers: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'learnings_%'",
            )
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    assert!(triggers.contains(&"learnings_ai".to_string()));
    assert!(triggers.contains(&"learnings_ad".to_string()));
    assert!(triggers.contains(&"learnings_au".to_string()));
}

#[test]
fn test_fts5_migration_populates_existing_learnings() {
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();

    // Create a learning BEFORE FTS5 migration
    let params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "Existing learning".to_string(),
        content: "Content from before migration".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    record_learning(&conn, params).unwrap();

    // Apply migrations (including FTS5)
    run_migrations(&mut conn).unwrap();

    // Verify learning is in FTS index
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"Existing\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn test_fts5_migration_down_removes_table_and_triggers() {
    let (_temp_dir, mut conn) = setup_db();

    // Apply all migrations
    run_migrations(&mut conn).unwrap();

    // Verify FTS5 table and triggers exist
    let fts_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='learnings_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(fts_exists);

    // Migrate down from version 6 to version 5 (reverts prd_files)
    migrate_down(&mut conn).unwrap();
    // Migrate down from version 5 to version 4 (reverts task_prefix)
    migrate_down(&mut conn).unwrap();
    // Migrate down from version 4 to version 3 (reverts external_git_repo)
    migrate_down(&mut conn).unwrap();
    // Migrate down from version 3 to version 2 (reverts FTS5)
    migrate_down(&mut conn).unwrap();

    // Verify FTS5 table is gone
    let fts_exists_after: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='learnings_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!fts_exists_after);

    // Verify triggers are gone
    let triggers_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'learnings_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(triggers_count, 0);

    // Verify schema version is now 2
    let version = get_schema_version(&conn).unwrap();
    assert_eq!(version, 2);
}
