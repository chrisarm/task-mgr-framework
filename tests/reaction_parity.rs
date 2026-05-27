//! Parity harness for the converged `reactions::` coordinators.
//!
//! TEST-INIT-001 establishes this file (the harness skeleton reused by
//! TEST-INIT-002/003/004) and pins the contract for the post-output rate-limit
//! reaction `reactions::account::react_to_outputs` / `react_to_outputs_inner`
//! (audit #6 — the headline strand-bug this PRD fixes: wave mode never waited
//! on a rate limit and false-aborted in-flight work).
//!
//! ## Harness conventions (mirrors `tests/iteration_pipeline_parity.rs`)
//!
//! - Integration test → cannot reach `pub(crate)` internals (learning #896);
//!   `reactions` + `reactions::account` are `pub` for exactly this reason
//!   (mirrors `pub mod iteration_pipeline`). Setup goes through the public DB
//!   API (`open_connection` + `create_schema` + `run_migrations`).
//! - Each behavioral test uses its **own** independent DB (TempDir-backed) so
//!   no side effect leaks across cases; the sequential ↔ wave "both shapes
//!   agree" tests stand up **two** independent DBs.
//! - `TASK_MGR_NO_EXTRACT_LEARNINGS=1` keeps every case hermetic — the
//!   rate-limit reaction must never spawn a real Claude subprocess.
//! - The usage wait is **injected** as a counting closure
//!   ([`WaitFn`] = `&dyn Fn(u64) -> bool`), so the tests run with no OAuth, no
//!   usage API, and no real `thread::sleep`. The inner/outer split mirrors
//!   `auto_review::{maybe_fire, maybe_fire_inner}`.
//! - Fixtures are production-shaped: real [`IterationOutcome`] values and real
//!   `tasks` rows, never hand-built maps.
//!
//! ## TDD status
//!
//! The behavioral cases are `#[ignore]`'d — they call
//! `react_to_outputs_inner`, whose body is an `unimplemented!()` scaffold under
//! CONTRACT-001/TEST-INIT-001. **FEAT-006 implements the body and removes the
//! `#[ignore]` attributes** (its AC1 is "TEST-INIT-001 cases pass"). Two cases
//! run live today: a known-bad discriminator (proves the wait-once assertion
//! actually rejects a naive per-item-loop stub) and a harness-validity
//! compile-marker (proves the production-shaped fixtures + setup helpers work).

use std::cell::Cell;
use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::loop_engine::config::IterationOutcome;
use task_mgr::loop_engine::reactions::account::{
    AccountReaction, AccountReactionParams, OutputReactionItem, WaitFn, react_to_outputs_inner,
};

// ---------------------------------------------------------------------------
// Shared fixtures / setup helpers (reused by TEST-INIT-002/003/004)
// ---------------------------------------------------------------------------

/// Prefix every fixture task id shares, so the in_progress reset
/// (`recover_in_progress_for_prefix`) scopes correctly. `prefix_and` appends
/// `-%`, so ids like `RP-RATE-1` match the bare prefix `RP`.
const PREFIX: &str = "RP";
const RUN_ID: &str = "reaction-parity-run";

/// Open a DB with full schema + all migrations applied. The `TempDir` MUST
/// outlive the `Connection` — dropping it deletes the on-disk file.
fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

/// Insert an `in_progress` task row (the shape a rate-limited slot leaves
/// behind: claimed, never completed). The reaction must reset these to `todo`.
fn insert_in_progress_task(conn: &Connection, task_id: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        [task_id, "Reaction parity task"],
    )
    .expect("insert in_progress task row");
}

/// Insert a `done` task row (the shape a slot that completed earlier in the
/// SAME wave leaves behind — `process_slot_result` already flipped it). The
/// reaction must NOT touch it (B1: reset filters on `status='in_progress'`).
fn insert_done_task(conn: &Connection, task_id: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'done', 50)",
        [task_id, "Reaction parity task"],
    )
    .expect("insert done task row");
}

