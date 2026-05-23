//! Unit tests for `decay_reset` (FEAT-001).

#![cfg(test)]

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, migrations::run_migrations, open_connection};
use crate::lifecycle::matrix::{TransitionSource, allowed_from_for_plan};
use crate::lifecycle::{DecayItem, DecayPlan, TaskLifecycle};
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

fn insert_task_with_decay_iter(
    conn: &Connection,
    id: &str,
    status: &str,
    blocked_at: Option<i64>,
    skipped_at: Option<i64>,
    notes: Option<&str>,
) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, blocked_at_iteration, \
         skipped_at_iteration, notes) VALUES (?, 'Test', ?, 10, ?, ?, ?)",
        rusqlite::params![id, status, blocked_at, skipped_at, notes],
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

fn get_decay_columns(conn: &Connection, id: &str) -> (Option<i64>, Option<i64>) {
    conn.query_row(
        "SELECT blocked_at_iteration, skipped_at_iteration FROM tasks WHERE id = ?",
        [id],
        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
    )
    .unwrap()
}

// ── core happy-path coverage ────────────────────────────────────────────────

#[test]
fn decay_reset_blocked_to_todo_clears_iteration_and_appends_audit() {
    let (_dir, mut conn) = setup();
    insert_task_with_decay_iter(&conn, "DEC-001", "blocked", Some(5), None, None);

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-001".to_string(),
            audit_label: "[DECAY] auto-reset".to_string(),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap()
    };

    assert_eq!(report.applied, 1);
    assert_eq!(report.skipped, 0);
    assert!(report.rejected.is_empty());
    assert_eq!(get_status(&conn, "DEC-001").as_deref(), Some("todo"));
    let (blocked_at, skipped_at) = get_decay_columns(&conn, "DEC-001");
    assert!(blocked_at.is_none(), "blocked_at_iteration must be cleared");
    assert!(skipped_at.is_none());
    assert_eq!(
        get_notes(&conn, "DEC-001").as_deref(),
        Some("[DECAY] auto-reset"),
        "audit note appended to empty-notes column"
    );
}

#[test]
fn decay_reset_skipped_to_todo_clears_iteration() {
    let (_dir, mut conn) = setup();
    insert_task_with_decay_iter(&conn, "DEC-002", "skipped", None, Some(7), None);

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-002".to_string(),
            audit_label: "[DECAY] auto-reset".to_string(),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap()
    };

    assert_eq!(report.applied, 1);
    assert_eq!(get_status(&conn, "DEC-002").as_deref(), Some("todo"));
    let (blocked_at, skipped_at) = get_decay_columns(&conn, "DEC-002");
    assert!(blocked_at.is_none());
    assert!(skipped_at.is_none(), "skipped_at_iteration must be cleared");
}

// ── matrix gating ──────────────────────────────────────────────────────────

#[test]
fn decay_reset_done_task_is_rejected_by_matrix() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "DEC-003", "done");

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-003".to_string(),
            audit_label: "[DECAY] should not apply".to_string(),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap()
    };

    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.rejected, vec!["DEC-003".to_string()]);
    // DB unchanged — the matrix gate stopped the UPDATE.
    assert_eq!(get_status(&conn, "DEC-003").as_deref(), Some("done"));
    assert!(
        get_notes(&conn, "DEC-003").is_none(),
        "notes must not be appended when the matrix rejects",
    );
}

#[test]
fn decay_reset_in_progress_task_is_rejected_by_matrix() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "DEC-004", "in_progress");

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-004".to_string(),
            audit_label: "[DECAY] x".to_string(),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap()
    };
    assert_eq!(report.rejected, vec!["DEC-004".to_string()]);
    assert_eq!(get_status(&conn, "DEC-004").as_deref(), Some("in_progress"));
}

#[test]
fn decay_reset_irrelevant_task_is_rejected_by_matrix() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "DEC-005", "irrelevant");

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-005".to_string(),
            audit_label: "[DECAY] x".to_string(),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap()
    };
    assert_eq!(report.rejected, vec!["DEC-005".to_string()]);
    assert_eq!(get_status(&conn, "DEC-005").as_deref(), Some("irrelevant"));
}

