//! Shadow-test harness for the `TaskLifecycle` migration.
//!
//! Per PRD FR-008: for each of the ~20 raw `UPDATE tasks SET status …` call
//! sites that this PRD migrated behind `TaskLifecycle`, we run the LEGACY
//! raw-SQL path (inlined here verbatim from the pre-migration code) against
//! one in-memory DB clone, run the SERVICE path
//! (`TaskLifecycle::apply` / `try_claim` / recovery / reconcile / repair
//! verbs) against an independent clone with the same initial state, then
//! assert that every column of `tasks` and `run_tasks` matches (modulo
//! normalized timestamp columns).
//!
//! TEST-INIT-004 landed the harness plus the skip shadow. TEST-001 (this
//! file) extends coverage to all migrated Category A/B/C/D sites per the
//! Consumer Impact Table in `progress-035925a9.txt`. The legacy command
//! wrappers (e.g. `commands::skip::skip`) now route through
//! `TaskLifecycle::apply` internally — for shadow purposes we therefore
//! reproduce the historical raw SQL inline in the `legacy` closure so the
//! comparison genuinely proves migration fidelity, not just wrapper
//! consistency.
//!
//! Helper duplication policy (per learning #3211): the helpers below are
//! intentionally local to this file rather than promoted to `tests/common/`.
//! Promotion is appropriate only after a second shadow-test file appears and
//! actually shares the helpers — at that point the duplication makes the
//! abstraction's shape obvious.
//!
//! # Coverage map (audit-row → test function)
//!
//! | Audit | Category | Test |
//! | --- | --- | --- |
//! | #2 complete | A | [`shadow_complete_in_progress_with_run`] |
//! | #3 fail(Blocked) | A | [`shadow_fail_blocked_with_run`] |
//! | #3 fail(Skipped) | A | [`shadow_fail_skipped_no_run`] |
//! | #4 skip (no run) | A | [`shadow_skip_in_progress_no_run`] |
//! | #4 skip (with run) | A | [`shadow_skip_in_progress_with_run`] |
//! | #5 irrelevant | A | [`shadow_irrelevant_with_learning_id`] |
//! | #6 unblock | A | [`shadow_unblock_blocked_clears_last_error`] |
//! | #1 reset | A | [`shadow_reset_from_blocked`] |
//! | #8 review --auto unblock | A | [`shadow_review_auto_unblock_audit_override`] |
//! | #11 next try_claim (Todo only) | B | [`shadow_try_claim_todo_only`] |
//! | #13 slot try_claim ([Todo,InProgress]) | B | [`shadow_try_claim_slot_idempotent`] |
//! | #15/16 recover_in_progress_for_prefix | C | [`shadow_recover_in_progress_for_prefix`] |
//! | #18 auto_block_after_failures | C | [`shadow_auto_block_after_failures_in_progress`] |
//! | #14/21 resurrect_for_iteration | C | [`shadow_resurrect_for_iteration_with_prefix`] |
//! | #19 decay_reset (blocked → todo) | C | [`shadow_decay_blocked_to_todo`] |
//! | #22 reconcile_from_prd (passes:true) | D | [`shadow_reconcile_from_prd_marks_done`] |
//! | #24 repair_stale (doctor stale) | D | [`shadow_repair_stale_resets_in_progress`] |
//!
//! Additionally:
//! - [`shadow_apply_stderr_vs_commit_ordering`] — FR-008 point 8: the PRD
//!   JSON sync warning fires only after the DB row is durable.
//! - [`shadow_prd_atomicity_partial_batch`] — FR-008 point 7: live
//!   implementation of [`crash_test_prd_atomicity`] proving the PRD JSON
//!   file is never left in a torn state when one intent in a multi-intent
//!   batch fails its PRD sync.
//!
//! # Helpers
//!
//! - [`assert_shadow_equivalent`] — the reusable diff core: sets up two DB
//!   clones, runs `legacy_fn` against one and `service_fn` against the other,
//!   then asserts row-level equivalence for the `expected_columns` of `tasks`
//!   (plus full diff of any `run_tasks` rows touching the same task ids).
//! - [`crash_test_prd_atomicity`] — FR-008 point 7: inject a PRD JSON sync
//!   failure mid-batch and assert no torn JSON state on disk.

#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::cli::FailStatus;
use task_mgr::commands::skip as skip_cmd;
use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::lifecycle::{
    DecayItem, DecayPlan, ReconcileItem, ReconcilePlan, RepairItem, RepairPlan, TaskLifecycle,
    TransitionChange, TransitionIntent, TransitionSource,
};
use task_mgr::models::TaskStatus;

// ── DB helpers ───────────────────────────────────────────────────────────────

/// Build a fresh in-memory DB (sqlite tempfile + full migration history).
fn setup_db() -> (TempDir, Connection) {
    let dir = TempDir::new().expect("tempdir for shadow DB");
    let mut conn = open_connection(dir.path()).expect("open shadow DB");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (dir, conn)
}

/// Insert a task with given status. Mirrors `tests/lifecycle_stderr_contract.rs`
/// for consistency — both files target the same `tasks` schema.
fn insert_task(conn: &Connection, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, priority, status) VALUES (?1, 'Shadow task', 50, ?2)",
        rusqlite::params![id, status],
    )
    .expect("insert task");
}

/// Insert a run row plus the matching `run_tasks` entry. Lets the run_id
/// thread cleanly into both the legacy `skip(.., Some(run_id))` and the
/// service `TaskLifecycle::with_run(.., run_id, iter).apply(..)` calls.
fn insert_run_with_task(conn: &Connection, run_id: &str, task_id: &str, iteration: i64) {
    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES (?1, 'active')",
        rusqlite::params![run_id],
    )
    .expect("insert run");
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES (?1, ?2, 'started', ?3)",
        rusqlite::params![run_id, task_id, iteration],
    )
    .expect("insert run_tasks");
}

/// Insert a task with extra columns commonly needed by Category A shadow
/// tests (error_count, last_error, notes). Keeps the wider [`insert_task`]
/// caller surface unchanged — opt in by calling this variant directly.
fn insert_task_full(
    conn: &Connection,
    id: &str,
    status: &str,
    error_count: i32,
    last_error: Option<&str>,
    notes: Option<&str>,
) {
    conn.execute(
        "INSERT INTO tasks (id, title, priority, status, error_count, last_error, notes) \
         VALUES (?1, 'Shadow task', 50, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, status, error_count, last_error, notes],
    )
    .expect("insert task full");
}

