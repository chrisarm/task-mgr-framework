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

    // Migrate down from version 10 to version 9 (reverts retired_at column)
    migrate_down(&mut conn).unwrap();
    // Migrate down from version 9 to version 8 (reverts prd_metadata singleton removal)
    migrate_down(&mut conn).unwrap();
    // Migrate down from version 8 to version 7 (reverts FTS5 tag indexing stub)
    migrate_down(&mut conn).unwrap();
    // Migrate down from version 7 to version 6 (reverts model selection fields)
    migrate_down(&mut conn).unwrap();
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

// ========== Migration v7 Tests (Model Selection Fields) ==========

#[test]
fn test_migration_v7_adds_model_columns_to_tasks() {
    let (_temp_dir, mut conn) = setup_db();

    run_migrations(&mut conn).unwrap();

    // Verify model, difficulty, escalation_note columns exist on tasks
    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('tasks') WHERE name IN ('model', 'difficulty', 'escalation_note')")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    assert!(
        columns.contains(&"model".to_string()),
        "tasks.model column missing"
    );
    assert!(
        columns.contains(&"difficulty".to_string()),
        "tasks.difficulty column missing"
    );
    assert!(
        columns.contains(&"escalation_note".to_string()),
        "tasks.escalation_note column missing"
    );
}

#[test]
fn test_migration_v7_adds_default_model_to_prd_metadata() {
    let (_temp_dir, mut conn) = setup_db();

    run_migrations(&mut conn).unwrap();

    // Verify default_model column exists on prd_metadata
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('prd_metadata') WHERE name = 'default_model'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(exists, "prd_metadata.default_model column missing");
}

#[test]
fn test_migration_v7_schema_version_is_7() {
    let (_temp_dir, mut conn) = setup_db();

    // Apply exactly 7 migrations (v1–v7), not all (would include v8+)
    for _ in 0..7 {
        migrate_up(&mut conn).unwrap();
    }

    let version = get_schema_version(&conn).unwrap();
    assert_eq!(version, 7);
}

#[test]
fn test_migration_v7_new_columns_default_to_null() {
    let (_temp_dir, mut conn) = setup_db();

    // Insert a task before v6 migration
    // Run migrations up to v6 first
    for _ in 0..6 {
        migrate_up(&mut conn).unwrap();
    }

    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'Test Task', 'todo')",
        [],
    )
    .unwrap();

    // Now apply v7
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 7);

    // Verify existing task has NULL for new columns
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'US-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(model, None, "model should default to NULL");
    assert_eq!(difficulty, None, "difficulty should default to NULL");
    assert_eq!(
        escalation_note, None,
        "escalation_note should default to NULL"
    );
}

#[test]
fn test_migration_v7_down_reverts_to_v6() {
    let (_temp_dir, mut conn) = setup_db();

    // Apply exactly 7 migrations (v1–v7), not all (would include v8+)
    for _ in 0..7 {
        migrate_up(&mut conn).unwrap();
    }
    assert_eq!(get_schema_version(&conn).unwrap(), 7);

    // Migrate down from v7
    migrate_down(&mut conn).unwrap();

    let version = get_schema_version(&conn).unwrap();
    assert_eq!(
        version, 6,
        "schema_version should revert to 6 after v7 down"
    );
}

#[test]
fn test_migration_v7_columns_writable_after_migration() {
    let (_temp_dir, mut conn) = setup_db();

    run_migrations(&mut conn).unwrap();

    // Insert a task with model selection fields
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, difficulty, escalation_note) VALUES ('US-001', 'Test', 'todo', 'claude-sonnet-4-6', 'high', 'Previous attempt failed')",
        [],
    )
    .unwrap();

    // Verify values are stored and retrievable
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'US-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(model, Some("claude-sonnet-4-6".to_string()));
    assert_eq!(difficulty, Some("high".to_string()));
    assert_eq!(escalation_note, Some("Previous attempt failed".to_string()));
}

#[test]
fn test_migration_v7_default_model_writable_on_prd_metadata() {
    let (_temp_dir, mut conn) = setup_db();

    run_migrations(&mut conn).unwrap();

    // Insert prd_metadata with default_model
    conn.execute(
        "INSERT INTO prd_metadata (id, project, default_model) VALUES (1, 'test', 'claude-haiku-4-5-20251001')",
        [],
    )
    .unwrap();

    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(default_model, Some("claude-haiku-4-5-20251001".to_string()));
}

