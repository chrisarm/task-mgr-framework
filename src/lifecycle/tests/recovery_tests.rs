//! Unit tests for `TaskLifecycle` Category C recovery verbs.
//!
//! Mirrors the TEST-INIT-002 scenarios (today shadowed by in-test
//! wrappers in `loop_engine::engine::tests::recovery_primitives`) but
//! calls the actual service implementations in [`super::super::recovery`].

#![cfg(test)]

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, migrations::run_migrations, open_connection};
use crate::lifecycle::TaskLifecycle;

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

// --- recover_in_progress (per-id orphan reclaim) ---

/// Predicate matrix for orphan reclaim: only `in_progress` flips to `todo`.
/// Covers PRD §2.5 statuses that 17.5/17.6 and overflow rungs 1–3 must not
/// reopen (honest terminals, already-todo, missing).
#[test]
fn recover_in_progress_table_driven_status_predicate() {
    // (status_or_none, expect_applied, expect_final_status_or_none)
    // None id → missing row; final status N/A.
    let cases: &[(&str, Option<&str>, bool, Option<&str>)] = &[
        ("in_progress", Some("in_progress"), true, Some("todo")),
        ("done", Some("done"), false, Some("done")),
        ("blocked", Some("blocked"), false, Some("blocked")),
        ("skipped", Some("skipped"), false, Some("skipped")),
        ("irrelevant", Some("irrelevant"), false, Some("irrelevant")),
        ("todo", Some("todo"), false, Some("todo")),
        ("missing", None, false, None),
    ];

    for (label, initial, expect_applied, expect_final) in cases {
        let (_dir, mut conn) = setup();
        let id = "FEAT-REC-1";
        if let Some(status) = initial {
            insert_task(&conn, id, status);
            if *status == "in_progress" {
                conn.execute(
                    "UPDATE tasks SET started_at = datetime('now') WHERE id = ?",
                    [id],
                )
                .unwrap();
            }
        }

        let applied = {
            let lc = TaskLifecycle::new(&mut conn);
            lc.recover_in_progress(id).unwrap()
        };
        assert_eq!(
            applied, *expect_applied,
            "recover_in_progress({label}): Ok(true) iff one in_progress row updated"
        );

        match expect_final {
            None => {
                let exists: bool = conn
                    .query_row("SELECT COUNT(*) > 0 FROM tasks WHERE id = ?", [id], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert!(!exists, "missing row must stay missing ({label})");
            }
            Some(final_status) => {
                let (status, started): (String, Option<String>) = conn
                    .query_row(
                        "SELECT status, started_at FROM tasks WHERE id = ?",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(
                    status, *final_status,
                    "status after recover_in_progress({label})"
                );
                if *expect_applied {
                    assert!(
                        started.is_none(),
                        "started_at must clear when recover_in_progress applies ({label})"
                    );
                }
            }
        }
    }
}

// --- reopen_after_merge_fail (per-id merge-fail reopen) ---

/// Predicate matrix for merge-fail: `in_progress` and `done` reopen to `todo`.
/// Honest terminals and already-todo must no-op (PRD §2.5 merge-fail edges).
#[test]
fn reopen_after_merge_fail_table_driven_status_predicate() {
    let cases: &[(&str, Option<&str>, bool, Option<&str>)] = &[
        ("in_progress", Some("in_progress"), true, Some("todo")),
        ("done", Some("done"), true, Some("todo")),
        ("blocked", Some("blocked"), false, Some("blocked")),
        ("skipped", Some("skipped"), false, Some("skipped")),
        ("irrelevant", Some("irrelevant"), false, Some("irrelevant")),
        ("todo", Some("todo"), false, Some("todo")),
        ("missing", None, false, None),
    ];

    for (label, initial, expect_applied, expect_final) in cases {
        let (_dir, mut conn) = setup();
        let id = "FEAT-MRG-1";
        if let Some(status) = initial {
            insert_task(&conn, id, status);
            if *status == "in_progress" || *status == "done" {
                // done may still carry started_at from a prior claim; clear on apply.
                conn.execute(
                    "UPDATE tasks SET started_at = datetime('now') WHERE id = ?",
                    [id],
                )
                .unwrap();
            }
        }

        let applied = {
            let lc = TaskLifecycle::new(&mut conn);
            lc.reopen_after_merge_fail(id).unwrap()
        };
        assert_eq!(
            applied, *expect_applied,
            "reopen_after_merge_fail({label}): Ok(true) iff in_progress|done updated"
        );

        match expect_final {
            None => {
                let exists: bool = conn
                    .query_row("SELECT COUNT(*) > 0 FROM tasks WHERE id = ?", [id], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert!(!exists, "missing row must stay missing ({label})");
            }
            Some(final_status) => {
                let (status, started): (String, Option<String>) = conn
                    .query_row(
                        "SELECT status, started_at FROM tasks WHERE id = ?",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(
                    status, *final_status,
                    "status after reopen_after_merge_fail({label})"
                );
                if *expect_applied {
                    assert!(
                        started.is_none(),
                        "started_at must clear when reopen_after_merge_fail applies ({label})"
                    );
                }
            }
        }
    }
}

/// Pin: merge-fail of premature `:done` must reopen — a shared
/// `in_progress`-only helper would strand this row (known-bad).
#[test]
fn reopen_after_merge_fail_reopens_done_while_recover_in_progress_does_not() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-DONE", "done");

    let orphan = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.recover_in_progress("FEAT-DONE").unwrap()
    };
    assert!(
        !orphan,
        "recover_in_progress must not reopen done (orphan predicate)"
    );
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FEAT-DONE'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "done");

    let merge = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.reopen_after_merge_fail("FEAT-DONE").unwrap()
    };
    assert!(merge, "reopen_after_merge_fail must reopen done → todo");
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FEAT-DONE'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "todo");
}

// --- recover_in_progress_for_prefix ---

#[test]
fn recover_in_progress_unscoped_reverts_all_in_progress_to_todo() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "in_progress");
    insert_task(&conn, "FIX-2", "in_progress");
    insert_task(&conn, "FEAT-3", "done");
    conn.execute(
        "UPDATE tasks SET started_at = datetime('now') WHERE status = 'in_progress'",
        [],
    )
    .unwrap();

    let count = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.recover_in_progress_for_prefix(None).unwrap()
    };
    assert_eq!(count, 2, "both in_progress rows must be reset");

    for id in ["FEAT-1", "FIX-2"] {
        let (status, started): (String, Option<String>) = conn
            .query_row(
                "SELECT status, started_at FROM tasks WHERE id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "todo", "{id} must be reset to todo");
        assert!(started.is_none(), "{id} started_at must be cleared");
    }

    let done: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FEAT-3'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(done, "done", "terminal row must not be touched");
}