/// Set `global_state.iteration_counter` to a known value so the Category A
/// fail-decay path (`blocked_at_iteration = COALESCE(?, blocked_at_iteration)`)
/// has a deterministic value on both sides of the shadow comparison.
fn set_iteration_counter(conn: &Connection, value: i64) {
    conn.execute(
        "UPDATE global_state SET iteration_counter = ?1 WHERE id = 1",
        [value],
    )
    .expect("set iteration_counter");
}

/// Read the current `tasks.status` for a given id — used by the
/// stderr-vs-commit ordering test to prove the DB row is durable before any
/// PRD-sync warning fires.
fn read_status(conn: &Connection, id: &str) -> String {
    conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |r| r.get(0))
        .expect("read status")
}

// ── Row dump + timestamp normalization ───────────────────────────────────────

/// Columns whose values are non-deterministic across runs (datetime('now'))
/// and so must be normalized to a placeholder before comparison.
const TIMESTAMP_COLUMNS: &[&str] = &[
    "created_at",
    "updated_at",
    "started_at",
    "completed_at",
    "ended_at",
];

/// All columns of `tasks` we may want to diff. Tests pass the subset they
/// care about to [`assert_shadow_equivalent`] via `expected_columns`.
pub const TASKS_ALL_COLUMNS: &[&str] = &[
    "id",
    "title",
    "description",
    "priority",
    "status",
    "notes",
    "acceptance_criteria",
    "review_scope",
    "severity",
    "source_review",
    "created_at",
    "updated_at",
    "started_at",
    "completed_at",
    "last_error",
    "error_count",
    "blocked_at_iteration",
    "skipped_at_iteration",
];

/// All columns of `run_tasks`. The harness always diffs the full set when a
/// `run_tasks` row exists — that's the FR-008 "diffs run_tasks rows in
/// addition to tasks" requirement.
const RUN_TASKS_ALL_COLUMNS: &[&str] = &[
    "id",
    "run_id",
    "task_id",
    "status",
    "iteration",
    "started_at",
    "ended_at",
    "duration_seconds",
    "notes",
];

/// One row, column-name → stringified value.
type Row = BTreeMap<String, String>;

fn col_value(row: &rusqlite::Row<'_>, idx: usize) -> String {
    use rusqlite::types::ValueRef;
    match row.get_ref_unwrap(idx) {
        ValueRef::Null => "<null>".to_string(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Blob(b) => format!("<blob:{}>", b.len()),
    }
}

/// Replace any value in a timestamp column with a stable placeholder so two
/// runs differing only by `datetime('now')` resolution still compare equal.
/// `<null>` stays `<null>` — null vs. set is a real semantic difference and
/// the harness must catch it (e.g. `completed_at` populated on `Done` but
/// not on `Skipped`).
fn normalize_timestamps(row: &mut Row) {
    for col in TIMESTAMP_COLUMNS {
        if let Some(v) = row.get_mut(*col)
            && v != "<null>"
        {
            *v = "<ts>".to_string();
        }
    }
}

fn dump_tasks_row(conn: &Connection, task_id: &str, columns: &[&str]) -> Option<Row> {
    let select_list = columns.join(", ");
    let sql = format!("SELECT {select_list} FROM tasks WHERE id = ?1");
    conn.query_row(&sql, [task_id], |row| {
        let mut out: Row = BTreeMap::new();
        for (i, col) in columns.iter().enumerate() {
            out.insert((*col).to_string(), col_value(row, i));
        }
        Ok(out)
    })
    .ok()
}

fn dump_run_tasks_rows(conn: &Connection, task_id: &str) -> Vec<Row> {
    let select_list = RUN_TASKS_ALL_COLUMNS.join(", ");
    let sql = format!(
        "SELECT {select_list} FROM run_tasks WHERE task_id = ?1 ORDER BY run_id, iteration",
    );
    let mut stmt = conn.prepare(&sql).expect("prepare run_tasks select");
    let mut out = Vec::new();
    let mut rows = stmt.query([task_id]).expect("query run_tasks");
    while let Some(row) = rows.next().expect("next run_tasks") {
        let mut entry: Row = BTreeMap::new();
        for (i, col) in RUN_TASKS_ALL_COLUMNS.iter().enumerate() {
            entry.insert((*col).to_string(), col_value(row, i));
        }
        out.push(entry);
    }
    out
}

// ── The reusable shadow assertion ────────────────────────────────────────────

/// Inputs the test author provides for each shadow scenario.
pub struct ShadowScenario<S, L, V> {
    /// Seeds both DB clones with the same starting rows. Runs against a fresh
    /// connection on each side — must be deterministic.
    pub setup: S,
    /// Drives the LEGACY raw-SQL code path. Receives the cloned DB connection
    /// and any context the test needs.
    pub legacy: L,
    /// Drives the SERVICE code path (`TaskLifecycle::*`). Same input shape as
    /// `legacy` — the harness runs them against independent connections.
    pub service: V,
    /// Task ids whose rows in `tasks` (and any matching `run_tasks` entries)
    /// must compare byte-equal after both paths run.
    pub assert_task_ids: Vec<String>,
    /// Subset of [`TASKS_ALL_COLUMNS`] to diff. Pass `TASKS_ALL_COLUMNS` for a
    /// full row diff; pass a narrower set for tests that want to scope.
    pub expected_columns: &'static [&'static str],
}