// ========== Migration v8 Tests (FTS5 Tag Indexing) ==========
//
// All behavior tests are #[ignore] — they define the implementation contract
// for FEAT task B4/FR-007. The active test verifies the stub registers correctly.
//
// Key FTS5 tokenization note: the default `ascii` tokenizer splits on `-`,
// so tag `chrono-date-handling` becomes FTS5 tokens `chrono`, `date`, `handling`.

#[test]
fn test_migration_v8_schema_version_is_8() {
    let (_temp_dir, mut conn) = setup_db();

    run_migrations(&mut conn).unwrap();

    let version = get_schema_version(&conn).unwrap();
    assert_eq!(
        version, 10,
        "schema_version should be 10 after all migrations"
    );
}

#[test]
fn test_migration_v8_adds_tags_text_column() {
    let (_temp_dir, mut conn) = setup_db();

    run_migrations(&mut conn).unwrap();

    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('learnings') WHERE name = 'tags_text'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        exists,
        "learnings.tags_text column must exist after v8 migration"
    );
}

#[test]
fn test_migration_v8_populates_tags_text_from_existing_tags() {
    // Happy path: tags_text column populated from existing learning_tags (migration path v7→v8)
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();

    // Apply migrations v1 through v7 only
    for _ in 0..7 {
        migrate_up(&mut conn).unwrap();
    }
    assert_eq!(get_schema_version(&conn).unwrap(), 7);

    // Create a learning with tag 'chrono-date-handling' BEFORE v8 migration
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Date parsing behavior".to_string(),
        content: "Time handling requires careful consideration".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["chrono-date-handling".to_string()]),
        confidence: Confidence::High,
    };
    let result = record_learning(&conn, params).unwrap();

    // Now apply v8
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 8);

    // tags_text must be populated from the existing learning_tags row
    let tags_text: Option<String> = conn
        .query_row(
            "SELECT tags_text FROM learnings WHERE id = ?1",
            [result.learning_id],
            |row| row.get(0),
        )
        .unwrap();

    assert!(
        tags_text.is_some(),
        "tags_text must be populated after v8 migration for learning with tags"
    );
    assert!(
        tags_text.unwrap().contains("chrono"),
        "tags_text must contain 'chrono' derived from tag 'chrono-date-handling'"
    );
}

#[test]
fn test_migration_v8_learning_with_no_tags_has_empty_tags_text() {
    // Edge case: learning with no tags has empty tags_text (not NULL, not garbage)
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();

    for _ in 0..7 {
        migrate_up(&mut conn).unwrap();
    }

    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "No tags learning".to_string(),
        content: "Content without tags".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    let result = record_learning(&conn, params).unwrap();

    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 8);

    let tags_text: Option<String> = conn
        .query_row(
            "SELECT tags_text FROM learnings WHERE id = ?1",
            [result.learning_id],
            |row| row.get(0),
        )
        .unwrap();

    assert!(
        tags_text.as_deref().unwrap_or("").is_empty(),
        "learning with no tags must have empty tags_text after v8 migration, got: {:?}",
        tags_text
    );
}

#[test]
fn test_migration_v8_fts5_searches_tags_text() {
    // Happy path: after migration, FTS5 search for 'chrono' finds learning tagged 'chrono-date-handling'
    // FTS5 ascii tokenizer splits on '-': 'chrono-date-handling' → tokens 'chrono', 'date', 'handling'
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    // Title and content do NOT contain 'chrono' — match must come from tags_text only
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Temporal handling note".to_string(),
        content: "Time zone offsets behave unexpectedly".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["chrono-date-handling".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"chrono\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        count, 1,
        "FTS5 search for 'chrono' must find learning tagged 'chrono-date-handling' via tags_text"
    );
}

#[test]
fn test_migration_v8_tag_add_updates_tags_text_and_fts5() {
    // Edge case: inserting a new row into learning_tags triggers tags_text update
    use crate::learnings::crud::{
        edit_learning, record_learning, EditLearningParams, RecordLearningParams,
    };
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    // Create a learning with NO tags initially
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Process note".to_string(),
        content: "General observation".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    let result = record_learning(&conn, params).unwrap();

    // Add a tag containing 'workflow'
    edit_learning(
        &conn,
        result.learning_id,
        EditLearningParams {
            add_tags: Some(vec!["pto-workflow-ux-fixes-v2".to_string()]),
            ..Default::default()
        },
    )
    .unwrap();

    // tags_text must now reflect the new tag
    let tags_text: Option<String> = conn
        .query_row(
            "SELECT tags_text FROM learnings WHERE id = ?1",
            [result.learning_id],
            |row| row.get(0),
        )
        .unwrap();

    assert!(
        tags_text.as_deref().unwrap_or("").contains("workflow"),
        "tags_text must contain 'workflow' after adding tag 'pto-workflow-ux-fixes-v2', got: {:?}",
        tags_text
    );
}

