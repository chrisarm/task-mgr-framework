//! Unit tests for `reconcile_from_prd` / `repair_stale` (FEAT-006).

#![cfg(test)]

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, migrations::run_migrations, open_connection};
use crate::lifecycle::{ReconcileItem, ReconcilePlan, RepairItem, RepairPlan, TaskLifecycle};
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

fn get_status(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |row| {
        row.get::<_, String>(0)
    })
    .ok()
}

fn get_notes(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT notes FROM tasks WHERE id = ?", [id], |row| {
        row.get::<_, Option<String>>(0)
    })
    .ok()
    .flatten()
}

// --- reconcile_from_prd ---

#[test]
fn reconcile_mark_done_from_todo() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "R-001", "todo");

    let plan = ReconcilePlan {
        items: vec![ReconcileItem {
            task_id: "R-001".to_string(),
            target: TaskStatus::Done,
            audit_label: None,
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.reconcile_from_prd(plan).unwrap()
    };

    assert_eq!(report.applied, 1);
    assert_eq!(report.skipped, 0);
    assert!(report.rejected.is_empty());
    assert_eq!(get_status(&conn, "R-001").as_deref(), Some("done"));
}

#[test]
fn reconcile_mark_done_from_in_progress() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "R-002", "in_progress");

    let plan = ReconcilePlan {
        items: vec![ReconcileItem {
            task_id: "R-002".to_string(),
            target: TaskStatus::Done,
            audit_label: None,
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.reconcile_from_prd(plan).unwrap()
    };

    assert_eq!(report.applied, 1);
    assert_eq!(get_status(&conn, "R-002").as_deref(), Some("done"));
}

#[test]
fn reconcile_done_to_irrelevant_permitted() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "R-003", "done");

    let plan = ReconcilePlan {
        items: vec![ReconcileItem {
            task_id: "R-003".to_string(),
            target: TaskStatus::Irrelevant,
            audit_label: None,
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.reconcile_from_prd(plan).unwrap()
    };

    assert_eq!(
        report.applied, 1,
        "done -> irrelevant must be permitted under ReconcilePrd"
    );
    assert_eq!(get_status(&conn, "R-003").as_deref(), Some("irrelevant"));
}

#[test]
fn reconcile_idempotent_already_done_is_skipped() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "R-004", "done");

    let plan = ReconcilePlan {
        items: vec![ReconcileItem {
            task_id: "R-004".to_string(),
            target: TaskStatus::Done,
            audit_label: None,
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.reconcile_from_prd(plan).unwrap()
    };

    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 1, "from == target must be a no-op skip");
    assert!(report.rejected.is_empty());
}

#[test]
fn reconcile_missing_row_is_rejected() {
    let (_dir, mut conn) = setup();

    let plan = ReconcilePlan {
        items: vec![ReconcileItem {
            task_id: "DOES-NOT-EXIST".to_string(),
            target: TaskStatus::Done,
            audit_label: None,
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.reconcile_from_prd(plan).unwrap()
    };

    assert_eq!(report.applied, 0);
    assert_eq!(report.rejected, vec!["DOES-NOT-EXIST".to_string()]);
}

#[test]
fn reconcile_partial_failure_continues_batch() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "R-005", "todo");
    // R-006 missing → rejected, R-007 done → skipped (idempotent)
    insert_task(&conn, "R-007", "done");

    let plan = ReconcilePlan {
        items: vec![
            ReconcileItem {
                task_id: "R-005".to_string(),
                target: TaskStatus::Done,
                audit_label: None,
            },
            ReconcileItem {
                task_id: "R-006".to_string(),
                target: TaskStatus::Done,
                audit_label: None,
            },
            ReconcileItem {
                task_id: "R-007".to_string(),
                target: TaskStatus::Done,
                audit_label: None,
            },
        ],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.reconcile_from_prd(plan).unwrap()
    };

    assert_eq!(report.applied, 1, "R-005 applied");
    assert_eq!(report.skipped, 1, "R-007 already done -> skipped");
    assert_eq!(report.rejected, vec!["R-006".to_string()]);
    // Per-item tolerance: R-005 applied even though R-006 was missing.
    assert_eq!(get_status(&conn, "R-005").as_deref(), Some("done"));
}

#[test]
fn reconcile_terminal_blocked_to_done_is_rejected_by_matrix() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "R-008", "blocked");

    let plan = ReconcilePlan {
        items: vec![ReconcileItem {
            task_id: "R-008".to_string(),
            target: TaskStatus::Done,
            audit_label: None,
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.reconcile_from_prd(plan).unwrap()
    };

    // Blocked → Done is not allowed under ReconcilePrd.
    assert_eq!(report.applied, 0);
    assert_eq!(report.rejected, vec!["R-008".to_string()]);
    assert_eq!(get_status(&conn, "R-008").as_deref(), Some("blocked"));
}

#[test]
fn reconcile_empty_plan_returns_zeros_no_writes() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "R-009", "todo");

    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.reconcile_from_prd(ReconcilePlan::default()).unwrap()
    };

    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 0);
    assert!(report.rejected.is_empty());
    assert_eq!(get_status(&conn, "R-009").as_deref(), Some("todo"));
}