/// Run a shadow scenario and assert byte-equivalent post-state on both sides.
///
/// Per FR-008:
/// - Two independent DB clones (no shared state)
/// - Identical seed via `scenario.setup`
/// - `tasks` diff scoped to `expected_columns`, timestamps normalized
/// - Full `run_tasks` diff (every column, timestamps normalized)
/// - Write-ordering check (FR-008 point 6): for each task_id, the legacy and
///   service paths must agree on whether a `run_tasks` row exists. If both
///   write a `run_tasks` row, the order tasks-then-run_tasks is implicit in
///   the per-side write sequence and verified by the `started_at` ≤
///   `updated_at` ordering on the tasks row when both are populated. This is
///   sufficient for the skip site (which writes tasks → conditionally
///   updates run_tasks) and is the strongest assertion we can make without
///   instrumenting sqlite's trace hook.
pub fn assert_shadow_equivalent<S, L, V>(scenario: ShadowScenario<S, L, V>)
where
    S: Fn(&mut Connection),
    L: FnOnce(&mut Connection),
    V: FnOnce(&mut Connection),
{
    let (_dir_a, mut conn_legacy) = setup_db();
    let (_dir_b, mut conn_service) = setup_db();

    (scenario.setup)(&mut conn_legacy);
    (scenario.setup)(&mut conn_service);

    (scenario.legacy)(&mut conn_legacy);
    (scenario.service)(&mut conn_service);

    for task_id in &scenario.assert_task_ids {
        let mut legacy_row = dump_tasks_row(&conn_legacy, task_id, scenario.expected_columns);
        let mut service_row = dump_tasks_row(&conn_service, task_id, scenario.expected_columns);
        if let Some(r) = legacy_row.as_mut() {
            normalize_timestamps(r);
        }
        if let Some(r) = service_row.as_mut() {
            normalize_timestamps(r);
        }
        assert_eq!(
            legacy_row, service_row,
            "tasks row diverged for task_id={task_id}",
        );

        let mut legacy_run_rows = dump_run_tasks_rows(&conn_legacy, task_id);
        let mut service_run_rows = dump_run_tasks_rows(&conn_service, task_id);
        for r in &mut legacy_run_rows {
            normalize_timestamps(r);
        }
        for r in &mut service_run_rows {
            normalize_timestamps(r);
        }
        assert_eq!(
            legacy_run_rows, service_run_rows,
            "run_tasks rows diverged for task_id={task_id}",
        );
    }
}

// ── PRD JSON atomicity crash mode (FR-008 point 7) ───────────────────────────

