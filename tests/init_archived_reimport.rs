//! Integration test: `init --append --update-existing` must reconcile against
//! archived rows instead of crashing on UNIQUE constraint violations.
//!
//! Regression: when a PRD's tasks were soft-archived (e.g. via a branch-change
//! archive pass), re-running `task-mgr loop` (which calls
//! `init --append --update-existing`) failed with
//! "Database error: UNIQUE constraint failed: tasks.id" because
//! `get_existing_task_ids()` filtered archived rows, so importer routed the
//! incoming stories to INSERT against rows that still existed in the table.
//!
//! Expected behaviour: archived rows with matching IDs are recognised, routed
//! to the update path, and revived (archived_at cleared) so the loop can
//! continue against the same task list.

use std::path::PathBuf;
use tempfile::TempDir;

use task_mgr::commands::init::{self, PrefixMode};
use task_mgr::db::open_connection;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Helper: archive every active row whose id starts with the given prefix
/// (mirrors what `branch::detect_branch_change` does via `archive::run_archive`).
fn archive_prefix(conn: &rusqlite::Connection, prefix: &str) {
    let pattern = format!("{}-%", prefix);
    let n = conn
        .execute(
            "UPDATE tasks SET archived_at = datetime('now') WHERE id LIKE ?",
            [&pattern],
        )
        .unwrap();
    assert!(n > 0, "expected to archive at least one row for {}", prefix);
}

fn count_active(conn: &rusqlite::Connection, prefix: &str) -> i64 {
    let pattern = format!("{}-%", prefix);
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE id LIKE ? AND archived_at IS NULL",
        [&pattern],
        |row| row.get(0),
    )
    .unwrap()
}

fn count_archived(conn: &rusqlite::Connection, prefix: &str) -> i64 {
    let pattern = format!("{}-%", prefix);
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE id LIKE ? AND archived_at IS NOT NULL",
        [&pattern],
        |row| row.get(0),
    )
    .unwrap()
}

/// AC1: `init --append --update-existing` must not crash with a UNIQUE
/// constraint error when archived rows share IDs with the incoming PRD.
#[test]
fn reimport_with_archived_rows_does_not_crash() {
    let temp_dir = TempDir::new().unwrap();

    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("P1".to_string()),
    )
    .unwrap();

    {
        let conn = open_connection(temp_dir.path()).unwrap();
        archive_prefix(&conn, "P1");
        assert_eq!(count_active(&conn, "P1"), 0);
        assert_eq!(count_archived(&conn, "P1"), 2);
    }

    let result = init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false,
        true,
        true,
        false,
        PrefixMode::Explicit("P1".to_string()),
    );

    assert!(
        result.is_ok(),
        "expected init --append --update-existing to succeed against archived rows, got: {:?}",
        result.err()
    );
}

/// AC2: re-imported tasks should be revived (archived_at cleared) so the
/// loop's `next` query can see them again.
#[test]
fn reimport_revives_archived_rows() {
    let temp_dir = TempDir::new().unwrap();

    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("P1".to_string()),
    )
    .unwrap();

    {
        let conn = open_connection(temp_dir.path()).unwrap();
        archive_prefix(&conn, "P1");
    }

    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false,
        true,
        true,
        false,
        PrefixMode::Explicit("P1".to_string()),
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    assert_eq!(
        count_active(&conn, "P1"),
        2,
        "all archived rows should be revived to active after re-import"
    );
    assert_eq!(
        count_archived(&conn, "P1"),
        0,
        "no rows should remain archived after re-import with update-existing"
    );
}

/// AC3: revival must update metadata fields from the JSON, not just clear
/// archived_at — proves the row went through the update path, not a no-op.
#[test]
fn reimport_updates_metadata_on_revived_rows() {
    let temp_dir = TempDir::new().unwrap();

    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("P1".to_string()),
    )
    .unwrap();

    {
        let conn = open_connection(temp_dir.path()).unwrap();
        // Mutate the live row so we can detect the post-revival overwrite.
        conn.execute(
            "UPDATE tasks SET title = 'STALE TITLE FROM PRIOR RUN' WHERE id = 'P1-TASK-001'",
            [],
        )
        .unwrap();
        archive_prefix(&conn, "P1");
    }

    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false,
        true,
        true,
        false,
        PrefixMode::Explicit("P1".to_string()),
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let title: String = conn
        .query_row(
            "SELECT title FROM tasks WHERE id = 'P1-TASK-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        title, "Alpha Task 1",
        "revived row should be updated with the JSON's title (proves update path ran)"
    );
}

/// AC4: when ALL active rows in the database are archived (so the importer
/// would otherwise consider it a "fresh" import), a re-import still
/// reconciles against the archived rows. Defends the fix path that no longer
/// gates `existing_ids` collection on `!fresh_import`.
#[test]
fn reimport_against_fully_archived_db_still_reconciles() {
    let temp_dir = TempDir::new().unwrap();

    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("P1".to_string()),
    )
    .unwrap();

    {
        let conn = open_connection(temp_dir.path()).unwrap();
        archive_prefix(&conn, "P1");
        // Sanity: db is now "fresh" by the legacy is_fresh_database definition
        // (no rows where archived_at IS NULL).
        let active: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE archived_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, 0);
    }

    let result = init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false,
        true,
        true,
        false,
        PrefixMode::Explicit("P1".to_string()),
    );
    assert!(
        result.is_ok(),
        "fully-archived DB should not crash on re-import: {:?}",
        result.err()
    );

    let conn = open_connection(temp_dir.path()).unwrap();
    assert_eq!(count_active(&conn, "P1"), 2);
    assert_eq!(count_archived(&conn, "P1"), 0);
}