#[test]
fn decay_reset_todo_task_is_no_op_skipped() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "DEC-006", "todo");

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-006".to_string(),
            audit_label: "[DECAY] should be skipped".to_string(),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap()
    };

    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 1, "todo -> todo is a reflexive no-op");
    assert!(report.rejected.is_empty());
    assert_eq!(get_status(&conn, "DEC-006").as_deref(), Some("todo"));
    assert!(
        get_notes(&conn, "DEC-006").is_none(),
        "no-op must NOT append audit notes",
    );
}

// ── notes append CASE WHEN coverage ─────────────────────────────────────────

#[test]
fn decay_reset_with_empty_notes_writes_audit_alone() {
    let (_dir, mut conn) = setup();
    insert_task_with_decay_iter(&conn, "DEC-007", "blocked", Some(0), None, Some(""));

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-007".to_string(),
            audit_label: "audit".to_string(),
        }],
    };
    {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap();
    }
    assert_eq!(
        get_notes(&conn, "DEC-007").as_deref(),
        Some("audit"),
        "empty-string notes must produce 'audit' alone (no leading newlines)",
    );
}

#[test]
fn decay_reset_with_null_notes_writes_audit_alone() {
    let (_dir, mut conn) = setup();
    insert_task_with_decay_iter(&conn, "DEC-008", "blocked", Some(0), None, None);

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-008".to_string(),
            audit_label: "audit".to_string(),
        }],
    };
    {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap();
    }
    assert_eq!(get_notes(&conn, "DEC-008").as_deref(), Some("audit"));
}

#[test]
fn decay_reset_with_existing_notes_appends_with_blank_line() {
    let (_dir, mut conn) = setup();
    insert_task_with_decay_iter(&conn, "DEC-009", "blocked", Some(0), None, Some("existing"));

    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DEC-009".to_string(),
            audit_label: "audit".to_string(),
        }],
    };
    {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap();
    }
    assert_eq!(
        get_notes(&conn, "DEC-009").as_deref(),
        Some("existing\n\naudit"),
        "non-empty notes must be separated from the audit label by a blank line",
    );
}

// ── batch behaviour ─────────────────────────────────────────────────────────

#[test]
fn decay_reset_bulk_multi_item_applies_all_eligible() {
    let (_dir, mut conn) = setup();
    insert_task_with_decay_iter(&conn, "DEC-010", "blocked", Some(0), None, None);
    insert_task_with_decay_iter(&conn, "DEC-011", "skipped", None, Some(0), Some("prior"));
    // DEC-012 missing → rejected
    insert_task(&conn, "DEC-013", "done"); // matrix rejection
    insert_task(&conn, "DEC-014", "todo"); // skipped (no-op)

    let plan = DecayPlan {
        items: vec![
            DecayItem {
                task_id: "DEC-010".to_string(),
                audit_label: "[DECAY] a".to_string(),
            },
            DecayItem {
                task_id: "DEC-011".to_string(),
                audit_label: "[DECAY] b".to_string(),
            },
            DecayItem {
                task_id: "DEC-012".to_string(),
                audit_label: "[DECAY] c".to_string(),
            },
            DecayItem {
                task_id: "DEC-013".to_string(),
                audit_label: "[DECAY] d".to_string(),
            },
            DecayItem {
                task_id: "DEC-014".to_string(),
                audit_label: "[DECAY] e".to_string(),
            },
        ],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap()
    };

    assert_eq!(report.applied, 2, "DEC-010 + DEC-011");
    assert_eq!(report.skipped, 1, "DEC-014 (todo no-op)");
    assert_eq!(
        report.rejected,
        vec!["DEC-012".to_string(), "DEC-013".to_string()],
    );
    assert_eq!(get_status(&conn, "DEC-010").as_deref(), Some("todo"));
    assert_eq!(get_status(&conn, "DEC-011").as_deref(), Some("todo"));
    assert_eq!(
        get_notes(&conn, "DEC-011").as_deref(),
        Some("prior\n\n[DECAY] b"),
    );
    assert_eq!(get_status(&conn, "DEC-013").as_deref(), Some("done"));
    assert_eq!(get_status(&conn, "DEC-014").as_deref(), Some("todo"));
}