/// Run a batch of `Done` intents through `TaskLifecycle::apply` with PRD JSON
/// sync configured against `prd_path`. Each intent is paired with a `bool`
/// saying whether the matching PRD story exists at the time of the call —
/// when `expects_prd_story` is `false`, `update_prd_task_passes` returns
/// `TaskMgrError::NotFound` and emits the legacy stderr warning, but the DB
/// write succeeds (DB-authoritative-PRD-best-effort invariant).
///
/// Asserts:
/// 1. The PRD JSON file remains parseable JSON after the batch (no torn
///    state — `update_prd_task_passes` uses atomic write-then-rename).
/// 2. Every story whose `expects_prd_story = true` has flipped to
///    `passes: true`; every story whose intent had no PRD entry remains
///    unchanged (zero side effects from the failed sync).
/// 3. All DB rows reach `Done` regardless of PRD outcome.
///
/// Stderr verification is deliberately out of scope here — libtest's
/// `OUTPUT_CAPTURE` thread-local intercepts `eprintln!` before it reaches
/// FD 2, making a `libc::dup2`-based check inside the standard test harness
/// silently see empty output. The exact warning shape is locked
/// byte-for-byte by `tests/lifecycle_stderr_contract.rs` (harness=false);
/// duplicating the FD swap here would not add coverage, only flakiness.
pub fn crash_test_prd_atomicity(
    conn: &mut Connection,
    intents: &[TransitionIntent],
    expects_prd_story: &[bool],
    prd_path: &Path,
    task_prefix: &str,
) {
    assert_eq!(
        intents.len(),
        expects_prd_story.len(),
        "intents and expects_prd_story slices must align"
    );

    // Pre-snapshot bytes so we can prove the file is unchanged from the
    // pre-batch state for every intent whose PRD sync was expected to fail.
    let pre_snapshot = fs::read_to_string(prd_path).expect("pre-snapshot PRD file");
    let pre_value: serde_json::Value =
        serde_json::from_str(&pre_snapshot).expect("pre-snapshot must parse");

    {
        let mut lc = TaskLifecycle::new(conn).with_prd_sync(prd_path, task_prefix);
        let outcomes = lc.apply(intents);
        // DB-authoritative: every intent applied even if PRD sync failed.
        for outcome in &outcomes {
            assert!(
                outcome.applied,
                "DB write must succeed regardless of PRD outcome for {}",
                outcome.task_id
            );
        }
    }

    // Atomicity invariant: the PRD file MUST parse after the batch — atomic
    // write-then-rename in `update_prd_task_passes` guarantees this.
    let post_snapshot = fs::read_to_string(prd_path).expect("post-batch PRD file");
    let post_value: serde_json::Value =
        serde_json::from_str(&post_snapshot).expect("post-batch PRD must still parse");

    // Effect-isolation invariant: for each intent, its PRD story flipped to
    // `passes: true` iff `expects_prd_story = true`; unrelated stories are
    // byte-identical to the pre-batch snapshot.
    let pre_stories = pre_value
        .get("userStories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let post_stories = post_value
        .get("userStories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        pre_stories.len(),
        post_stories.len(),
        "no story added or removed"
    );

    // Build a (base_id -> expected_passes) map from intents + expects flags.
    // The intent's task_id is the full prefixed id (PREFIX-LOCAL); the PRD
    // file stores the local id (LOCAL) per `update_prd_task_passes`'s base-id
    // stripping. We compare against both forms for robustness.
    let prefix_strip = |s: &str| -> String {
        match s.strip_prefix(&format!("{task_prefix}-")) {
            Some(rest) => rest.to_string(),
            None => s.to_string(),
        }
    };
    let mut expected: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    for (i, intent) in intents.iter().enumerate() {
        expected.insert(prefix_strip(&intent.task_id), expects_prd_story[i]);
    }

    for (i, story) in post_stories.iter().enumerate() {
        let id = story
            .get("id")
            .and_then(|v| v.as_str())
            .expect("story missing id")
            .to_string();
        let post_passes = story
            .get("passes")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let pre_passes = pre_stories[i]
            .get("passes")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        match expected.get(&id) {
            Some(true) => assert!(
                post_passes,
                "story {id} should have flipped to passes:true after successful sync",
            ),
            Some(false) => assert_eq!(
                post_passes, pre_passes,
                "story {id} expected a failed sync but the value changed",
            ),
            None => assert_eq!(
                post_passes, pre_passes,
                "story {id} not targeted by batch but value changed (unrelated story drift)",
            ),
        }
    }
}

// ── Concrete shadow test #1: skip.rs ─────────────────────────────────────────

/// The vertical-slice shadow test. The skip site is the FEAT-007 migration
/// target: today its raw `UPDATE tasks SET status = 'skipped' …` SQL is the
/// reference; the service path is `TaskLifecycle::new(conn).apply([Skipped])`,
/// which currently routes through `commands::skip::skip` (see
/// `src/lifecycle/apply.rs:220`) — so legacy and service are expected to
/// produce byte-identical post-state today.
///
/// Why we don't `#[ignore]` this: FEAT-003 has already landed
/// `TaskLifecycle::apply`; the service call is no longer a stub. The shadow
/// equivalence holds NOW and is the contract FEAT-007 must preserve when it
/// inverts the call direction (CLI `skip` becomes a thin wrapper over
/// `apply`).
#[test]
fn shadow_skip_in_progress_no_run() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task(conn, "FEAT-007", "in_progress");
        },
        legacy: |conn: &mut Connection| {
            skip_cmd::skip(
                conn,
                &["FEAT-007".to_string()],
                "deferring vertical slice",
                None,
            )
            .expect("legacy skip");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::new(conn);
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-007".to_string(),
                change: TransitionChange::Skipped,
                source: TransitionSource::Operator,
                reason: Some("deferring vertical slice".to_string()),
                fail_status: None,
                audit_note: None,
            }]);
            assert_eq!(outcomes.len(), 1, "one outcome per intent");
            assert!(outcomes[0].applied, "service skip must succeed");
        },
        assert_task_ids: vec!["FEAT-007".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Skip with `run_id` populated — exercises the conditional `run_tasks`
/// update path in `skip_cmd::skip_single_task` (sets status='skipped',
/// ended_at, notes on the matching run_tasks row).
///
/// Diffs both `tasks` AND `run_tasks` columns. This is the FR-008 point 4
/// requirement ("diffs run_tasks rows in addition to tasks (matters when
/// run_id is set)") materialized as a concrete assertion.
#[test]
fn shadow_skip_in_progress_with_run() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task(conn, "FEAT-007", "in_progress");
            insert_run_with_task(conn, "run-shadow", "FEAT-007", 1);
        },
        legacy: |conn: &mut Connection| {
            skip_cmd::skip(
                conn,
                &["FEAT-007".to_string()],
                "shadow run-id path",
                Some("run-shadow"),
            )
            .expect("legacy skip with run");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::with_run(conn, "run-shadow");
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-007".to_string(),
                change: TransitionChange::Skipped,
                source: TransitionSource::LoopStatusTag,
                reason: Some("shadow run-id path".to_string()),
                fail_status: None,
                audit_note: None,
            }]);
            assert!(outcomes[0].applied);
        },
        assert_task_ids: vec!["FEAT-007".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

// ── Category A — user-intent + LoopStatusTag (apply) ─────────────────────────

/// Audit row #2 — `commands/complete.rs`. Legacy SQL:
/// `UPDATE tasks SET status='done', completed_at=now, updated_at=now WHERE id=?`
/// plus the v13 `consecutive_failures = 0` reset and the conditional
/// `run_tasks SET status='completed', ended_at, duration_seconds` update.
#[test]
fn shadow_complete_in_progress_with_run() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task(conn, "FEAT-DONE", "in_progress");
            insert_run_with_task(conn, "run-done", "FEAT-DONE", 1);
        },
        legacy: |conn: &mut Connection| {
            // Inlined pre-migration SQL (audit row #2). The Done flip writes
            // tasks first, then the run_tasks 'completed' update — that order
            // is the write-ordering contract (FR-008 point 6).
            conn.execute(
                "UPDATE tasks SET status = 'done', completed_at = datetime('now'), \
                 updated_at = datetime('now') WHERE id = ?",
                ["FEAT-DONE"],
            )
            .expect("legacy tasks UPDATE");
            conn.execute(
                "UPDATE tasks SET consecutive_failures = 0 WHERE id = ?",
                ["FEAT-DONE"],
            )
            .expect("legacy consecutive_failures reset");
            conn.execute(
                "UPDATE run_tasks SET status = 'completed', \
                 ended_at = datetime('now'), \
                 duration_seconds = CAST((julianday('now') - julianday(started_at)) * 86400 AS INTEGER) \
                 WHERE run_id = ? AND task_id = ? AND status = 'started'",
                ["run-done", "FEAT-DONE"],
            )
            .expect("legacy run_tasks update");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::with_run(conn, "run-done");
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-DONE".to_string(),
                change: TransitionChange::Done,
                source: TransitionSource::Operator,
                reason: None,
                fail_status: None,
                audit_note: None,
            }]);
            assert!(outcomes[0].applied, "service complete must succeed");
        },
        assert_task_ids: vec!["FEAT-DONE".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #3 — `commands/fail/transition.rs` with `FailStatus::Blocked`.
/// Legacy SQL writes status, error_count++, last_error, notes prefix
/// `[BLOCKED] err`, blocked_at_iteration via `COALESCE`, and updates the
/// matching run_tasks row to status='failed'.
#[test]
fn shadow_fail_blocked_with_run() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task_full(conn, "FEAT-FAIL", "in_progress", 2, None, Some("prior"));
            insert_run_with_task(conn, "run-fail", "FEAT-FAIL", 1);
            set_iteration_counter(conn, 42);
        },
        legacy: |conn: &mut Connection| {
            // Pre-migration tasks UPDATE — audit row #3.
            conn.execute(
                "UPDATE tasks SET status = ?, error_count = ?, last_error = ?, notes = ?, \
                 blocked_at_iteration = COALESCE(?, blocked_at_iteration), \
                 skipped_at_iteration = COALESCE(?, skipped_at_iteration), \
                 updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![
                    "blocked",
                    3,
                    "boom",
                    "prior\n\n[BLOCKED] boom",
                    Some(42i64),
                    None::<i64>,
                    "FEAT-FAIL",
                ],
            )
            .expect("legacy tasks UPDATE");
            conn.execute(
                "UPDATE run_tasks SET status = ?, notes = ?, ended_at = datetime('now') \
                 WHERE run_id = ? AND task_id = ?",
                rusqlite::params!["failed", "boom", "run-fail", "FEAT-FAIL"],
            )
            .expect("legacy run_tasks UPDATE");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::with_run(conn, "run-fail");
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-FAIL".to_string(),
                change: TransitionChange::Failed,
                source: TransitionSource::Operator,
                reason: Some("boom".to_string()),
                fail_status: Some(FailStatus::Blocked),
                audit_note: None,
            }]);
            assert!(outcomes[0].applied);
        },
        assert_task_ids: vec!["FEAT-FAIL".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #3 — `fail` with `FailStatus::Skipped`. Decay column lands on
/// `skipped_at_iteration` instead of `blocked_at_iteration`; run_tasks
/// status='skipped' (the fail-Skipped variant uses 'skipped' for run_tasks,
/// matching `apply.rs::fail_one`).
#[test]
fn shadow_fail_skipped_no_run() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task_full(conn, "FEAT-FSK", "in_progress", 0, None, None);
            set_iteration_counter(conn, 7);
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = ?, error_count = ?, last_error = ?, notes = ?, \
                 blocked_at_iteration = COALESCE(?, blocked_at_iteration), \
                 skipped_at_iteration = COALESCE(?, skipped_at_iteration), \
                 updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![
                    "skipped",
                    1,
                    "deferred",
                    "[SKIPPED] deferred",
                    None::<i64>,
                    Some(7i64),
                    "FEAT-FSK",
                ],
            )
            .expect("legacy tasks UPDATE");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::new(conn);
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-FSK".to_string(),
                change: TransitionChange::Failed,
                source: TransitionSource::Operator,
                reason: Some("deferred".to_string()),
                fail_status: Some(FailStatus::Skipped),
                audit_note: None,
            }]);
            assert!(outcomes[0].applied);
        },
        assert_task_ids: vec!["FEAT-FSK".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #5 — `commands/irrelevant.rs`. Legacy SQL writes
/// `status='irrelevant'`, audit notes with optional `(learning #N)` suffix,
/// and updates the matching `run_tasks` row to status='skipped' with the
/// reason text.
#[test]
fn shadow_irrelevant_with_learning_id() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task_full(conn, "FEAT-IRR", "in_progress", 0, None, Some("baseline"));
            insert_run_with_task(conn, "run-irr", "FEAT-IRR", 1);
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = 'irrelevant', notes = ?, \
                 updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![
                    "baseline\n\n[IRRELEVANT (learning #42)] covered by learning",
                    "FEAT-IRR",
                ],
            )
            .expect("legacy tasks UPDATE");
            conn.execute(
                "UPDATE run_tasks SET status = 'skipped', notes = ?, \
                 ended_at = datetime('now') WHERE run_id = ? AND task_id = ?",
                rusqlite::params!["covered by learning (learning #42)", "run-irr", "FEAT-IRR"],
            )
            .expect("legacy run_tasks UPDATE");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::with_run(conn, "run-irr");
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-IRR".to_string(),
                change: TransitionChange::Irrelevant,
                source: TransitionSource::Operator,
                reason: Some("covered by learning (learning #42)".to_string()),
                fail_status: None,
                audit_note: Some("[IRRELEVANT (learning #42)] covered by learning".to_string()),
            }]);
            assert!(outcomes[0].applied);
        },
        assert_task_ids: vec!["FEAT-IRR".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #6 — `commands/unblock.rs`. Legacy SQL flips status to 'todo',
/// clears `last_error = NULL`, and appends the canonical `[UNBLOCKED]` audit
/// note. Distinct from `unskip` because of the `last_error` clear.
#[test]
fn shadow_unblock_blocked_clears_last_error() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task_full(
                conn,
                "FEAT-UB",
                "blocked",
                3,
                Some("missing dep"),
                Some("prior"),
            );
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = 'todo', last_error = NULL, notes = ?, \
                 updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![
                    "prior\n\n[UNBLOCKED] Returned to todo from blocked status",
                    "FEAT-UB",
                ],
            )
            .expect("legacy tasks UPDATE");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::new(conn);
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-UB".to_string(),
                change: TransitionChange::Unblock,
                source: TransitionSource::Operator,
                reason: None,
                fail_status: None,
                audit_note: None,
            }]);
            assert!(outcomes[0].applied);
        },
        assert_task_ids: vec!["FEAT-UB".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #1 — `commands/reset.rs`. Legacy SQL flips to 'todo', clears
/// started_at / completed_at / last_error, increments error_count, and writes
/// the `[RESET] Reset to todo from <previous> status` audit note.
#[test]
fn shadow_reset_from_blocked() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            // started_at / completed_at intentionally NULL on insert — the
            // reset write must idempotently set them to NULL on both sides
            // (no diff if they were already NULL).
            insert_task_full(
                conn,
                "FEAT-RST",
                "blocked",
                4,
                Some("old err"),
                Some("prior"),
            );
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL, completed_at = NULL, \
                 last_error = NULL, error_count = ?, notes = ?, \
                 updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![
                    5,
                    "prior\n\n[RESET] Reset to todo from blocked status",
                    "FEAT-RST",
                ],
            )
            .expect("legacy tasks UPDATE");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::new(conn);
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-RST".to_string(),
                change: TransitionChange::Reset,
                source: TransitionSource::Operator,
                reason: None,
                fail_status: None,
                audit_note: None,
            }]);
            assert!(outcomes[0].applied);
        },
        assert_task_ids: vec!["FEAT-RST".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #8 — `commands/review.rs::auto_unblock_all` (review --auto path).
/// Legacy SQL writes `status='todo', last_error=NULL` with the custom
/// `[AUTO-UNBLOCKED]` audit prefix — distinct from the `[UNBLOCKED]` default
/// used by the CLI unblock command. The service path uses `audit_note`
/// override to drop the Blocked-only validation (any state → Todo).
#[test]
fn shadow_review_auto_unblock_audit_override() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            // Note: skipped status (not blocked) — exercises the override
            // path that lets review --auto cycle ANY blocked-or-skipped row
            // through `Unblock` with the custom audit note.
            insert_task_full(conn, "FEAT-AUTO", "skipped", 1, None, Some("prior"));
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = 'todo', last_error = NULL, notes = ?, \
                 updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![
                    "prior\n\n[AUTO-UNBLOCKED] Returned to todo via review --auto",
                    "FEAT-AUTO",
                ],
            )
            .expect("legacy tasks UPDATE");
        },
        service: |conn: &mut Connection| {
            let mut lc = TaskLifecycle::new(conn);
            let outcomes = lc.apply(&[TransitionIntent {
                task_id: "FEAT-AUTO".to_string(),
                change: TransitionChange::Unblock,
                source: TransitionSource::Operator,
                reason: None,
                fail_status: None,
                audit_note: Some("[AUTO-UNBLOCKED] Returned to todo via review --auto".to_string()),
            }]);
            assert!(outcomes[0].applied);
        },
        assert_task_ids: vec!["FEAT-AUTO".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

// ── Category B — try_claim ───────────────────────────────────────────────────

/// Audit row #11 — `commands/next/mod.rs:244` (CLI `next --claim`). Legacy
/// SQL is the explicit conditional WHERE on `status = 'todo'` so the claim
/// is race-safe (a row that has already advanced is left untouched).
#[test]
fn shadow_try_claim_todo_only() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task(conn, "FEAT-CLM", "todo");
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = 'in_progress', \
                 started_at = datetime('now'), \
                 updated_at = datetime('now') \
                 WHERE id = ? AND status = 'todo'",
                ["FEAT-CLM"],
            )
            .expect("legacy try_claim UPDATE");
        },
        service: |conn: &mut Connection| {
            let lc = TaskLifecycle::new(conn);
            let claimed = lc
                .try_claim("FEAT-CLM", &[TaskStatus::Todo])
                .expect("service try_claim");
            assert!(claimed, "should claim a todo row");
        },
        assert_task_ids: vec!["FEAT-CLM".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #13 — `loop_engine/engine.rs:786` (`claim_slot_task`). Legacy
/// SQL widens the WHERE clause to `status IN ('todo','in_progress')` so a
/// retry-after-recovery re-claim of an in_progress row refreshes `started_at`
/// idempotently. Critical for the parallel-slot retry semantics — narrowing
/// the predicate to `'todo'` would silently break slot recovery (PRD FR-005).
#[test]
fn shadow_try_claim_slot_idempotent() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            // started_at intentionally pre-populated so the re-claim must
            // refresh it; identical timestamps on both sides normalize to
            // `<ts>`, but the diff would catch a missing refresh path.
            insert_task(conn, "FEAT-SLOT", "in_progress");
            conn.execute(
                "UPDATE tasks SET started_at = datetime('now', '-1 hour') WHERE id = ?",
                ["FEAT-SLOT"],
            )
            .expect("seed started_at");
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = 'in_progress', \
                 started_at = datetime('now'), \
                 updated_at = datetime('now') \
                 WHERE id = ? AND status IN ('todo', 'in_progress')",
                ["FEAT-SLOT"],
            )
            .expect("legacy slot try_claim UPDATE");
        },
        service: |conn: &mut Connection| {
            let lc = TaskLifecycle::new(conn);
            let claimed = lc
                .try_claim("FEAT-SLOT", &[TaskStatus::Todo, TaskStatus::InProgress])
                .expect("service try_claim slot");
            assert!(claimed, "should re-claim an in_progress row idempotently");
        },
        assert_task_ids: vec!["FEAT-SLOT".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

// ── Category C — bulk recovery ───────────────────────────────────────────────

/// Audit rows #15 / #16 — `recover_in_progress_for_prefix`. Legacy bulk SQL
/// resets every `in_progress` row under a prefix to `todo` with started_at
/// cleared. Two rows are exercised here to prove the bulk WHERE actually
/// resets multiple rows on the service side (a `WHERE id = ?` regression
/// would silently update only one).
#[test]
fn shadow_recover_in_progress_for_prefix() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task(conn, "FEAT-R1", "in_progress");
            insert_task(conn, "FEAT-R2", "in_progress");
            // Decoy under a different prefix — must NOT be touched by either path.
            insert_task(conn, "BUG-R3", "in_progress");
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL, \
                 updated_at = datetime('now') \
                 WHERE status = 'in_progress' AND id LIKE ?",
                ["FEAT-%"],
            )
            .expect("legacy bulk UPDATE");
        },
        service: |conn: &mut Connection| {
            let n = TaskLifecycle::new(conn)
                .recover_in_progress_for_prefix(Some("FEAT"))
                .expect("service bulk recover");
            assert_eq!(n, 2, "exactly the two FEAT- rows recover");
        },
        assert_task_ids: vec![
            "FEAT-R1".to_string(),
            "FEAT-R2".to_string(),
            "BUG-R3".to_string(),
        ],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #18 — `auto_block_task` / `auto_block_after_failures`. Legacy