/// Insert a `runs` row so any FK-referencing bookkeeping a future body adds is
/// satisfied. Cheap and defensive.
fn insert_run(conn: &Connection) {
    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES (?1, 'active')",
        [RUN_ID],
    )
    .expect("insert run row");
}

/// Disable LLM-based learning extraction for the duration of the test. The
/// opt-out is a documented public contract (see module docs).
fn disable_llm_extraction() {
    // SAFETY: cargo test is the canonical caller; we accept the inherent
    // single-process race on env vars (same rationale as iteration_pipeline_parity).
    unsafe {
        std::env::set_var("TASK_MGR_NO_EXTRACT_LEARNINGS", "1");
    }
}

/// Read a single task's status, or `None` if the row is absent.
fn task_status(conn: &Connection, task_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT status FROM tasks WHERE id = ?1",
        [task_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// All `(id, status)` rows, id-ascending — a stable DB snapshot for "zero DB
/// writes" assertions.
fn all_task_statuses(conn: &Connection) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare("SELECT id, status FROM tasks ORDER BY id")
        .expect("prepare");
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

fn learnings_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
        .unwrap_or(0)
}

/// Spy implementing the injected wait seam. `Cell`-based (the seam is `Fn`,
/// not `FnMut`) so a `&self` closure can record call count + the last
/// wait-seconds argument. `return_value` is what the wait reports: `true`
/// (completed → retry) or `false` (`.stop` signal → stop).
struct WaitSpy {
    calls: Cell<u32>,
    last_secs: Cell<Option<u64>>,
    return_value: bool,
}

impl WaitSpy {
    fn completing() -> Self {
        Self {
            calls: Cell::new(0),
            last_secs: Cell::new(None),
            return_value: true,
        }
    }

    fn stopping() -> Self {
        Self {
            calls: Cell::new(0),
            last_secs: Cell::new(None),
            return_value: false,
        }
    }

    /// A `Fn(u64) -> bool` closure that records each invocation.
    fn closure(&self) -> impl Fn(u64) -> bool + '_ {
        move |secs| {
            self.calls.set(self.calls.get() + 1);
            self.last_secs.set(Some(secs));
            self.return_value
        }
    }
}

/// Standard params for a hermetic inner call. `tasks_dir` is a throwaway
/// TempDir path (the injected wait never actually polls it).
fn params<'a>(tasks_dir: &'a Path, fallback_wait: u64) -> AccountReactionParams<'a> {
    AccountReactionParams {
        threshold: 80,
        usage_enabled: false,
        tasks_dir,
        fallback_wait,
        prefix: PREFIX,
        run_id: RUN_ID,
    }
}

// ---------------------------------------------------------------------------
// AC: Wait-once — a 3-slot item-slice with 2 RateLimit items fires the
// injected wait closure EXACTLY once.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unblocked by FEAT-006: react_to_outputs_inner body"]
fn wait_once_fires_wait_exactly_once_for_multi_rate_limit_wave() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    // 3-slot wave: slots 0 and 2 rate-limited (in_progress), slot 1 completed.
    insert_in_progress_task(&conn, "RP-RATE-0");
    insert_done_task(&conn, "RP-DONE-1");
    insert_in_progress_task(&conn, "RP-RATE-2");

    let (rate, done) = (IterationOutcome::RateLimit, IterationOutcome::Completed);
    let items = [
        OutputReactionItem {
            task_id: Some("RP-RATE-0"),
            outcome: &rate,
            output: "Claude hit the limit; resets 4pm (America/Los_Angeles)",
        },
        OutputReactionItem {
            task_id: Some("RP-DONE-1"),
            outcome: &done,
            output: "<completed>RP-DONE-1</completed>",
        },
        OutputReactionItem {
            task_id: Some("RP-RATE-2"),
            outcome: &rate,
            output: "Claude hit the limit; resets 4pm (America/Los_Angeles)",
        },
    ];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    let reaction =
        react_to_outputs_inner(&mut conn, &items, &params(db_temp.path(), 600), &wait as WaitFn);

    assert_eq!(
        spy.calls.get(),
        1,
        "the account-global wait must fire EXACTLY once per wave, not once per rate-limited slot",
    );
    assert_eq!(
        reaction,
        AccountReaction::WaitedAndRetry,
        "a completed wait over a rate-limited wave must return WaitedAndRetry",
    );
}