#[test]
fn recover_in_progress_prefix_scoped_only_touches_matching_rows() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "in_progress");
    insert_task(&conn, "FEAT-2", "in_progress");
    insert_task(&conn, "FIX-1", "in_progress");

    let count = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.recover_in_progress_for_prefix(Some("FEAT")).unwrap()
    };
    assert_eq!(count, 2, "only FEAT- rows in scope");

    let fix_status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FIX-1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        fix_status, "in_progress",
        "prefix scope MUST NOT leak across PRD boundaries",
    );
}

#[test]
fn recover_in_progress_empty_result_returns_zero() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "todo");
    insert_task(&conn, "FEAT-2", "done");

    let count = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.recover_in_progress_for_prefix(None).unwrap()
    };
    assert_eq!(count, 0, "no in_progress rows — no-op");

    let rows: Vec<(String, String)> = conn
        .prepare("SELECT id, status FROM tasks ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![
            ("FEAT-1".to_string(), "todo".to_string()),
            ("FEAT-2".to_string(), "done".to_string()),
        ],
    );
}

// --- auto_block_after_failures ---

#[test]
fn auto_block_after_failures_sets_blocked_when_in_progress() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "in_progress");

    let applied = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.auto_block_after_failures("FEAT-1", "max retries exceeded", 42)
            .unwrap()
    };
    assert!(applied, "in_progress→blocked transition must apply");

    let (status, last_err, blocked_iter): (String, String, i64) = conn
        .query_row(
            "SELECT status, last_error, blocked_at_iteration \
             FROM tasks WHERE id = 'FEAT-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "blocked");
    assert_eq!(
        last_err, "max retries exceeded",
        "free-form err must be stored verbatim",
    );
    assert_eq!(blocked_iter, 42, "iteration recorded for decay-tracking");
}

