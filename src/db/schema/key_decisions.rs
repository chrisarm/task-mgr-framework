//! Key decisions schema operations.
//!
//! Provides insert, query, and update functions for the `key_decisions` table.
//! The table is created via migration v12 — no DDL here.

use rusqlite::Connection;
use serde_json;

use crate::loop_engine::config::{KeyDecision, KeyDecisionOption};
use crate::{TaskMgrError, TaskMgrResult};

/// A key decision as stored in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredKeyDecision {
    pub id: i64,
    pub run_id: String,
    pub task_id: Option<String>,
    pub iteration: i64,
    pub title: String,
    pub description: String,
    pub options: Vec<KeyDecisionOption>,
    pub status: String,
    pub created_at: String,
}

/// Insert a key decision into the database.
///
/// Serializes `decision.options` as a JSON text blob.
///
/// Returns the row id of the inserted decision.
pub fn insert_key_decision(
    conn: &Connection,
    run_id: &str,
    task_id: Option<&str>,
    iteration: i64,
    decision: &KeyDecision,
) -> TaskMgrResult<i64> {
    let options_json = serde_json::to_string(&decision.options).map_err(|e| {
        TaskMgrError::DatabaseError(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })?;

    conn.execute(
        "INSERT INTO key_decisions (run_id, task_id, iteration, title, description, options)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            run_id,
            task_id,
            iteration,
            decision.title,
            decision.description,
            options_json
        ],
    )
    .map_err(TaskMgrError::DatabaseError)?;

    Ok(conn.last_insert_rowid())
}

/// Get pending and deferred decisions for a specific run.
pub fn get_pending_decisions(
    conn: &Connection,
    run_id: &str,
) -> TaskMgrResult<Vec<StoredKeyDecision>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, run_id, task_id, iteration, title, description, options, status, created_at
             FROM key_decisions
             WHERE run_id = ?1 AND status IN ('pending', 'deferred')
             ORDER BY created_at ASC",
        )
        .map_err(TaskMgrError::DatabaseError)?;

    let rows = stmt
        .query_map(rusqlite::params![run_id], map_row)
        .map_err(TaskMgrError::DatabaseError)?;

    collect_rows(rows)
}

/// Get all pending and deferred decisions across all runs.
pub fn get_all_pending_decisions(conn: &Connection) -> TaskMgrResult<Vec<StoredKeyDecision>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, run_id, task_id, iteration, title, description, options, status, created_at
             FROM key_decisions
             WHERE status IN ('pending', 'deferred')
             ORDER BY created_at ASC",
        )
        .map_err(TaskMgrError::DatabaseError)?;

    let rows = stmt
        .query_map([], map_row)
        .map_err(TaskMgrError::DatabaseError)?;

    collect_rows(rows)
}

/// Resolve a key decision with a resolution text.
///
/// Sets `status = 'resolved'`, `resolution = text`, and `resolved_at = datetime('now')`.
pub fn resolve_decision(conn: &Connection, id: i64, resolution: &str) -> TaskMgrResult<()> {
    conn.execute(
        "UPDATE key_decisions
         SET status = 'resolved', resolution = ?1, resolved_at = datetime('now')
         WHERE id = ?2",
        rusqlite::params![resolution, id],
    )
    .map_err(TaskMgrError::DatabaseError)?;

    Ok(())
}

/// Defer a key decision (marks it as deferred for later resurface).
pub fn defer_decision(conn: &Connection, id: i64) -> TaskMgrResult<()> {
    conn.execute(
        "UPDATE key_decisions SET status = 'deferred' WHERE id = ?1",
        rusqlite::params![id],
    )
    .map_err(TaskMgrError::DatabaseError)?;

    Ok(())
}