#[test]
fn test_migration_v8_tag_remove_updates_tags_text_and_fts5() {
    // Edge case: deleting a row from learning_tags triggers tags_text update
    use crate::learnings::crud::{
        edit_learning, record_learning, EditLearningParams, RecordLearningParams,
    };
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    // Create a learning WITH a tag
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Chrono feature note".to_string(),
        content: "Date handling observation".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["chrono-date-handling".to_string()]),
        confidence: Confidence::High,
    };
    let result = record_learning(&conn, params).unwrap();

    // Remove the tag
    edit_learning(
        &conn,
        result.learning_id,
        EditLearningParams {
            remove_tags: Some(vec!["chrono-date-handling".to_string()]),
            ..Default::default()
        },
    )
    .unwrap();

    // tags_text must now be empty (no remaining tags)
    let tags_text: Option<String> = conn
        .query_row(
            "SELECT tags_text FROM learnings WHERE id = ?1",
            [result.learning_id],
            |row| row.get(0),
        )
        .unwrap();

    assert!(
        tags_text.as_deref().unwrap_or("").is_empty(),
        "tags_text must be empty after removing all tags, got: {:?}",
        tags_text
    );
}

#[test]
fn test_migration_v8_workflow_tag_found_when_title_content_lack_keyword() {
    // Known-bad discriminator: FTS5 search for 'workflow' returns tag-matched result
    // even when title and content don't contain the word 'workflow'
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    // Title and content are deliberate non-matches for 'workflow'
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Sprint deviation note".to_string(),
        content: "Detour taken during planning session".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["pto-workflow-ux-fixes-v2".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // Control: unrelated learning with no 'workflow' anywhere
    let control = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Unrelated observation".to_string(),
        content: "Nothing to do with the keyword".to_string(),
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
    record_learning(&conn, control).unwrap();

    // FTS5 search for 'workflow' must find exactly the tagged learning, not the control
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"workflow\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        count, 1,
        "FTS5 search for 'workflow' must find exactly the learning tagged 'pto-workflow-ux-fixes-v2'"
    );
}

// ========== TEST-003: Comprehensive FTS5 tag indexing migration tests ==========

#[test]
fn test_migration_v8_pto_token_finds_hyphenated_tag() {
    // AC6: searching 'pto' finds learning tagged 'pto-workflow-ux-fixes-v2'.
    // FTS5 ascii tokenizer splits hyphens: 'pto-workflow-ux-fixes-v2' → tokens
    // 'pto', 'workflow', 'ux', 'fixes', 'v2'.
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    // Title and content deliberately lack 'pto'
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Leave balance adjustment".to_string(),
        content: "Accrual calculation was off by one day".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["pto-workflow-ux-fixes-v2".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"pto\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        count, 1,
        "FTS5 search for 'pto' must find learning tagged 'pto-workflow-ux-fixes-v2'"
    );
}

#[test]
fn test_migration_v8_fts5_table_has_tags_text_column() {
    // AC2: fresh database FTS5 table includes tags_text as a searchable column.
    // We verify by inserting directly into learnings_fts with 3 columns.
    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    // If FTS5 only has 2 columns (title, content), this INSERT will fail
    let result = conn.execute(
        "INSERT INTO learnings_fts(rowid, title, content, tags_text) VALUES (9999, 'test', 'test', 'test-tag')",
        [],
    );
    assert!(
        result.is_ok(),
        "FTS5 table must accept 3 columns (title, content, tags_text): {:?}",
        result.err()
    );

    // Clean up
    conn.execute(
        "INSERT INTO learnings_fts(learnings_fts, rowid, title, content, tags_text) VALUES ('delete', 9999, 'test', 'test', 'test-tag')",
        [],
    ).unwrap();
}

#[test]
fn test_migration_v8_creates_learning_tags_sync_triggers() {
    // Verify learning_tags_ai and learning_tags_ad triggers exist after v8 migration.
    // These keep tags_text in sync when learning_tags rows are added or removed.
    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    let triggers: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'learning_tags_%'",
            )
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    assert!(
        triggers.contains(&"learning_tags_ai".to_string()),
        "learning_tags_ai trigger must exist after v8 migration"
    );
    assert!(
        triggers.contains(&"learning_tags_ad".to_string()),
        "learning_tags_ad trigger must exist after v8 migration"
    );
}