#[test]
fn auto_block_after_failures_is_noop_on_done_task() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "done");

    let applied = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.auto_block_after_failures("FEAT-1", "err", 7).unwrap()
    };
    assert!(!applied, "terminal Done must NOT be re-blocked");

    let (status, last_err): (String, Option<String>) = conn
        .query_row(
            "SELECT status, last_error FROM tasks WHERE id = 'FEAT-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "done", "row untouched");
    assert!(last_err.is_none(), "no last_error mutation on no-op path",);
}

#[test]
fn auto_block_after_failures_missing_task_returns_false() {
    let (_dir, mut conn) = setup();

    let applied = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.auto_block_after_failures("DOES-NOT-EXIST", "err", 1)
            .unwrap()
    };
    assert!(!applied, "missing row → rows_affected == 0 → Ok(false)");
}

// --- resurrect_for_iteration ---

#[test]
fn resurrect_for_iteration_flips_listed_ids_to_todo() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "in_progress");
    insert_task(&conn, "FEAT-2", "blocked");
    insert_task(&conn, "FEAT-3", "done");
    conn.execute(
        "UPDATE tasks SET started_at = datetime('now') WHERE id IN ('FEAT-1','FEAT-2')",
        [],
    )
    .unwrap();

    let count = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.resurrect_for_iteration(Some("FEAT-"), &["FEAT-1", "FEAT-2"])
            .unwrap()
    };
    assert_eq!(count, 2);

    for id in ["FEAT-1", "FEAT-2"] {
        let (status, started): (String, Option<String>) = conn
            .query_row(
                "SELECT status, started_at FROM tasks WHERE id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "todo", "{id}");
        assert!(started.is_none(), "{id} started_at must be cleared");
    }

    let unchanged: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FEAT-3'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(unchanged, "done");
}

#[test]
fn resurrect_for_iteration_prefix_filters_out_cross_prd_ids() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "in_progress");
    insert_task(&conn, "FIX-1", "in_progress");

    let count = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.resurrect_for_iteration(Some("FEAT-"), &["FEAT-1", "FIX-1"])
            .unwrap()
    };
    assert_eq!(count, 1, "only FEAT-1 reset");

    let fix_status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FIX-1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        fix_status, "in_progress",
        "cross-PRD id must be skipped at the boundary",
    );
}

#[test]
fn resurrect_with_model_override_resets_in_progress_and_sets_model() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-OVF-1", "in_progress");
    conn.execute(
        "UPDATE tasks SET model = 'claude-sonnet' WHERE id = 'FEAT-OVF-1'",
        [],
    )
    .unwrap();

    let applied = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.resurrect_with_model_override("FEAT-OVF-1", "grok-build")
            .unwrap()
    };
    assert!(applied);

    let (status, model): (String, String) = conn
        .query_row(
            "SELECT status, model FROM tasks WHERE id = 'FEAT-OVF-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "todo");
    assert_eq!(model, "grok-build");
}

#[test]
fn resurrect_for_iteration_empty_ids_returns_zero_without_db_write() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "in_progress");

    let count = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.resurrect_for_iteration(Some("FEAT-"), &[]).unwrap()
    };
    assert_eq!(count, 0, "empty slice short-circuits to Ok(0)");

    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FEAT-1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "in_progress", "row must be untouched");
}

#[test]
fn resurrect_for_iteration_no_prefix_resets_all_listed() {
    let (_dir, mut conn) = setup();
    insert_task(&conn, "FEAT-1", "in_progress");
    insert_task(&conn, "FIX-1", "in_progress");

    let count = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.resurrect_for_iteration(None, &["FEAT-1", "FIX-1"])
            .unwrap()
    };
    assert_eq!(count, 2, "no prefix scope → both rows reset");
}
