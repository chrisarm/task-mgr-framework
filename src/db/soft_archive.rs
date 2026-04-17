//! Shared soft-archive helpers for prefix-scoped database operations.
//!
//! [`soft_archive_by_prefix`] soft-archives `run_tasks`, `key_decisions`, and
//! `runs` for a given PRD task prefix. Both the archive command and
//! `init --force` reimport use this helper to avoid duplicating SQL.

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::db::prefix::make_like_pattern;

/// Soft-archive `run_tasks`, `key_decisions`, and `runs` matching `prefix`.
///
/// Sets `archived_at = datetime('now')` on:
/// 1. `run_tasks` where `task_id LIKE '{prefix}-%'`
/// 2. `key_decisions` where `task_id LIKE '{prefix}-%'`
/// 3. `key_decisions` for runs that are now fully archived (all run_tasks archived)
/// 4. `runs` where all `run_tasks` are now archived (NOT EXISTS)
///
/// Only rows with `archived_at IS NULL` are updated — previously archived rows
/// keep their original timestamp (idempotent).
///
/// # Transaction safety
///
/// The caller controls the transaction boundary. Pass `&*tx` to execute within
/// an existing [`rusqlite::Transaction`], or `conn` directly to execute
/// without an explicit transaction.
pub fn soft_archive_by_prefix(conn: &Connection, prefix: &str) -> TaskMgrResult<()> {
    let pattern = make_like_pattern(prefix);

    // 1. Soft-archive run_tasks for this prefix
    conn.execute(
        "UPDATE run_tasks SET archived_at = datetime('now') \
         WHERE task_id LIKE ? ESCAPE '\\' AND archived_at IS NULL",
        rusqlite::params![pattern],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 2a. Soft-archive key_decisions by task prefix
    conn.execute(
        "UPDATE key_decisions SET archived_at = datetime('now') \
         WHERE task_id LIKE ? ESCAPE '\\' AND archived_at IS NULL",
        rusqlite::params![pattern],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 2b. Soft-archive key_decisions for runs that are now fully archived
    //     (covers NULL task_id key_decisions that reference the run)
    conn.execute(
        "UPDATE key_decisions SET archived_at = datetime('now') \
         WHERE archived_at IS NULL \
         AND run_id IN ( \
             SELECT run_id FROM runs \
             WHERE NOT EXISTS ( \
                 SELECT 1 FROM run_tasks \
                 WHERE run_tasks.run_id = runs.run_id \
                 AND run_tasks.archived_at IS NULL \
             ) \
         )",
        [],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 3. Soft-archive runs where ALL run_tasks are now archived
    conn.execute(
        "UPDATE runs SET archived_at = datetime('now') \
         WHERE archived_at IS NULL \
         AND NOT EXISTS ( \
             SELECT 1 FROM run_tasks \
             WHERE run_tasks.run_id = runs.run_id \
             AND run_tasks.archived_at IS NULL \
         )",
        [],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{open_connection, run_migrations};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let mut conn = open_connection(dir.path()).unwrap();
        run_migrations(&mut conn).unwrap();
        (dir, conn)
    }

    fn insert_task(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, acceptance_criteria) \
             VALUES (?, 'title', 'todo', 1, '[]')",
            rusqlite::params![id],
        )
        .unwrap();
    }

    fn insert_run(conn: &Connection, run_id: &str) {
        conn.execute(
            "INSERT INTO runs (run_id, started_at) VALUES (?, datetime('now'))",
            rusqlite::params![run_id],
        )
        .unwrap();
    }

    fn insert_run_task(conn: &Connection, run_id: &str, task_id: &str) {
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES (?, ?, 1)",
            rusqlite::params![run_id, task_id],
        )
        .unwrap();
    }

    fn insert_key_decision(conn: &Connection, run_id: &str, task_id: Option<&str>) {
        conn.execute(
            "INSERT INTO key_decisions \
             (run_id, task_id, iteration, title, description, options) \
             VALUES (?, ?, 1, 'kd', 'desc', '[]')",
            rusqlite::params![run_id, task_id],
        )
        .unwrap();
    }

    #[test]
    fn test_archives_run_tasks_for_prefix() {
        let (_dir, conn) = setup_db();
        insert_task(&conn, "PA-001");
        insert_task(&conn, "PB-001");
        insert_run(&conn, "r1");
        insert_run_task(&conn, "r1", "PA-001");
        insert_run_task(&conn, "r1", "PB-001");

        soft_archive_by_prefix(&conn, "PA").unwrap();

        let pa_archived: Option<String> = conn
            .query_row(
                "SELECT archived_at FROM run_tasks WHERE task_id = 'PA-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let pb_archived: Option<String> = conn
            .query_row(
                "SELECT archived_at FROM run_tasks WHERE task_id = 'PB-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert!(pa_archived.is_some(), "PA run_task must be archived");
        assert!(pb_archived.is_none(), "PB run_task must not be archived");
    }

    #[test]
    fn test_shared_run_stays_active_when_other_prefix_has_unarchived_tasks() {
        let (_dir, conn) = setup_db();
        insert_task(&conn, "PA-001");
        insert_task(&conn, "PB-001");
        insert_run(&conn, "r-shared");
        insert_run_task(&conn, "r-shared", "PA-001");
        insert_run_task(&conn, "r-shared", "PB-001");

        soft_archive_by_prefix(&conn, "PA").unwrap();

        let run_archived: Option<String> = conn
            .query_row(
                "SELECT archived_at FROM runs WHERE run_id = 'r-shared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            run_archived.is_none(),
            "Shared run must stay active: PB still has an unarchived run_task"
        );
    }

    #[test]
    fn test_orphaned_run_gets_archived() {
        let (_dir, conn) = setup_db();
        insert_task(&conn, "PA-001");
        insert_run(&conn, "r-pa-only");
        insert_run_task(&conn, "r-pa-only", "PA-001");

        soft_archive_by_prefix(&conn, "PA").unwrap();

        let run_archived: Option<String> = conn
            .query_row(
                "SELECT archived_at FROM runs WHERE run_id = 'r-pa-only'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            run_archived.is_some(),
            "Orphaned run must be archived when all its run_tasks belong to archived prefix"
        );
    }

    #[test]
    fn test_archives_key_decisions_by_task_prefix() {
        let (_dir, conn) = setup_db();
        // Both tasks processed by the same run: PA key_decision must be archived,
        // PB key_decision must stay active because PB run_task is still active.
        insert_task(&conn, "PA-001");
        insert_task(&conn, "PB-001");
        insert_run(&conn, "r1");
        insert_run_task(&conn, "r1", "PA-001");
        insert_run_task(&conn, "r1", "PB-001");
        insert_key_decision(&conn, "r1", Some("PA-001"));
        insert_key_decision(&conn, "r1", Some("PB-001"));

        soft_archive_by_prefix(&conn, "PA").unwrap();

        let pa_archived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM key_decisions \
                 WHERE task_id = 'PA-001' AND archived_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let pb_archived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM key_decisions \
                 WHERE task_id = 'PB-001' AND archived_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(pa_archived, 1, "PA key_decision must be archived");
        assert_eq!(pb_archived, 0, "PB key_decision must not be archived");
    }

    #[test]
    fn test_idempotent_double_call_does_not_change_timestamp() {
        let (_dir, conn) = setup_db();
        insert_task(&conn, "PA-001");
        insert_run(&conn, "r1");
        insert_run_task(&conn, "r1", "PA-001");

        soft_archive_by_prefix(&conn, "PA").unwrap();
        let ts1: Option<String> = conn
            .query_row(
                "SELECT archived_at FROM run_tasks WHERE task_id = 'PA-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        soft_archive_by_prefix(&conn, "PA").unwrap();
        let ts2: Option<String> = conn
            .query_row(
                "SELECT archived_at FROM run_tasks WHERE task_id = 'PA-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(
            ts1, ts2,
            "Second call must not change archived_at timestamp"
        );
    }
}