// --- repair_stale ---

#[test]
fn repair_reset_stale_in_progress_to_todo() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "D-001", "in_progress");

    let plan = RepairPlan {
        items: vec![RepairItem {
            task_id: "D-001".to_string(),
            target: TaskStatus::Todo,
            audit_label: Some(
                "[DOCTOR] Reset from 'in_progress' to 'todo' - no active run".to_string(),
            ),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.repair_stale(plan).unwrap()
    };

    assert_eq!(report.applied, 1);
    assert_eq!(get_status(&conn, "D-001").as_deref(), Some("todo"));
    let notes = get_notes(&conn, "D-001").expect("notes should be set");
    assert!(
        notes.contains("[DOCTOR] Reset from 'in_progress' to 'todo'"),
        "audit label must be appended to notes: {notes}"
    );
}

#[test]
fn repair_mark_done_from_git_appends_label() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "D-002", "in_progress");

    let plan = RepairPlan {
        items: vec![RepairItem {
            task_id: "D-002".to_string(),
            target: TaskStatus::Done,
            audit_label: Some("[DOCTOR] Reconciled from git history - commit: abc123".to_string()),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.repair_stale(plan).unwrap()
    };

    assert_eq!(report.applied, 1);
    assert_eq!(get_status(&conn, "D-002").as_deref(), Some("done"));
    let notes = get_notes(&conn, "D-002").expect("notes should be set");
    assert!(notes.contains("abc123"));
}

#[test]
fn repair_audit_label_appends_to_existing_notes() {
    let (_dir, mut conn) = setup();
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, notes) VALUES (?, 'T', 'in_progress', 10, ?)",
        rusqlite::params!["D-003", "previous note"],
    )
    .unwrap();

    let plan = RepairPlan {
        items: vec![RepairItem {
            task_id: "D-003".to_string(),
            target: TaskStatus::Todo,
            audit_label: Some("[DOCTOR] Reset".to_string()),
        }],
    };
    {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.repair_stale(plan).unwrap();
    }

    let notes = get_notes(&conn, "D-003").unwrap();
    assert!(notes.starts_with("previous note"));
    assert!(notes.contains("[DOCTOR] Reset"));
    assert!(
        notes.contains("\n\n[DOCTOR] Reset"),
        "must be separated by blank line, got: {notes:?}"
    );
}

#[test]
fn repair_allows_done_target_from_todo_under_doctor_matrix() {
    let (_dir, mut conn) = setup();
    // DoctorRepair permits Todo -> Done for git-reconciliation: legacy SQL at
    // fixes.rs:93 had no WHERE-status clause, so a `todo` row with a matching
    // git commit also flipped to `done`. The matrix preserves that semantics.
    insert_task(&conn, "D-004", "todo");

    let plan = RepairPlan {
        items: vec![RepairItem {
            task_id: "D-004".to_string(),
            target: TaskStatus::Done,
            audit_label: Some("[DOCTOR] x".to_string()),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.repair_stale(plan).unwrap()
    };

    assert_eq!(report.applied, 1);
    assert!(report.rejected.is_empty());
    assert_eq!(get_status(&conn, "D-004").as_deref(), Some("done"));
}

#[test]
fn repair_partial_failure_continues_batch() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "D-005", "in_progress");
    insert_task(&conn, "D-007", "in_progress");
    // D-006 missing → rejected

    let plan = RepairPlan {
        items: vec![
            RepairItem {
                task_id: "D-005".to_string(),
                target: TaskStatus::Todo,
                audit_label: Some("[DOCTOR] a".to_string()),
            },
            RepairItem {
                task_id: "D-006".to_string(),
                target: TaskStatus::Todo,
                audit_label: Some("[DOCTOR] b".to_string()),
            },
            RepairItem {
                task_id: "D-007".to_string(),
                target: TaskStatus::Done,
                audit_label: Some("[DOCTOR] c".to_string()),
            },
        ],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.repair_stale(plan).unwrap()
    };

    assert_eq!(report.applied, 2);
    assert_eq!(report.rejected, vec!["D-006".to_string()]);
    assert_eq!(get_status(&conn, "D-005").as_deref(), Some("todo"));
    assert_eq!(get_status(&conn, "D-007").as_deref(), Some("done"));
}

#[test]
fn repair_empty_plan_returns_zeros() {
    let (_dir, mut conn) = setup();

    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.repair_stale(RepairPlan::default()).unwrap()
    };

    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 0);
    assert!(report.rejected.is_empty());
}

#[test]
fn repair_reset_stale_clears_started_at() {
    let (_dir, mut conn) = setup();
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, started_at) VALUES (?, 'T', 'in_progress', 10, '2000-01-01 00:00:00')",
        ["D-008"],
    )
    .unwrap();

    let plan = RepairPlan {
        items: vec![RepairItem {
            task_id: "D-008".to_string(),
            target: TaskStatus::Todo,
            audit_label: None,
        }],
    };
    {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.repair_stale(plan).unwrap();
    }

    let started_at: Option<String> = conn
        .query_row(
            "SELECT started_at FROM tasks WHERE id = ?",
            ["D-008"],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    assert!(started_at.is_none(), "started_at must be cleared on reset");
}