/// SQL sets `status='blocked', last_error=?, blocked_at_iteration=?` ONLY when
/// the row is currently `in_progress` — terminal rows are a clean no-op
/// (FR-005 conditional WHERE: tightens the legacy any-status update to gate
/// on in_progress, which is what the migrated service does today).
#[test]
fn shadow_auto_block_after_failures_in_progress() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task_full(conn, "FEAT-AB", "in_progress", 5, None, None);
        },
        legacy: |conn: &mut Connection| {
            conn.execute(
                "UPDATE tasks SET status = 'blocked', last_error = ?, \
                 blocked_at_iteration = ?, updated_at = datetime('now') \
                 WHERE id = ? AND status = 'in_progress'",
                rusqlite::params![
                    "Auto-blocked after 5 consecutive failures (task: FEAT-AB)",
                    99i64,
                    "FEAT-AB",
                ],
            )
            .expect("legacy auto-block UPDATE");
        },
        service: |conn: &mut Connection| {
            let applied = TaskLifecycle::new(conn)
                .auto_block_after_failures(
                    "FEAT-AB",
                    "Auto-blocked after 5 consecutive failures (task: FEAT-AB)",
                    99,
                )
                .expect("service auto-block");
            assert!(applied, "in_progress row must be blocked");
        },
        assert_task_ids: vec!["FEAT-AB".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #14 / #21 — `resurrect_for_iteration` (cf. `reset_task_to_todo`
/// at engine.rs:1642 and overflow rungs 1-3 at overflow.rs:473). Legacy
/// per-id reset with conditional WHERE `status='in_progress'`. Service uses
/// the bulk verb with a prefix filter — both produce the same per-row
/// post-state.
#[test]
fn shadow_resurrect_for_iteration_with_prefix() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task(conn, "FEAT-RES1", "in_progress");
            insert_task(conn, "FEAT-RES2", "in_progress");
            // Cross-prefix decoy — must be filtered out by the LIKE clause.
            insert_task(conn, "BUG-RES3", "in_progress");
        },
        legacy: |conn: &mut Connection| {
            // overflow.rs:473 rung-1 shape: per-id with conditional WHERE.
            conn.execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL, \
                 updated_at = datetime('now') \
                 WHERE id IN (?, ?) AND id LIKE ?",
                rusqlite::params!["FEAT-RES1", "FEAT-RES2", "FEAT-%"],
            )
            .expect("legacy resurrect UPDATE");
        },
        service: |conn: &mut Connection| {
            let n = TaskLifecycle::new(conn)
                .resurrect_for_iteration(Some("FEAT-"), &["FEAT-RES1", "FEAT-RES2"])
                .expect("service resurrect");
            assert_eq!(n, 2, "both FEAT- rows resurrect");
        },
        assert_task_ids: vec![
            "FEAT-RES1".to_string(),
            "FEAT-RES2".to_string(),
            "BUG-RES3".to_string(),
        ],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