#[test]
fn decay_reset_empty_plan_returns_zeros_and_no_writes() {
    let (_dir, mut conn) = setup();
    insert_task_with_decay_iter(&conn, "DEC-015", "blocked", Some(99), None, Some("pre"));

    // Stamp a sentinel updated_at so we can verify no UPDATE ran.
    conn.execute(
        "UPDATE tasks SET updated_at = '1999-01-01 00:00:00' WHERE id = ?",
        ["DEC-015"],
    )
    .unwrap();
    let pre: String = conn
        .query_row(
            "SELECT updated_at FROM tasks WHERE id = ?",
            ["DEC-015"],
            |row| row.get(0),
        )
        .unwrap();

    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(DecayPlan::default()).unwrap()
    };
    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 0);
    assert!(report.rejected.is_empty());

    // Sentinel survives — proves no DB write fired for the empty plan.
    let post: String = conn
        .query_row(
            "SELECT updated_at FROM tasks WHERE id = ?",
            ["DEC-015"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        pre, post,
        "empty plan must perform zero DB writes (updated_at sentinel preserved)",
    );
    assert_eq!(get_status(&conn, "DEC-015").as_deref(), Some("blocked"));
}

#[test]
fn decay_reset_missing_row_is_rejected() {
    let (_dir, mut conn) = setup();
    let plan = DecayPlan {
        items: vec![DecayItem {
            task_id: "DOES-NOT-EXIST".to_string(),
            audit_label: "[DECAY] x".to_string(),
        }],
    };
    let report = {
        let mut lc = TaskLifecycle::new(&mut conn);
        lc.decay_reset(plan).unwrap()
    };
    assert_eq!(report.rejected, vec!["DOES-NOT-EXIST".to_string()]);
}

// ── allowed_from_for_plan matrix helper ─────────────────────────────────────

#[test]
fn allowed_from_for_plan_decay_reset_returns_blocked_and_skipped_into_todo() {
    let got = allowed_from_for_plan(TaskStatus::Todo, TransitionSource::DecayReset);
    let mut sorted: Vec<TaskStatus> = got.to_vec();
    sorted.sort_by_key(|s| format!("{s:?}"));
    let mut want = vec![TaskStatus::Blocked, TaskStatus::Skipped];
    want.sort_by_key(|s| format!("{s:?}"));
    assert_eq!(
        sorted, want,
        "decay reset enters Todo from blocked or skipped"
    );
}

#[test]
fn allowed_from_for_plan_matches_matrix_validate_for_each_target_source_pair() {
    use crate::lifecycle::matrix::validate as matrix_validate;

    let statuses = [
        TaskStatus::Todo,
        TaskStatus::InProgress,
        TaskStatus::Done,
        TaskStatus::Blocked,
        TaskStatus::Skipped,
        TaskStatus::Irrelevant,
    ];
    let plan_sources = [
        TransitionSource::ReconcilePrd,
        TransitionSource::DoctorRepair,
        TransitionSource::DecayReset,
    ];

    for target in statuses {
        for source in plan_sources {
            let allowed: Vec<TaskStatus> = allowed_from_for_plan(target, source).to_vec();

            for candidate in statuses {
                if candidate == target {
                    continue;
                }
                let matrix_ok = matrix_validate(candidate, target, source).is_ok();
                let in_helper = allowed.contains(&candidate);
                assert_eq!(
                    matrix_ok, in_helper,
                    "matrix and allowed_from_for_plan disagree for \
                     ({candidate:?} -> {target:?}, {source:?}): matrix={matrix_ok}, \
                     helper={in_helper}",
                );
            }
        }
    }
}