// ---------------------------------------------------------------------------
// AC: Mixed-wave durability — a slice with one RateLimit + one Completed
// leaves the completed task `done` and resets only the rate-limited task to
// `todo` (B1: reset filters on status='in_progress').
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unblocked by FEAT-006: react_to_outputs_inner body"]
fn mixed_wave_resets_only_rate_limited_task_and_preserves_completed() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-RATE-1"); // rate-limited slot, still in_progress
    insert_done_task(&conn, "RP-DONE-1"); // completed earlier this wave

    let (rate, done) = (IterationOutcome::RateLimit, IterationOutcome::Completed);
    let items = [
        OutputReactionItem {
            task_id: Some("RP-RATE-1"),
            outcome: &rate,
            output: "rate limited; resets 4pm",
        },
        OutputReactionItem {
            task_id: Some("RP-DONE-1"),
            outcome: &done,
            output: "<completed>RP-DONE-1</completed>",
        },
    ];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    let reaction =
        react_to_outputs_inner(&mut conn, &items, &params(db_temp.path(), 600), &wait as WaitFn);

    assert_eq!(reaction, AccountReaction::WaitedAndRetry);
    assert_eq!(
        task_status(&conn, "RP-DONE-1").as_deref(),
        Some("done"),
        "the completed task must remain `done` — the reset must not clobber it",
    );
    assert_eq!(
        task_status(&conn, "RP-RATE-1").as_deref(),
        Some("todo"),
        "the rate-limited in_progress task must be reset to `todo` for retry",
    );
}

// ---------------------------------------------------------------------------
// AC: RateLimit-output inert — a RateLimit item's output produces zero
// learnings / completions / status-updates. The rate-limit reaction does NOT
// process completion/learning/status tags carried in the captured output.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unblocked by FEAT-006: react_to_outputs_inner body"]
fn rate_limit_output_does_not_trigger_completions_or_learnings() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-RATE-1");

    let baseline_learnings = learnings_count(&conn);

    // The rate-limited output is laden with tags the *pipeline* would act on.
    // The rate-limit reaction must ignore them entirely.
    let rate = IterationOutcome::RateLimit;
    let items = [OutputReactionItem {
        task_id: Some("RP-RATE-1"),
        outcome: &rate,
        output: "<task-status>RP-RATE-1:done</task-status>\n\
                 <completed>RP-RATE-1</completed>\n\
                 <learning><title>Should not be recorded</title>\
                 <content>inert</content></learning>\n\
                 resets 4pm",
    }];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    let reaction =
        react_to_outputs_inner(&mut conn, &items, &params(db_temp.path(), 600), &wait as WaitFn);

    assert_eq!(reaction, AccountReaction::WaitedAndRetry);
    assert_eq!(
        learnings_count(&conn),
        baseline_learnings,
        "a rate-limit item must NOT extract learnings from its output",
    );
    // The task is reset to `todo` for retry — NEVER promoted to `done` by the
    // `<completed>` / `<task-status>` tags embedded in the rate-limit output.
    assert_eq!(
        task_status(&conn, "RP-RATE-1").as_deref(),
        Some("todo"),
        "a rate-limit item must NOT complete its task via output tags; it resets to todo",
    );
}