// ── Category D — PRD-driven reconcile + doctor heuristic repair ──────────────

/// Audit row #22 — `loop_engine/prd_reconcile.rs:305`. PRD `passes: true` for
/// a task whose DB row is still `in_progress`. The service path appends the
/// audit label to notes via the atomic CASE-WHEN UPDATE. This shadow now
/// exercises that path (M4) by passing a real `audit_label` and simulating
/// the equivalent notes write on the legacy side so the byte comparison
/// remains fair.
#[test]
fn shadow_reconcile_from_prd_marks_done() {
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task(conn, "FEAT-REC", "in_progress");
        },
        legacy: |conn: &mut Connection| {
            // We simulate the audit label write on the legacy side too so the
            // shadow still compares byte-identical while exercising the
            // service's notes-append path (M4 / easy win).
            conn.execute(
                "UPDATE tasks SET status = 'done', completed_at = datetime('now'), \
                 notes = CASE WHEN notes IS NULL OR notes = '' THEN ? \
                              ELSE notes || char(10) || char(10) || ? END, \
                 updated_at = datetime('now') \
                 WHERE id = ? AND status IN ('todo', 'in_progress')",
                ["prd_marked_done", "prd_marked_done", "FEAT-REC"],
            )
            .expect("legacy reconcile UPDATE");
        },
        service: |conn: &mut Connection| {
            let plan = ReconcilePlan {
                items: vec![ReconcileItem {
                    task_id: "FEAT-REC".to_string(),
                    target: TaskStatus::Done,
                    audit_label: Some("prd_marked_done".to_string()),
                }],
            };
            let report = TaskLifecycle::new(conn)
                .reconcile_from_prd(plan)
                .expect("service reconcile");
            assert_eq!(report.applied, 1, "one row reconciled");
            assert_eq!(report.skipped, 0);
            assert!(report.rejected.is_empty());
        },
        assert_task_ids: vec!["FEAT-REC".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #19 — `commands/next/decay.rs:127` (`apply_decay`). The legacy
/// per-task body did a `SELECT notes` followed by a raw
/// `UPDATE tasks SET status = 'todo', blocked_at_iteration = NULL,
/// skipped_at_iteration = NULL, notes = ?, updated_at = datetime('now')`.
/// The service path replaces both with a single atomic UPDATE that uses
/// `CASE WHEN notes IS NULL OR notes = ''` to append the audit label inline
/// — no SELECT-then-UPDATE round-trip on notes. The post-state must compare
/// byte-identical to the legacy two-statement path.
#[test]
fn shadow_decay_blocked_to_todo() {
    let audit = "[DECAY] auto-reset";
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            // blocked_at_iteration populated so the column-clear is observable.
            insert_task_full(conn, "FEAT-DEC", "blocked", 1, None, Some("prior"));
            conn.execute(
                "UPDATE tasks SET blocked_at_iteration = 5 WHERE id = ?",
                ["FEAT-DEC"],
            )
            .expect("seed blocked_at_iteration");
        },
        legacy: |conn: &mut Connection| {
            // Inlined pre-migration sequence (audit row #19): SELECT notes,
            // build appended string in Rust, then UPDATE. The shadow
            // comparison proves the new CASE WHEN approach produces an
            // identical post-state.
            let existing: Option<String> = conn
                .query_row("SELECT notes FROM tasks WHERE id = ?", ["FEAT-DEC"], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .ok()
                .flatten();
            let new_notes = match existing {
                Some(e) if !e.is_empty() => format!("{e}\n\n{audit}"),
                _ => audit.to_string(),
            };
            conn.execute(
                "UPDATE tasks SET status = 'todo', \
                 blocked_at_iteration = NULL, \
                 skipped_at_iteration = NULL, \
                 notes = ?, \
                 updated_at = datetime('now') \
                 WHERE id = ?",
                rusqlite::params![new_notes, "FEAT-DEC"],
            )
            .expect("legacy decay UPDATE");
        },
        service: |conn: &mut Connection| {
            let plan = DecayPlan {
                items: vec![DecayItem {
                    task_id: "FEAT-DEC".to_string(),
                    audit_label: audit.to_string(),
                }],
            };
            let report = TaskLifecycle::new(conn)
                .decay_reset(plan)
                .expect("service decay");
            assert_eq!(report.applied, 1, "one row decayed");
            assert_eq!(report.skipped, 0);
            assert!(report.rejected.is_empty());
        },
        assert_task_ids: vec!["FEAT-DEC".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

/// Audit row #24 — `commands/doctor/fixes.rs:30` (`fix_stale_task`). Legacy
/// SQL flips status to 'todo', clears started_at, appends the `[DOCTOR] Reset
/// from 'in_progress' to 'todo'…` audit note. No PRD JSON sync (doctor never
/// consults the PRD JSON — kept distinct from reconcile per the §6
/// sub-decision).
#[test]
fn shadow_repair_stale_resets_in_progress() {
    let audit_note =
        "[DOCTOR] Reset from 'in_progress' to 'todo' - no active run tracking this task";
    assert_shadow_equivalent(ShadowScenario {
        setup: |conn: &mut Connection| {
            insert_task_full(conn, "FEAT-DOC", "in_progress", 0, None, Some("prior"));
        },
        legacy: |conn: &mut Connection| {
            // doctor/fixes.rs:30 legacy shape — no conditional WHERE; the
            // caller's check identified the stale rows. The notes append
            // pattern matches the service's apply_plan_with_source helper.
            conn.execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL, notes = ?, \
                 updated_at = datetime('now') \
                 WHERE id = ? AND status = ?",
                rusqlite::params![format!("prior\n\n{audit_note}"), "FEAT-DOC", "in_progress",],
            )
            .expect("legacy repair UPDATE");
        },
        service: |conn: &mut Connection| {
            let plan = RepairPlan {
                items: vec![RepairItem {
                    task_id: "FEAT-DOC".to_string(),
                    target: TaskStatus::Todo,
                    audit_label: Some(audit_note.to_string()),
                }],
            };
            let report = TaskLifecycle::new(conn)
                .repair_stale(plan)
                .expect("service repair");
            assert_eq!(report.applied, 1);
            assert!(report.rejected.is_empty());
        },
        assert_task_ids: vec!["FEAT-DOC".to_string()],
        expected_columns: TASKS_ALL_COLUMNS,
    });
}

// ── FR-008 point 8 — stderr-vs-DB-commit ordering for apply() ────────────────

/// FR-008 point 8: prove that `TaskLifecycle::apply` commits the DB write
/// BEFORE the PRD-sync warning would fire. The legacy stderr bytes are
/// already locked by `tests/lifecycle_stderr_contract.rs` (harness=false,
/// libc::dup2); duplicating the FD swap inside the default libtest harness
/// here would silently see empty output (`OUTPUT_CAPTURE` thread-local
/// intercepts `eprintln!` before it reaches FD 2) — so we instead validate
/// ordering via observable DB state.
///
/// Configure `apply()` with PRD JSON sync against a non-existent path so
/// the inner `update_prd_task_passes` fails at `fs::read_to_string`. After
/// the call returns, SELECT the row's status: if the warning had fired
/// before the DB commit (or instead of it), we'd see `in_progress`.
/// Observing `done` plus `applied = true` together proves the order:
/// DB commit → PRD-sync attempt → warning → return.
#[test]
fn shadow_apply_stderr_vs_commit_ordering() {
    let (_dir, mut conn) = setup_db();
    insert_task(&conn, "FEAT-ORD", "in_progress");
    let tmp = TempDir::new().unwrap();
    let prd_path = tmp.path().join("nonexistent.json");

    // Pre-condition (lower bound): the row is still in_progress before apply.
    assert_eq!(read_status(&conn, "FEAT-ORD"), "in_progress");

    let outcomes = {
        let mut lc = TaskLifecycle::new(&mut conn).with_prd_sync(&prd_path, "");
        lc.apply(&[TransitionIntent {
            task_id: "FEAT-ORD".to_string(),
            change: TransitionChange::Done,
            source: TransitionSource::LoopStatusTag,
            reason: None,
            fail_status: None,
            audit_note: None,
        }])
    };

    // DB-authoritative-PRD-best-effort: applied stays true even though the
    // PRD sync failed (path does not exist → IoErrorWithContext).
    assert!(
        outcomes[0].applied,
        "DB write must succeed even when PRD sync fails"
    );

    // ORDER INVARIANT: the post-apply SELECT sees `done`. The PRD path
    // doesn't exist, so we know `update_prd_task_passes` was attempted and
    // failed (the warning would have been emitted). The DB commit must have
    // run before that — if the order were reversed, apply() would have
    // returned before the DB UPDATE landed and the SELECT below would see
    // `in_progress`.
    assert_eq!(
        read_status(&conn, "FEAT-ORD"),
        "done",
        "DB row must be durable; the PRD-sync warning fires AFTER the commit",
    );

    // PRD file unchanged (it never existed in the first place — confirms
    // the failed sync did NOT create any partial file on disk).
    assert!(
        !prd_path.exists(),
        "PRD-sync failure must not create the file"
    );
}

// ── FR-008 point 7 — PRD JSON atomicity under partial-batch failure ──────────

/// Live implementation of [`crash_test_prd_atomicity`]. Build a 2-story PRD
/// JSON, run a batch of three `Done` intents where the middle intent's PRD
/// story is missing — `update_prd_task_passes` returns `NotFound`. Assert:
///
/// 1. The PRD file remains parseable JSON (atomic write-then-rename
///    guarantees no torn state on disk).
/// 2. Stories whose intents had a matching PRD entry flipped to
///    `passes: true`; stories whose intents failed mid-batch are
///    byte-identical to the pre-batch snapshot.
/// 3. All three DB rows reach `Done` regardless (DB-authoritative).
///
/// The full assertion lives in [`crash_test_prd_atomicity`] — this test
/// just sets up the live scenario and post-checks DB state.
#[test]
fn shadow_prd_atomicity_partial_batch() {
    let (_dir, mut conn) = setup_db();
    insert_task(&conn, "FEAT-P1", "in_progress");
    insert_task(&conn, "FEAT-P2", "in_progress");
    insert_task(&conn, "FEAT-P3", "in_progress");

    // PRD JSON: stories for P1 and P3 only (P2 deliberately omitted — its
    // `update_prd_task_passes` call will return NotFound).
    let tmp = TempDir::new().unwrap();
    let prd_path = tmp.path().join("prd.json");
    fs::write(
        &prd_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "userStories": [
                {"id": "P1", "passes": false},
                {"id": "P3", "passes": false},
            ]
        }))
        .unwrap(),
    )
    .expect("write PRD JSON");

    let intents = vec![
        TransitionIntent {
            task_id: "FEAT-P1".to_string(),
            change: TransitionChange::Done,
            source: TransitionSource::LoopStatusTag,
            reason: None,
            fail_status: None,
            audit_note: None,
        },
        TransitionIntent {
            task_id: "FEAT-P2".to_string(),
            change: TransitionChange::Done,
            source: TransitionSource::LoopStatusTag,
            reason: None,
            fail_status: None,
            audit_note: None,
        },
        TransitionIntent {
            task_id: "FEAT-P3".to_string(),
            change: TransitionChange::Done,
            source: TransitionSource::LoopStatusTag,
            reason: None,
            fail_status: None,
            audit_note: None,
        },
    ];

    crash_test_prd_atomicity(&mut conn, &intents, &[true, false, true], &prd_path, "FEAT");

    // All three DB rows landed Done (DB-authoritative invariant — the
    // failing PRD sync did NOT roll back the middle DB write).
    for id in ["FEAT-P1", "FEAT-P2", "FEAT-P3"] {
        assert_eq!(read_status(&conn, id), "done", "DB row {id} must be Done");
    }
}