#[test]
fn test_migration_v8_down_reverts_to_v7() {
    // Verify v8 down migration specifically reverts to v7 (not further).
    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();
    // Current latest is v10; first migrate down to v9, then v8
    assert_eq!(get_schema_version(&conn).unwrap(), 10);
    migrate_down(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 9);
    migrate_down(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 8);

    migrate_down(&mut conn).unwrap();

    let version = get_schema_version(&conn).unwrap();
    assert_eq!(version, 7, "v8 down should revert to v7");

    // learning_tags sync triggers must be gone
    let tag_triggers: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'learning_tags_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        tag_triggers, 0,
        "learning_tags triggers must be removed after v8 down"
    );

    // FTS5 should still exist (restored as 2-column by down migration)
    let fts_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='learnings_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        fts_exists,
        "FTS5 table should still exist after v8 down (restored as 2-column)"
    );
}

#[test]
fn test_migration_v8_preserves_existing_title_content_search() {
    // Invariant: existing FTS5 queries on title/content return same results after v8 migration.
    // Pre-v8 data must not be lost when FTS5 table is rebuilt with 3 columns.
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();

    // Apply v1–v7 (includes FTS5 setup in v3)
    for _ in 0..7 {
        migrate_up(&mut conn).unwrap();
    }

    // Create a learning with searchable title/content before v8
    let params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "SQLite locking deadlock".to_string(),
        content: "Concurrent writers caused WAL checkpoint stall".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // Verify searchable before v8
    let count_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"deadlock\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count_before, 1, "learning must be searchable before v8");

    // Apply v8 migration
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 8);

    // Same search must still work after v8
    let count_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"deadlock\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count_after, 1,
        "existing title/content must remain searchable after v8 migration"
    );
}

#[test]
fn test_migration_v8_multiple_tags_stored_space_separated() {
    // Edge case: learning with multiple tags has space-separated tags_text.
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Multi-tag learning".to_string(),
        content: "Has several tags".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec![
            "workflow-detour-phase3".to_string(),
            "long-term".to_string(),
            "rust-patterns".to_string(),
        ]),
        confidence: Confidence::High,
    };
    let result = record_learning(&conn, params).unwrap();

    let tags_text: String = conn
        .query_row(
            "SELECT tags_text FROM learnings WHERE id = ?1",
            [result.learning_id],
            |row| row.get(0),
        )
        .unwrap();

    // tags_text must contain all three tags, space-separated
    assert!(
        tags_text.contains("workflow-detour-phase3"),
        "tags_text must contain first tag, got: {}",
        tags_text
    );
    assert!(
        tags_text.contains("long-term"),
        "tags_text must contain second tag, got: {}",
        tags_text
    );
    assert!(
        tags_text.contains("rust-patterns"),
        "tags_text must contain third tag, got: {}",
        tags_text
    );

    // Each tag should be findable via FTS5
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"detour\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "FTS5 must find learning via token from first tag");

    let count2: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"rust\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count2, 1,
        "FTS5 must find learning via token from third tag"
    );
}

// ========== Migration v9 Tests (prd_metadata singleton removal) ==========
//
// v9 removes CHECK(id=1) from prd_metadata (requires table recreation in SQLite),
// adds UNIQUE constraint on task_prefix, and enables AUTOINCREMENT for new rows.
// The down migration restores the singleton constraint and copies back the first row.

/// Apply migrations up to exactly v8 (for v9 test setup).
fn migrate_to_v8(conn: &mut Connection) {
    for _ in 0..8 {
        migrate_up(conn).unwrap();
    }
    assert_eq!(get_schema_version(conn).unwrap(), 8);
}

#[test]
fn test_migration_v9_schema_version_is_9() {
    let (_temp_dir, mut conn) = setup_db();

    // Apply all migrations — v9 is now the latest
    run_migrations(&mut conn).unwrap();

    let version = get_schema_version(&conn).unwrap();
    assert_eq!(
        version, 10,
        "schema_version should be 10 after all migrations"
    );
}

#[test]
fn test_migration_v9_up_preserves_existing_prd_metadata_row() {
    let (_temp_dir, mut conn) = setup_db();

    migrate_to_v8(&mut conn);

    // Insert a prd_metadata row at id=1 with a task_prefix (v5 added that column)
    conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'my-prd', 'SS')",
        [],
    )
    .unwrap();

    // Apply v9
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 9);

    // The existing row must still be present with original data
    let (project, task_prefix): (String, Option<String>) = conn
        .query_row(
            "SELECT project, task_prefix FROM prd_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(project, "my-prd");
    assert_eq!(task_prefix, Some("SS".to_string()));
}