// ---------------------------------------------------------------------------
// AC: Parse-fail fallback — unparseable reset output → wait closure invoked
// with the `fallback_wait` value, and the sequential (1-item) and wave
// (N-item) shapes agree on that value.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unblocked by FEAT-006: react_to_outputs_inner body"]
fn parse_fail_falls_back_to_fallback_wait_and_both_shapes_agree() {
    disable_llm_extraction();
    const FALLBACK: u64 = 1234;

    // Sequential shape: a single rate-limited item with unparseable output.
    let (seq_temp, mut seq_conn) = setup_migrated_db();
    insert_run(&seq_conn);
    insert_in_progress_task(&seq_conn, "RP-RATE-1");
    let rate = IterationOutcome::RateLimit;
    let seq_items = [OutputReactionItem {
        task_id: Some("RP-RATE-1"),
        outcome: &rate,
        output: "rate limited but NO parseable reset token here",
    }];
    let seq_spy = WaitSpy::completing();
    let seq_wait = seq_spy.closure();
    react_to_outputs_inner(
        &mut seq_conn,
        &seq_items,
        &params(seq_temp.path(), FALLBACK),
        &seq_wait as WaitFn,
    );

    // Wave shape: same unparseable rate-limited item, plus a completed sibling.
    let (wave_temp, mut wave_conn) = setup_migrated_db();
    insert_run(&wave_conn);
    insert_in_progress_task(&wave_conn, "RP-RATE-1");
    insert_done_task(&wave_conn, "RP-DONE-2");
    let done = IterationOutcome::Completed;
    let wave_items = [
        OutputReactionItem {
            task_id: Some("RP-RATE-1"),
            outcome: &rate,
            output: "rate limited but NO parseable reset token here",
        },
        OutputReactionItem {
            task_id: Some("RP-DONE-2"),
            outcome: &done,
            output: "<completed>RP-DONE-2</completed>",
        },
    ];
    let wave_spy = WaitSpy::completing();
    let wave_wait = wave_spy.closure();
    react_to_outputs_inner(
        &mut wave_conn,
        &wave_items,
        &params(wave_temp.path(), FALLBACK),
        &wave_wait as WaitFn,
    );

    assert_eq!(
        seq_spy.last_secs.get(),
        Some(FALLBACK),
        "unparseable reset output must fall back to fallback_wait (sequential shape)",
    );
    assert_eq!(
        seq_spy.last_secs.get(),
        wave_spy.last_secs.get(),
        "sequential (1-item) and wave (N-item) shapes must compute the SAME wait value",
    );
    assert_eq!(seq_spy.calls.get(), 1, "sequential must fire the wait once");
    assert_eq!(wave_spy.calls.get(), 1, "wave must fire the wait once");
}

// ---------------------------------------------------------------------------
// AC: Negative control — a slice with no RateLimit returns
// AccountReaction::None and performs ZERO DB writes (and never waits).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unblocked by FEAT-006: react_to_outputs_inner body"]
fn no_rate_limit_returns_none_and_writes_nothing() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-WORK-1");
    insert_done_task(&conn, "RP-DONE-1");

    let before = all_task_statuses(&conn);

    let (completed, empty) = (IterationOutcome::Completed, IterationOutcome::Empty);
    let items = [
        OutputReactionItem {
            task_id: Some("RP-DONE-1"),
            outcome: &completed,
            output: "<completed>RP-DONE-1</completed>",
        },
        OutputReactionItem {
            task_id: Some("RP-WORK-1"),
            outcome: &empty,
            output: "still working, no rate limit",
        },
    ];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    let reaction =
        react_to_outputs_inner(&mut conn, &items, &params(db_temp.path(), 600), &wait as WaitFn);

    assert_eq!(
        reaction,
        AccountReaction::None,
        "no RateLimit item ⇒ AccountReaction::None",
    );
    assert_eq!(spy.calls.get(), 0, "no rate limit ⇒ the wait must NEVER fire");
    assert_eq!(
        all_task_statuses(&conn),
        before,
        "no rate limit ⇒ ZERO DB writes (in_progress task must NOT be reset)",
    );
}

// ---------------------------------------------------------------------------
// AC: Stop — injected stop/signal during the wait → AccountReaction::Stop.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unblocked by FEAT-006: react_to_outputs_inner body"]
fn stop_signal_during_wait_returns_stop() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-RATE-1");

    let rate = IterationOutcome::RateLimit;
    let items = [OutputReactionItem {
        task_id: Some("RP-RATE-1"),
        outcome: &rate,
        output: "rate limited; resets 4pm",
    }];

    let spy = WaitSpy::stopping(); // wait reports interrupted-by-stop
    let wait = spy.closure();
    let reaction =
        react_to_outputs_inner(&mut conn, &items, &params(db_temp.path(), 600), &wait as WaitFn);

    assert_eq!(spy.calls.get(), 1, "the wait must have been attempted once");
    assert_eq!(
        reaction,
        AccountReaction::Stop,
        "a wait interrupted by the .stop signal must return AccountReaction::Stop",
    );
}

