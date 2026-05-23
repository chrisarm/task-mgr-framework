//! Unit tests for `TaskLifecycle::apply` — Category A dispatch + auto-claim.

#![cfg(test)]

use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, migrations::run_migrations, open_connection};
use crate::lifecycle::matrix::TransitionSource;
use crate::lifecycle::{TaskLifecycle, TransitionChange, TransitionIntent, TransitionRejectReason};
use crate::models::TaskStatus;

fn setup() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let mut conn = open_connection(dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (dir, conn)
}

fn insert_task(conn: &Connection, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?, 'Test', ?, 10)",
        rusqlite::params![id, status],
    )
    .unwrap();
}

fn insert_run(conn: &Connection, run_id: &str) {
    conn.execute(
        "INSERT INTO runs (run_id, status, iteration_count) VALUES (?, 'active', 0)",
        [run_id],
    )
    .unwrap();
}

fn get_status(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |row| {
        row.get::<_, String>(0)
    })
    .ok()
}

fn make_intent(task_id: &str, change: TransitionChange) -> TransitionIntent {
    TransitionIntent {
        task_id: task_id.to_string(),
        change,
        source: TransitionSource::LoopStatusTag,
        reason: None,
        fail_status: None,
        audit_note: None,
    }
}

#[test]
fn apply_empty_returns_empty_vec_no_writes() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-001", "todo");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[])
    };

    assert!(outcomes.is_empty());
    // Row untouched
    assert_eq!(get_status(&conn, "T-001").as_deref(), Some("todo"));
}

#[test]
fn apply_done_from_in_progress_transitions_to_done() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-002", "in_progress");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("T-002", TransitionChange::Done)])
    };

    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].applied);
    assert_eq!(outcomes[0].task_id, "T-002");
    assert_eq!(outcomes[0].target, TaskStatus::Done);
    assert_eq!(outcomes[0].previous, Some(TaskStatus::InProgress));
    assert!(outcomes[0].reason.is_none());
    assert_eq!(get_status(&conn, "T-002").as_deref(), Some("done"));
}

#[test]
fn apply_auto_claims_done_from_todo_for_loop_status_tag() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-003", "todo");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("T-003", TransitionChange::Done)])
    };

    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].applied);
    // outcome.previous reports the ORIGINAL previous (Todo), not the
    // intermediate InProgress the auto-claim path set.
    assert_eq!(outcomes[0].previous, Some(TaskStatus::Todo));
    assert_eq!(outcomes[0].target, TaskStatus::Done);
    assert_eq!(get_status(&conn, "T-003").as_deref(), Some("done"));
}

#[test]
fn apply_auto_claim_inserts_run_tasks_row_when_run_id_is_set() {
    let (_dir, mut conn) = setup();
    insert_run(&conn, "run-42");
    insert_task(&conn, "T-004", "todo");

    {
        let mut lc = TaskLifecycle::with_run(&mut conn, "run-42");
        let outcomes = lc.apply(&[make_intent("T-004", TransitionChange::Done)]);
        assert!(outcomes[0].applied);
    }

    // Auto-claim must have inserted a run_tasks row at iteration=1
    // (MAX(iteration)+1 over empty table = 1).
    let iter: Option<i64> = conn
        .query_row(
            "SELECT iteration FROM run_tasks WHERE run_id = 'run-42' AND task_id = 'T-004'",
            [],
            |row| row.get(0),
        )
        .ok();
    assert_eq!(
        iter,
        Some(1),
        "auto-claim should insert run_tasks at MAX(iteration)+1"
    );
}

#[test]
fn apply_does_not_auto_claim_for_operator_source() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-005", "todo");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[TransitionIntent {
            task_id: "T-005".to_string(),
            change: TransitionChange::Done,
            source: TransitionSource::Operator,
            reason: None,
            fail_status: None,
            audit_note: None,
        }])
    };

    // Operator source does not auto-claim — complete() rejects Todo->Done.
    assert!(!outcomes[0].applied);
    assert!(outcomes[0].reason.is_some());
    assert_eq!(get_status(&conn, "T-005").as_deref(), Some("todo"));
}

#[test]
fn apply_partial_failure_does_not_short_circuit_batch() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "OK-1", "in_progress");
    // BAD-1 doesn't exist — the dispatcher will return NotFound.
    insert_task(&conn, "OK-2", "in_progress");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[
            make_intent("OK-1", TransitionChange::Done),
            make_intent("BAD-1", TransitionChange::Done),
            make_intent("OK-2", TransitionChange::Done),
        ])
    };

    assert_eq!(outcomes.len(), 3);
    assert!(outcomes[0].applied, "OK-1 must succeed independently");
    assert!(!outcomes[1].applied, "BAD-1 must fail");
    assert!(
        matches!(
            outcomes[1].reason,
            Some(TransitionRejectReason::DispatchFailed(_))
        ),
        "BAD-1 outcome must carry DispatchFailed reason"
    );
    assert!(outcomes[2].applied, "OK-2 still applies after BAD-1 fails");

    assert_eq!(get_status(&conn, "OK-1").as_deref(), Some("done"));
    assert_eq!(get_status(&conn, "OK-2").as_deref(), Some("done"));
}

#[test]
fn apply_failed_routes_to_blocked() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-006", "in_progress");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("T-006", TransitionChange::Failed)])
    };

    assert!(outcomes[0].applied);
    assert_eq!(outcomes[0].target, TaskStatus::Blocked);
    assert_eq!(get_status(&conn, "T-006").as_deref(), Some("blocked"));
}

#[test]
fn apply_skipped_routes_to_skipped() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-007", "in_progress");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("T-007", TransitionChange::Skipped)])
    };

    assert!(outcomes[0].applied);
    assert_eq!(outcomes[0].target, TaskStatus::Skipped);
    assert_eq!(get_status(&conn, "T-007").as_deref(), Some("skipped"));
}