#[test]
fn test_migration_v9_allows_inserting_second_row() {
    // Known-bad discriminator: inserting id=2 must succeed after v9 migration.
    // This would fail with SQLITE_CONSTRAINT if CHECK(id=1) were still present.
    let (_temp_dir, mut conn) = setup_db();

    migrate_to_v8(&mut conn);

    conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'prd-one', 'P1')",
        [],
    )
    .unwrap();

    // Apply v9
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 9);

    // Must succeed — CHECK(id=1) is gone
    let result = conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (2, 'prd-two', 'P2')",
        [],
    );
    assert!(
        result.is_ok(),
        "Inserting id=2 must succeed after v9 migration (CHECK(id=1) removed): {:?}",
        result.err()
    );

    // Both rows must be readable
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prd_metadata", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2, "Both prd_metadata rows must exist after insert");
}

#[test]
fn test_migration_v9_task_prefix_unique_constraint() {
    let (_temp_dir, mut conn) = setup_db();

    migrate_to_v8(&mut conn);
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 9);

    conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'prd-one', 'SS')",
        [],
    )
    .unwrap();

    // Inserting a second row with the same task_prefix must fail
    let result = conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (2, 'prd-two', 'SS')",
        [],
    );
    assert!(
        result.is_err(),
        "Duplicate task_prefix must be rejected by UNIQUE constraint"
    );
}

#[test]
fn test_migration_v9_null_task_prefix_not_unique_constrained() {
    // NULL task_prefix is allowed for multiple rows (SQLite: UNIQUE allows multiple NULLs)
    let (_temp_dir, mut conn) = setup_db();

    migrate_to_v8(&mut conn);
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 9);

    conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'prd-one', NULL)",
        [],
    )
    .unwrap();

    let result = conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (2, 'prd-two', NULL)",
        [],
    );
    assert!(
        result.is_ok(),
        "Multiple rows with NULL task_prefix must be allowed (SQLite UNIQUE semantics): {:?}",
        result.err()
    );
}

#[test]
fn test_migration_v9_down_reverts_to_v8() {
    let (_temp_dir, mut conn) = setup_db();

    migrate_to_v8(&mut conn);

    // Insert a row before applying v9
    conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'my-prd', 'SS')",
        [],
    )
    .unwrap();

    // Apply v9
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 9);

    // Add a second row (only possible after v9)
    conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (2, 'other-prd', 'P2')",
        [],
    )
    .unwrap();

    // Migrate down to v8
    migrate_down(&mut conn).unwrap();

    let version = get_schema_version(&conn).unwrap();
    assert_eq!(version, 8, "schema_version must revert to 8 after v9 down");
}

#[test]
fn test_migration_v9_down_restores_singleton_constraint() {
    // After down migration, CHECK(id=1) must be restored so id=2 inserts fail again.
    let (_temp_dir, mut conn) = setup_db();

    migrate_to_v8(&mut conn);
    migrate_up(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 9);

    // Insert row 1
    conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'my-prd', 'SS')",
        [],
    )
    .unwrap();

    // Migrate back down
    migrate_down(&mut conn).unwrap();
    assert_eq!(get_schema_version(&conn).unwrap(), 8);

    // After down migration, id=2 insert must fail (CHECK(id=1) restored)
    let result = conn.execute(
        "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (2, 'other-prd', 'P2')",
        [],
    );
    assert!(
        result.is_err(),
        "Inserting id=2 must fail after v9 down migration (CHECK(id=1) restored)"
    );
}

#[test]
fn test_migration_v8_fts5_rebuild_succeeds() {
    // Edge case from TEST-INIT-004: FTS5 rebuild command must succeed after migration.
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let (_temp_dir, mut conn) = setup_db();
    run_migrations(&mut conn).unwrap();

    // Add some data first
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Rebuild test learning".to_string(),
        content: "Testing FTS5 rebuild command".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["rebuild-test".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // FTS5 rebuild must succeed without SQLITE_CORRUPT_VTAB
    let result = conn.execute(
        "INSERT INTO learnings_fts(learnings_fts) VALUES('rebuild')",
        [],
    );
    assert!(
        result.is_ok(),
        "FTS5 rebuild must succeed after v8 migration: {:?}",
        result.err()
    );

    // Data must still be searchable after rebuild
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"rebuild\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "data must be searchable after FTS5 rebuild");
}