// ---------------------------------------------------------------------------
// AC: Known-bad discriminator (RUNS LIVE). Proves the wait-once assertion
// actually discriminates: a naive "loop over items, call wait per RateLimit
// item" stub fires the wait once PER rate-limited item, so the same spy that
// the wait-once test asserts `== 1` on would see `== 2` here. This guarantees
// `wait_once_fires_wait_exactly_once_for_multi_rate_limit_wave` is not a
// vacuous test — a regression to per-item waiting fails it.
// ---------------------------------------------------------------------------

/// A deliberately-wrong implementation: fires `wait` once per RateLimit item.
fn naive_per_item_wait(items: &[OutputReactionItem<'_>], wait: WaitFn<'_>) {
    for item in items {
        if *item.outcome == IterationOutcome::RateLimit {
            wait(0);
        }
    }
}

#[test]
fn known_bad_per_item_stub_fails_the_wait_once_assertion() {
    let (rate, done) = (IterationOutcome::RateLimit, IterationOutcome::Completed);
    let items = [
        OutputReactionItem {
            task_id: Some("RP-RATE-0"),
            outcome: &rate,
            output: "resets 4pm",
        },
        OutputReactionItem {
            task_id: Some("RP-DONE-1"),
            outcome: &done,
            output: "<completed>RP-DONE-1</completed>",
        },
        OutputReactionItem {
            task_id: Some("RP-RATE-2"),
            outcome: &rate,
            output: "resets 4pm",
        },
    ];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    naive_per_item_wait(&items, &wait as WaitFn);

    // The wait-once test asserts `spy.calls.get() == 1`. The naive stub yields
    // 2 over a 2-RateLimit wave, so it WOULD fail that assertion — the
    // discriminator is real.
    assert_eq!(
        spy.calls.get(),
        2,
        "naive per-item stub fires the wait once per rate-limited slot (2 here)",
    );
    assert_ne!(
        spy.calls.get(),
        1,
        "...which violates the once-per-wave contract the real reaction must satisfy",
    );
}

// ---------------------------------------------------------------------------
// Harness-validity compile-marker (RUNS LIVE). Proves the production-shaped
// fixtures + setup helpers wire up: a migrated DB stands up, real `tasks` rows
// insert, and `OutputReactionItem`s are constructible from real
// `IterationOutcome` values. Does NOT call the unimplemented coordinator.
// ---------------------------------------------------------------------------

#[test]
fn harness_fixtures_are_production_shaped_and_setup_works() {
    disable_llm_extraction();
    let (db_temp, conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-RATE-1");
    insert_done_task(&conn, "RP-DONE-1");

    // Production-shaped items built from real outcomes + the captured outputs.
    let (rate, done) = (IterationOutcome::RateLimit, IterationOutcome::Completed);
    let items = [
        OutputReactionItem {
            task_id: Some("RP-RATE-1"),
            outcome: &rate,
            output: "resets 4pm",
        },
        OutputReactionItem {
            task_id: Some("RP-DONE-1"),
            outcome: &done,
            output: "<completed>RP-DONE-1</completed>",
        },
    ];

    // The params struct (consumed exhaustively by the FEAT-006 body) is
    // constructible from the harness.
    let _p = params(db_temp.path(), 600);

    assert_eq!(items.len(), 2);
    assert!(matches!(items[0].outcome, IterationOutcome::RateLimit));
    assert_eq!(task_status(&conn, "RP-RATE-1").as_deref(), Some("in_progress"));
    assert_eq!(task_status(&conn, "RP-DONE-1").as_deref(), Some("done"));
    assert_eq!(all_task_statuses(&conn).len(), 2);
}