/// Map a rusqlite row to a `StoredKeyDecision`.
fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredKeyDecision> {
    let options_json: String = row.get(6)?;
    let options: Vec<KeyDecisionOption> = match serde_json::from_str(&options_json) {
        Ok(opts) => opts,
        Err(e) => {
            eprintln!(
                "Warning: malformed options JSON in key_decisions row, defaulting to empty: {}",
                e
            );
            Vec::new()
        }
    };

    Ok(StoredKeyDecision {
        id: row.get(0)?,
        run_id: row.get(1)?,
        task_id: row.get(2)?,
        iteration: row.get(3)?,
        title: row.get(4)?,
        description: row.get(5)?,
        options,
        status: row.get(7)?,
        created_at: row.get(8)?,
    })
}

/// Collect query_map results into a Vec, propagating errors.
fn collect_rows(
    rows: impl Iterator<Item = rusqlite::Result<StoredKeyDecision>>,
) -> TaskMgrResult<Vec<StoredKeyDecision>> {
    rows.map(|r| r.map_err(TaskMgrError::DatabaseError))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::{create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let mut conn = open_connection(dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        // Insert a run to satisfy FK constraint
        conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
            .unwrap();
        (dir, conn)
    }

    fn make_decision() -> KeyDecision {
        KeyDecision {
            title: "Storage backend".to_string(),
            description: "Choose between SQLite and PostgreSQL".to_string(),
            options: vec![
                KeyDecisionOption {
                    label: "SQLite".to_string(),
                    description: "Simple, embedded".to_string(),
                },
                KeyDecisionOption {
                    label: "PostgreSQL".to_string(),
                    description: "Scalable, networked".to_string(),
                },
            ],
        }
    }

    #[test]
    fn test_insert_and_get_pending_decisions() {
        let (_dir, conn) = setup_db();
        let decision = make_decision();

        let id = insert_key_decision(&conn, "run-001", None, 1, &decision).unwrap();
        assert!(id > 0);

        let pending = get_pending_decisions(&conn, "run-001").unwrap();
        assert_eq!(pending.len(), 1);

        let stored = &pending[0];
        assert_eq!(stored.id, id);
        assert_eq!(stored.run_id, "run-001");
        assert_eq!(stored.task_id, None);
        assert_eq!(stored.iteration, 1);
        assert_eq!(stored.title, decision.title);
        assert_eq!(stored.description, decision.description);
        assert_eq!(stored.options, decision.options);
        assert_eq!(stored.status, "pending");
        assert!(!stored.created_at.is_empty());
    }

    #[test]
    fn test_get_pending_decisions_filters_by_run_id() {
        let (_dir, conn) = setup_db();
        conn.execute("INSERT INTO runs (run_id) VALUES ('run-002')", [])
            .unwrap();

        let d = make_decision();
        insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();
        insert_key_decision(&conn, "run-002", None, 1, &d).unwrap();

        let pending = get_pending_decisions(&conn, "run-001").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].run_id, "run-001");
    }

    #[test]
    fn test_get_all_pending_decisions() {
        let (_dir, conn) = setup_db();
        conn.execute("INSERT INTO runs (run_id) VALUES ('run-002')", [])
            .unwrap();

        let d = make_decision();
        insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();
        insert_key_decision(&conn, "run-002", None, 2, &d).unwrap();

        let all = get_all_pending_decisions(&conn).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_resolve_decision() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        resolve_decision(&conn, id, "Go with SQLite for simplicity").unwrap();

        // Should no longer appear in pending
        let pending = get_pending_decisions(&conn, "run-001").unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn test_defer_decision() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        defer_decision(&conn, id).unwrap();

        // Deferred decisions still appear in pending queries
        let pending = get_pending_decisions(&conn, "run-001").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, "deferred");
    }

    #[test]
    fn test_get_all_pending_filters_resolved() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();
        insert_key_decision(&conn, "run-001", None, 2, &d).unwrap();

        resolve_decision(&conn, id, "resolved").unwrap();

        let all = get_all_pending_decisions(&conn).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_options_serialized_as_json() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        let raw: String = conn
            .query_row(
                "SELECT options FROM key_decisions WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap();

        // Must be valid JSON array
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(parsed.is_array());
    }
}