#[test]
fn apply_irrelevant_routes_to_irrelevant() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-008", "in_progress");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("T-008", TransitionChange::Irrelevant)])
    };

    assert!(outcomes[0].applied);
    assert_eq!(outcomes[0].target, TaskStatus::Irrelevant);
    assert_eq!(get_status(&conn, "T-008").as_deref(), Some("irrelevant"));
}

#[test]
fn apply_unblock_returns_blocked_to_todo() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-009", "blocked");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("T-009", TransitionChange::Unblock)])
    };

    assert!(outcomes[0].applied);
    assert_eq!(outcomes[0].target, TaskStatus::Todo);
    assert_eq!(get_status(&conn, "T-009").as_deref(), Some("todo"));
}

#[test]
fn apply_reset_returns_in_progress_to_todo() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-010", "in_progress");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("T-010", TransitionChange::Reset)])
    };

    assert!(outcomes[0].applied);
    assert_eq!(outcomes[0].target, TaskStatus::Todo);
    assert_eq!(get_status(&conn, "T-010").as_deref(), Some("todo"));
}

#[test]
fn apply_missing_task_reports_dispatch_failed_without_panic() {
    let (_dir, mut conn) = setup();

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("MISSING", TransitionChange::Done)])
    };

    assert_eq!(outcomes.len(), 1);
    assert!(!outcomes[0].applied);
    assert_eq!(outcomes[0].previous, None);
    assert!(outcomes[0].reason.is_some());
}

#[test]
fn apply_done_flips_prd_passes_when_prd_sync_configured() {
    let (dir, mut conn) = setup();
    insert_task(&conn, "T-011", "in_progress");

    // Minimal PRD JSON the reconciler will accept; passes starts false.
    // The reconciler looks up by `userStories[].id`, not `tasks[].id`.
    let prd_path = dir.path().join("prd.json");
    std::fs::write(
        &prd_path,
        r#"{"userStories":[{"id":"T-011","title":"x","passes":false}]}"#,
    )
    .unwrap();

    {
        let mut lc = TaskLifecycle::new(&mut conn).with_prd_sync(&prd_path, "");
        let outcomes = lc.apply(&[make_intent("T-011", TransitionChange::Done)]);
        assert!(outcomes[0].applied);
    }

    let body = std::fs::read_to_string(&prd_path).unwrap();
    assert!(
        body.contains("\"passes\": true") || body.contains("\"passes\":true"),
        "PRD JSON should reflect passes=true after Done; got: {body}"
    );
}

#[test]
fn apply_done_succeeds_even_when_prd_sync_fails() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-012", "in_progress");

    // Non-existent PRD path → update_prd_task_passes returns
    // IoErrorWithContext. apply() must still report applied=true and DB
    // must reach 'done' — DB-authoritative, PRD best-effort (learning #2284).
    let missing = Path::new("/dev/null/nonexistent-prd-for-test.json");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn).with_prd_sync(missing, "");
        lc.apply(&[make_intent("T-012", TransitionChange::Done)])
    };

    assert!(
        outcomes[0].applied,
        "applied must stay true when only PRD sync fails"
    );
    assert_eq!(get_status(&conn, "T-012").as_deref(), Some("done"));
}

// ── Gate auto-claim run_tasks INSERT on UPDATE rows_affected (FEAT-003) ──────

/// Happy path: task at Todo, run_id set — auto-claim fires, run_tasks row
/// inserted, outcome.applied = true.
#[test]
fn auto_claim_todo_with_run_id_inserts_run_tasks_row() {
    let (_dir, mut conn) = setup();
    insert_run(&conn, "run-feat003-a");
    insert_task(&conn, "T-FA1", "todo");

    let outcomes = {
        let mut lc = TaskLifecycle::with_run(&mut conn, "run-feat003-a");
        lc.apply(&[make_intent("T-FA1", TransitionChange::Done)])
    };

    assert!(outcomes[0].applied, "Todo→Done via auto-claim must succeed");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM run_tasks WHERE run_id = 'run-feat003-a' AND task_id = 'T-FA1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "run_tasks must have exactly one row for the auto-claim"
    );
}

/// Fix: task already at Done (simulating post-concurrent-flip state), run_id
/// set — auto-claim UPDATE no-ops, no orphan run_tasks row must be inserted.
///
/// Known-bad: if the gate used `claimed || self.run_id.is_some()` (OR instead
/// of AND), the INSERT would fire even when claimed=false.
#[test]
fn auto_claim_done_state_inserts_no_run_tasks_row() {
    let (_dir, mut conn) = setup();
    insert_run(&conn, "run-feat003-b");
    insert_task(&conn, "T-FA2", "done");

    {
        let mut lc = TaskLifecycle::with_run(&mut conn, "run-feat003-b");
        lc.apply(&[make_intent("T-FA2", TransitionChange::Done)]);
    }

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM run_tasks WHERE run_id = 'run-feat003-b' AND task_id = 'T-FA2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "auto-claim must not insert a run_tasks row when the task is not Todo"
    );
}

/// Edge: run_id is None — auto-claim UPDATE may still fire on a Todo task but
/// no run_tasks INSERT must occur (there's no run to associate).
#[test]
fn auto_claim_no_run_id_inserts_no_run_tasks_row() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "T-FA3", "todo");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.apply(&[make_intent("T-FA3", TransitionChange::Done)])
    };

    assert!(
        outcomes[0].applied,
        "Todo→Done without run context must succeed"
    );

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM run_tasks WHERE task_id = 'T-FA3'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "no run_tasks row without a run_id");
}
