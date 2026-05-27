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
use task_mgr::loop_engine::config::{IterationOutcome, PermissionMode};
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
    conn.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |r| {
        r.get::<_, String>(0)
    })
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

/// `permission_mode` only feeds the production wait closure's early-lift probe;
/// the hermetic inner tests inject the wait, so any value works. A `static`
/// gives the `&'static PermissionMode` the params struct borrows.
static PERMISSION_MODE: PermissionMode = PermissionMode::Dangerous;

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
        permission_mode: &PERMISSION_MODE,
    }
}

// ---------------------------------------------------------------------------
// AC: Wait-once — a 3-slot item-slice with 2 RateLimit items fires the
// injected wait closure EXACTLY once.
// ---------------------------------------------------------------------------

#[test]
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
    let reaction = react_to_outputs_inner(
        &mut conn,
        &items,
        &params(db_temp.path(), 600),
        &wait as WaitFn,
    );

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
    let reaction = react_to_outputs_inner(
        &mut conn,
        &items,
        &params(db_temp.path(), 600),
        &wait as WaitFn,
    );

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
    let reaction = react_to_outputs_inner(
        &mut conn,
        &items,
        &params(db_temp.path(), 600),
        &wait as WaitFn,
    );

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
    let reaction = react_to_outputs_inner(
        &mut conn,
        &items,
        &params(db_temp.path(), 600),
        &wait as WaitFn,
    );

    assert_eq!(
        reaction,
        AccountReaction::None,
        "no RateLimit item ⇒ AccountReaction::None",
    );
    assert_eq!(
        spy.calls.get(),
        0,
        "no rate limit ⇒ the wait must NEVER fire"
    );
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
    let reaction = react_to_outputs_inner(
        &mut conn,
        &items,
        &params(db_temp.path(), 600),
        &wait as WaitFn,
    );

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
    assert_eq!(
        task_status(&conn, "RP-RATE-1").as_deref(),
        Some("in_progress")
    );
    assert_eq!(task_status(&conn, "RP-DONE-1").as_deref(), Some("done"));
    assert_eq!(all_task_statuses(&conn).len(), 2);
}

// ===========================================================================
// TEST-INIT-002 — pre-spawn `resolve_task_execution` + `account_usage_gate`
// parity (#2 effort, #3 crash). Pins the contract for FEAT-002 (the
// `resolve_task_execution` body) and FEAT-003 (the `account_usage_gate_inner`
// body), which fill the `unimplemented!()` scaffolds and remove the `#[ignore]`
// attributes below. The two LIVE cases (known-bad effort discriminator +
// harness compile-marker) guard that the ignored assertions actually
// discriminate and that the fixtures wire up.
// ===========================================================================

use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
use task_mgr::loop_engine::reactions::account::{
    AccountUsageGateParams, UsageGateFn, account_usage_gate_inner,
};
use task_mgr::loop_engine::reactions::pre_spawn::{
    ResolveTaskExecutionParams, TaskExecutionPlan, resolve_task_execution,
};
use task_mgr::loop_engine::runner::RunnerKind;
use task_mgr::loop_engine::usage::UsageCheckResult;

/// Insert a task row carrying an explicit `model` column — the shape
/// `check_override_invalidation` re-reads when comparing against its snapshot.
fn insert_task_with_model(conn: &Connection, task_id: &str, model: Option<&str>) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, max_retries, consecutive_failures) \
         VALUES (?1, ?2, 'in_progress', ?3, 5, 0)",
        rusqlite::params![task_id, "Pre-spawn parity task", model],
    )
    .expect("insert task row with model");
}

/// Pre-populate every per-task auto-recovery channel for `task_id`, mirroring
/// what the overflow ladder + RuntimeError fallback would write. `snapshot`
/// is the `tasks.model` value captured when the first override was set — a
/// later DB edit that diverges from it fires the operator escape valve.
fn seed_all_overrides(ctx: &mut IterationContext, task_id: &str, snapshot: &str) {
    ctx.effort_overrides.insert(task_id.to_string(), "high");
    ctx.model_overrides
        .insert(task_id.to_string(), OPUS_MODEL.to_string());
    ctx.overflow_recovered.insert(task_id.to_string());
    ctx.overflow_original_model
        .insert(task_id.to_string(), snapshot.to_string());
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Grok);
    ctx.overflow_original_task_model
        .insert(task_id.to_string(), Some(snapshot.to_string()));
}

/// True iff NO per-task override channel still carries an entry for `task_id`.
fn all_overrides_cleared(ctx: &IterationContext, task_id: &str) -> bool {
    !ctx.effort_overrides.contains_key(task_id)
        && !ctx.model_overrides.contains_key(task_id)
        && !ctx.overflow_recovered.contains(task_id)
        && !ctx.overflow_original_model.contains_key(task_id)
        && !ctx.runner_overrides.contains_key(task_id)
        && !ctx.overflow_original_task_model.contains_key(task_id)
}

/// Counting spy for the injected usage-gate seam. Records the call count and
/// the `(threshold, fallback)` it was handed, so the test can prove those
/// params flow through to the gate (not dropped). Returns a fixed decision —
/// the coordinator must map it through unchanged.
struct UsageGateSpy {
    calls: Cell<u32>,
    last_threshold: Cell<Option<u8>>,
    last_fallback: Cell<Option<u64>>,
}

impl UsageGateSpy {
    fn new() -> Self {
        Self {
            calls: Cell::new(0),
            last_threshold: Cell::new(None),
            last_fallback: Cell::new(None),
        }
    }

    fn closure(&self) -> impl Fn(u8, &Path, u64) -> UsageCheckResult + '_ {
        move |threshold, _dir, fallback| {
            self.calls.set(self.calls.get() + 1);
            self.last_threshold.set(Some(threshold));
            self.last_fallback.set(Some(fallback));
            UsageCheckResult::BelowThreshold
        }
    }
}

// ---------------------------------------------------------------------------
// AC #1: the sequential (1 call) and wave (per-slot) shapes fold through the
// SAME coordinator, so identical (ctx, task, resolved_model, conn) inputs yield
// an identical TaskExecutionPlan. Two independent (ctx, DB) pairs seeded
// identically must compute equal plans.
// ---------------------------------------------------------------------------

#[test]
fn resolve_task_execution_parity_seq_and_wave_agree() {
    let task_id = "PS-PARITY-1";

    let (seq_dir, seq_conn) = setup_migrated_db();
    insert_task_with_model(&seq_conn, task_id, Some(SONNET_MODEL));
    let mut seq_ctx = IterationContext::new(5);
    seq_ctx
        .crashed_last_iteration
        .insert(task_id.to_string(), true);
    seq_ctx.effort_overrides.insert(task_id.to_string(), "high");
    let seq_plan = resolve_task_execution(ResolveTaskExecutionParams {
        ctx: &mut seq_ctx,
        conn: &seq_conn,
        task_id,
        resolved_model: Some(SONNET_MODEL),
    });

    let (wave_dir, wave_conn) = setup_migrated_db();
    insert_task_with_model(&wave_conn, task_id, Some(SONNET_MODEL));
    let mut wave_ctx = IterationContext::new(5);
    wave_ctx
        .crashed_last_iteration
        .insert(task_id.to_string(), true);
    wave_ctx
        .effort_overrides
        .insert(task_id.to_string(), "high");
    let wave_plan = resolve_task_execution(ResolveTaskExecutionParams {
        ctx: &mut wave_ctx,
        conn: &wave_conn,
        task_id,
        resolved_model: Some(SONNET_MODEL),
    });

    assert_eq!(
        seq_plan, wave_plan,
        "sequential (1 call) and wave (per-slot) shapes must compute the SAME plan",
    );
    assert_eq!(
        seq_plan,
        TaskExecutionPlan {
            model: Some(OPUS_MODEL.to_string()),
            effort: Some("high"),
            runner: RunnerKind::Claude,
        },
        "a crashed sonnet task with a high effort override escalates to opus on Claude",
    );
    drop((seq_dir, wave_dir));
}

// ---------------------------------------------------------------------------
// AC #2: a prior overflow effort_override on ctx is reflected in the plan
// (guards the audit-#6-effort channel). No crash ⇒ model stays None, but the
// effort must still surface.
// ---------------------------------------------------------------------------

#[test]
fn resolve_task_execution_surfaces_prior_effort_override() {
    let task_id = "PS-EFFORT-1";
    let (_dir, conn) = setup_migrated_db();
    insert_task_with_model(&conn, task_id, Some(SONNET_MODEL));

    let mut ctx = IterationContext::new(5);
    ctx.effort_overrides.insert(task_id.to_string(), "high");
    // Not crashed: model escalation must NOT fire.

    let plan = resolve_task_execution(ResolveTaskExecutionParams {
        ctx: &mut ctx,
        conn: &conn,
        task_id,
        resolved_model: Some(SONNET_MODEL),
    });

    assert_eq!(
        plan.effort,
        Some("high"),
        "a prior overflow effort_override MUST flow through into the plan",
    );
    assert_eq!(
        plan.model, None,
        "no crash ⇒ no model escalation (the plan keeps the resolved baseline)",
    );
}

// ---------------------------------------------------------------------------
// AC #3: a task flagged crashed escalates the model on re-pick; a non-crashed
// task does not. Two independent (ctx, DB) pairs isolate the cases.
// ---------------------------------------------------------------------------

#[test]
fn resolve_task_execution_escalates_on_crash_not_otherwise() {
    let task_id = "PS-CRASH-1";

    // Crashed: sonnet escalates to opus.
    let (_crash_dir, crash_conn) = setup_migrated_db();
    insert_task_with_model(&crash_conn, task_id, Some(SONNET_MODEL));
    let mut crash_ctx = IterationContext::new(5);
    crash_ctx
        .crashed_last_iteration
        .insert(task_id.to_string(), true);
    let crashed_plan = resolve_task_execution(ResolveTaskExecutionParams {
        ctx: &mut crash_ctx,
        conn: &crash_conn,
        task_id,
        resolved_model: Some(SONNET_MODEL),
    });
    assert_eq!(
        crashed_plan.model,
        Some(OPUS_MODEL.to_string()),
        "a crashed sonnet task must escalate to opus on re-pick",
    );

    // Not crashed: no escalation.
    let (_ok_dir, ok_conn) = setup_migrated_db();
    insert_task_with_model(&ok_conn, task_id, Some(SONNET_MODEL));
    let mut ok_ctx = IterationContext::new(5);
    ok_ctx
        .crashed_last_iteration
        .insert(task_id.to_string(), false);
    let ok_plan = resolve_task_execution(ResolveTaskExecutionParams {
        ctx: &mut ok_ctx,
        conn: &ok_conn,
        task_id,
        resolved_model: Some(SONNET_MODEL),
    });
    assert_eq!(
        ok_plan.model, None,
        "a non-crashed task must NOT escalate its model",
    );
}

// ---------------------------------------------------------------------------
// AC #4: an out-of-band tasks.model edit clears the six per-task recovery
// channels (operator escape valve), and the resulting plan resolves fresh —
// the cleared effort override is gone and the stale Grok runner override no
// longer shadows the operator's Claude model.
// ---------------------------------------------------------------------------

#[test]
fn resolve_task_execution_invalidates_stale_overrides_on_operator_edit() {
    let task_id = "PS-INVAL-1";
    let (_dir, conn) = setup_migrated_db();
    // Snapshot captured at opus; operator later edits the column to haiku.
    insert_task_with_model(&conn, task_id, Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides(&mut ctx, task_id, OPUS_MODEL);

    // Operator edits tasks.model out-of-band — diverges from the snapshot.
    conn.execute(
        "UPDATE tasks SET model = ?1 WHERE id = ?2",
        rusqlite::params![HAIKU_MODEL, task_id],
    )
    .expect("operator model edit");

    let plan = resolve_task_execution(ResolveTaskExecutionParams {
        ctx: &mut ctx,
        conn: &conn,
        task_id,
        resolved_model: Some(HAIKU_MODEL),
    });

    assert!(
        all_overrides_cleared(&ctx, task_id),
        "an out-of-band tasks.model edit must clear all six per-task recovery channels",
    );
    assert_eq!(
        plan.effort, None,
        "the cleared effort override must NOT resurface in the fresh plan",
    );
    assert_eq!(
        plan.runner,
        RunnerKind::Claude,
        "the cleared Grok runner override must not shadow the operator's haiku (Claude) model",
    );
}

// ---------------------------------------------------------------------------
// AC #5: account_usage_gate returns the same GateDecision for the same usage
// state on both shapes. Inject the same fixed gate decision via the seam on two
// independent shapes; assert equal results and that each fired the gate exactly
// once with the params handed in (proves the params flow through, not dropped).
// ---------------------------------------------------------------------------

#[test]
fn account_usage_gate_inner_same_decision_both_shapes() {
    let seq_dir = TempDir::new().expect("tempdir");
    let wave_dir = TempDir::new().expect("tempdir");

    let seq_spy = UsageGateSpy::new();
    let seq_gate = seq_spy.closure();
    let seq_decision = account_usage_gate_inner(
        AccountUsageGateParams {
            threshold: 80,
            tasks_dir: seq_dir.path(),
            fallback_wait: 600,
        },
        &seq_gate as UsageGateFn,
    );

    let wave_spy = UsageGateSpy::new();
    let wave_gate = wave_spy.closure();
    let wave_decision = account_usage_gate_inner(
        AccountUsageGateParams {
            threshold: 80,
            tasks_dir: wave_dir.path(),
            fallback_wait: 600,
        },
        &wave_gate as UsageGateFn,
    );

    assert_eq!(
        seq_decision, wave_decision,
        "same usage state ⇒ same GateDecision on both shapes",
    );
    assert_eq!(seq_decision, UsageCheckResult::BelowThreshold);
    assert_eq!(
        seq_spy.calls.get(),
        1,
        "the usage gate must fire exactly once (sequential)"
    );
    assert_eq!(
        wave_spy.calls.get(),
        1,
        "the usage gate must fire exactly once (wave)"
    );
    assert_eq!(
        seq_spy.last_threshold.get(),
        Some(80),
        "the threshold param must flow through to the gate, not be dropped",
    );
    assert_eq!(seq_spy.last_fallback.get(), Some(600));
}

// ---------------------------------------------------------------------------
// AC #6 (RUNS LIVE): known-bad discriminator. A naive plan-builder that DROPS
// ctx.effort_overrides yields effort=None even when ctx carries a "high"
// override — so it would FAIL the AC#2 assertion (which expects Some("high")).
// This proves resolve_task_execution_surfaces_prior_effort_override is not a
// vacuous test: a regression that forgets the effort channel is rejected.
// ---------------------------------------------------------------------------

/// A deliberately-wrong builder: ignores `ctx.effort_overrides` entirely.
fn naive_plan_dropping_effort(ctx: &IterationContext, task_id: &str) -> TaskExecutionPlan {
    let _ = (ctx, task_id); // BUG: never reads ctx.effort_overrides
    TaskExecutionPlan {
        model: None,
        effort: None,
        runner: RunnerKind::Claude,
    }
}

#[test]
fn known_bad_plan_dropping_effort_fails_the_effort_assertion() {
    let task_id = "PS-EFFORT-DISC";
    let mut ctx = IterationContext::new(5);
    ctx.effort_overrides.insert(task_id.to_string(), "high");

    let plan = naive_plan_dropping_effort(&ctx, task_id);

    // The AC#2 test asserts `plan.effort == Some("high")`. The naive stub
    // yields None despite the seeded override, so it WOULD fail that
    // assertion — the discriminator is real.
    assert_eq!(
        plan.effort, None,
        "naive stub drops the effort override (yields None)",
    );
    assert_ne!(
        plan.effort,
        Some("high"),
        "...which violates the effort-flows-through contract the real coordinator must satisfy",
    );
}

// ---------------------------------------------------------------------------
// Harness-validity compile-marker (RUNS LIVE). Proves the TEST-INIT-002
// fixtures + the new pub contract types wire up: a migrated DB stands up, a
// task row with a model column inserts, an IterationContext seeds, and both
// param structs are constructible. Does NOT call the unimplemented coordinators.
// ---------------------------------------------------------------------------

#[test]
fn pre_spawn_and_gate_harness_compiles_and_setup_works() {
    let task_id = "PS-MARK-1";
    let (db_temp, conn) = setup_migrated_db();
    insert_task_with_model(&conn, task_id, Some(SONNET_MODEL));

    let mut ctx = IterationContext::new(5);
    ctx.crashed_last_iteration.insert(task_id.to_string(), true);
    ctx.effort_overrides.insert(task_id.to_string(), "high");

    assert_eq!(ctx.crashed_last_iteration.get(task_id).copied(), Some(true),);
    assert_eq!(ctx.effort_overrides.get(task_id).copied(), Some("high"));

    // The usage-gate seam + params struct are constructible from the harness.
    let spy = UsageGateSpy::new();
    let _gate = spy.closure();
    let _gate_params = AccountUsageGateParams {
        threshold: 80,
        tasks_dir: db_temp.path(),
        fallback_wait: 600,
    };

    // The pre-spawn params struct is constructible (mutably borrows ctx last).
    let _resolve_params = ResolveTaskExecutionParams {
        ctx: &mut ctx,
        conn: &conn,
        task_id,
        resolved_model: Some(SONNET_MODEL),
    };
}

// ===========================================================================
// TEST-INIT-003 — overflow reaction (`handle_overflow`) parity + per-slot
// isolation (#5). Pins the contract that the converged coordinator
// `reactions::post_output::handle_overflow` preserves the five-rung recovery
// ladder, the diagnostics bundle (dump/JSONL/rotation), `FallbackToProvider`
// promotion, and the per-slot keying isolation that `overflow.rs` already
// guarantees (learning #2852: wave overflow recovery evolved bypass →
// shared-ladder — do not regress).
//
// Unlike TEST-INIT-001/002, `handle_overflow` is ALREADY wired end-to-end
// (CONTRACT-001: both iteration.rs and slot.rs route through it), so these
// cases run LIVE — they are the relocation safety net. The existing
// `tests/overflow_per_slot.rs` / `overflow_recovery.rs` suites drive the
// `#[deprecated]` leaf directly and are the equivalence oracle; these cases
// re-assert the same observable behavior through the coordinator so the
// eventual body relocation (the owning FEAT) must keep BOTH suites green.
//
// Per-AC mapping (learning #4100):
//   AC1  → handle_overflow_rung_ladder_matches_pre_relocation
//          + handle_overflow_rung4_fallback_to_provider_when_enabled
//          + handle_overflow_seq_and_wave_agree_on_rung
//   AC2  → handle_overflow_per_slot_keying_excludes_sibling_slot_task_ids
//   AC3  → handle_overflow_sanitizes_path_traversal_task_id
//   AC4  → handle_overflow_emits_jsonl_event_with_core_fields
//   AC5  → known_bad_overflow_skipping_rung1_fails_the_downgrade_assertion (LIVE)
// ===========================================================================

use task_mgr::loop_engine::model::{OPUS_MODEL_1M, escalate_below_opus, to_1m_model};
use task_mgr::loop_engine::overflow::{OverflowEvent, RecoveryAction, sanitize_id_for_filename};
use task_mgr::loop_engine::project_config::{FallbackRunnerConfig, ProjectConfig};
use task_mgr::loop_engine::prompt::PromptResult;
use task_mgr::loop_engine::reactions::post_output::{HandleOverflowParams, handle_overflow};

/// Production-shaped `PromptResult` for an overflow event: a non-empty prompt
/// with real per-section byte counts (so the dump + JSONL `sections` array is
/// non-trivial), mirroring `tests/overflow_per_slot.rs::make_prompt_result`.
fn make_overflow_prompt_result(task_id: &str) -> PromptResult {
    PromptResult {
        prompt: "TASK SECTION\n\nLEARNINGS SECTION\n\nBASE PROMPT SECTION\n".to_string(),
        task_id: task_id.to_string(),
        task_files: Vec::new(),
        shown_learning_ids: Vec::new(),
        resolved_model: None,
        dropped_sections: Vec::new(),
        task_difficulty: Some("medium".to_string()),
        cluster_effort: None,
        section_sizes: vec![("task", 12), ("learnings", 17), ("base_prompt", 19)],
    }
}

/// Read + parse every JSONL line `handle_overflow` appended to
/// `<base_dir>/overflow-events.jsonl`.
fn read_overflow_events(base_dir: &Path) -> Vec<OverflowEvent> {
    let path = base_dir.join("overflow-events.jsonl");
    let raw = std::fs::read_to_string(&path).expect("overflow jsonl exists");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<OverflowEvent>(l).expect("parse jsonl line"))
        .collect()
}

/// A `ProjectConfig` with the Grok fallback runner ENABLED — the precondition
/// for rung 4 (`FallbackToProvider`) on the Claude → Grok direction.
fn config_with_fallback_enabled() -> ProjectConfig {
    ProjectConfig {
        fallback_runner: Some(FallbackRunnerConfig {
            enabled: true,
            ..FallbackRunnerConfig::default()
        }),
        ..ProjectConfig::default()
    }
}

/// Run ONE overflow through the converged coordinator. Borrows `conn`/`ctx`
/// mutably for the duration of the call (the temporary `PromptResult` lives for
/// the full call expression). Returns the chosen [`RecoveryAction`].
#[allow(clippy::too_many_arguments)]
fn run_handle_overflow(
    conn: &mut Connection,
    ctx: &mut IterationContext,
    task_id: &str,
    effort: Option<&str>,
    effective_model: Option<&str>,
    iteration: u32,
    base_dir: &Path,
    slot_index: Option<usize>,
    effective_runner: RunnerKind,
    project_config: &ProjectConfig,
) -> RecoveryAction {
    handle_overflow(HandleOverflowParams {
        ctx,
        conn,
        task_id,
        effort,
        effective_model,
        prompt_result: &make_overflow_prompt_result(task_id),
        iteration,
        run_id: Some(RUN_ID),
        base_dir,
        slot_index,
        effective_runner,
        project_config,
    })
}

// ---------------------------------------------------------------------------
// AC1: rung selection through the coordinator matches the pre-relocation
// ladder. Each fresh (DB, ctx) drives one rung from a distinct (effort, model)
// input and asserts the action AND the resulting task-row status transition.
// ---------------------------------------------------------------------------

#[test]
fn handle_overflow_rung_ladder_matches_pre_relocation() {
    disable_llm_extraction();

    // Rung 1 — effort downgrade (xhigh → high). DB row resets to todo.
    {
        let (tmp, mut conn) = setup_migrated_db();
        insert_run(&conn);
        insert_task_with_model(&conn, "OF-R1", None);
        let mut ctx = IterationContext::new(10);
        let action = run_handle_overflow(
            &mut conn,
            &mut ctx,
            "OF-R1",
            Some("xhigh"),
            Some(SONNET_MODEL),
            1,
            tmp.path(),
            None,
            RunnerKind::Claude,
            &ProjectConfig::default(),
        );
        assert_eq!(
            action,
            RecoveryAction::DowngradeEffort {
                new_effort: "high".to_string()
            },
            "rung 1: xhigh effort must downgrade to high before any model escalation",
        );
        assert_eq!(task_status(&conn, "OF-R1").as_deref(), Some("todo"));
        assert_eq!(ctx.effort_overrides.get("OF-R1").copied(), Some("high"));
    }

    // Rung 2 — model escalation below opus once effort floor (high) is reached.
    for (model, expected) in [(SONNET_MODEL, OPUS_MODEL), (HAIKU_MODEL, SONNET_MODEL)] {
        let (tmp, mut conn) = setup_migrated_db();
        insert_run(&conn);
        insert_task_with_model(&conn, "OF-R2", None);
        let mut ctx = IterationContext::new(10);
        let action = run_handle_overflow(
            &mut conn,
            &mut ctx,
            "OF-R2",
            Some("high"),
            Some(model),
            1,
            tmp.path(),
            None,
            RunnerKind::Claude,
            &ProjectConfig::default(),
        );
        assert_eq!(
            action,
            RecoveryAction::EscalateModel {
                new_model: expected.to_string()
            },
            "rung 2: at the high effort floor, {model} must escalate to {expected}",
        );
        assert_eq!(task_status(&conn, "OF-R2").as_deref(), Some("todo"));
        assert_eq!(
            ctx.model_overrides.get("OF-R2").map(String::as_str),
            Some(expected),
        );
    }

    // Rung 3 — 1M-context escalation once at the Opus ceiling.
    {
        let (tmp, mut conn) = setup_migrated_db();
        insert_run(&conn);
        insert_task_with_model(&conn, "OF-R3", None);
        let mut ctx = IterationContext::new(10);
        let action = run_handle_overflow(
            &mut conn,
            &mut ctx,
            "OF-R3",
            Some("high"),
            Some(OPUS_MODEL),
            1,
            tmp.path(),
            None,
            RunnerKind::Claude,
            &ProjectConfig::default(),
        );
        assert_eq!(
            action,
            RecoveryAction::To1mModel {
                new_model: OPUS_MODEL_1M.to_string()
            },
            "rung 3: opus must escalate to the 1M-context variant",
        );
        assert_eq!(task_status(&conn, "OF-R3").as_deref(), Some("todo"));
    }

    // Rung 5 — no recovery available (Opus[1M] at high effort, no fallback). The
    // task is blocked, NOT reset to todo.
    {
        let (tmp, mut conn) = setup_migrated_db();
        insert_run(&conn);
        insert_task_with_model(&conn, "OF-R5", None);
        let mut ctx = IterationContext::new(10);
        let action = run_handle_overflow(
            &mut conn,
            &mut ctx,
            "OF-R5",
            Some("high"),
            Some(OPUS_MODEL_1M),
            1,
            tmp.path(),
            None,
            RunnerKind::Claude,
            &ProjectConfig::default(),
        );
        assert_eq!(
            action,
            RecoveryAction::Blocked,
            "rung 5: Opus[1M] at high effort with no fallback config has no recovery",
        );
        assert_eq!(
            task_status(&conn, "OF-R5").as_deref(),
            Some("blocked"),
            "rung 5 blocks the task (started_at preserved for audit); it is NOT reset to todo",
        );
    }
}

// ---------------------------------------------------------------------------
// AC1 (rung 4): FallbackToProvider fires through the coordinator when the
// Claude ladder is exhausted AND the Grok fallback runner is enabled. The task
// row is reset to todo and `tasks.model` is rewritten to the cross-provider
// target so the next iteration picks up the Grok model.
// ---------------------------------------------------------------------------

#[test]
fn handle_overflow_rung4_fallback_to_provider_when_enabled() {
    disable_llm_extraction();
    let (tmp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_task_with_model(&conn, "OF-R4", Some(OPUS_MODEL_1M));
    let mut ctx = IterationContext::new(10);

    let action = run_handle_overflow(
        &mut conn,
        &mut ctx,
        "OF-R4",
        Some("high"),
        Some(OPUS_MODEL_1M),
        1,
        tmp.path(),
        None,
        RunnerKind::Claude,
        &config_with_fallback_enabled(),
    );

    assert_eq!(
        action,
        RecoveryAction::FallbackToProvider {
            provider: "grok".to_string(),
            model: "grok-build".to_string(),
        },
        "rung 4: Claude ladder exhausted + fallback enabled ⇒ pivot to the Grok runner",
    );
    // Recovery state written atomically: runner + model overrides + the todo reset.
    assert_eq!(task_status(&conn, "OF-R4").as_deref(), Some("todo"));
    assert_eq!(
        ctx.model_overrides.get("OF-R4").map(String::as_str),
        Some("grok-build"),
        "rung 4 must record the cross-provider model override on ctx",
    );
    assert_eq!(
        ctx.runner_overrides.get("OF-R4"),
        Some(&RunnerKind::Grok),
        "rung 4 must record the Grok runner override on ctx",
    );
}

// ---------------------------------------------------------------------------
// AC1 (parity): the sequential (slot_index=None) and wave (slot_index=Some)
// shapes route through the SAME coordinator, so identical (ctx, task, effort,
// model) inputs select the same rung — only the JSONL slot_index differs.
// ---------------------------------------------------------------------------

#[test]
fn handle_overflow_seq_and_wave_agree_on_rung() {
    disable_llm_extraction();

    let (seq_tmp, mut seq_conn) = setup_migrated_db();
    insert_run(&seq_conn);
    insert_task_with_model(&seq_conn, "OF-PAR", None);
    let mut seq_ctx = IterationContext::new(10);
    let seq_action = run_handle_overflow(
        &mut seq_conn,
        &mut seq_ctx,
        "OF-PAR",
        Some("high"),
        Some(SONNET_MODEL),
        7,
        seq_tmp.path(),
        None,
        RunnerKind::Claude,
        &ProjectConfig::default(),
    );

    let (wave_tmp, mut wave_conn) = setup_migrated_db();
    insert_run(&wave_conn);
    insert_task_with_model(&wave_conn, "OF-PAR", None);
    let mut wave_ctx = IterationContext::new(10);
    let wave_action = run_handle_overflow(
        &mut wave_conn,
        &mut wave_ctx,
        "OF-PAR",
        Some("high"),
        Some(SONNET_MODEL),
        7,
        wave_tmp.path(),
        Some(2),
        RunnerKind::Claude,
        &ProjectConfig::default(),
    );

    assert_eq!(
        seq_action, wave_action,
        "sequential (None) and wave (Some(slot)) shapes must select the SAME rung",
    );
    // The ONLY shape-dependent observable is the JSONL slot_index.
    let seq_events = read_overflow_events(seq_tmp.path());
    let wave_events = read_overflow_events(wave_tmp.path());
    assert_eq!(seq_events.len(), 1);
    assert_eq!(wave_events.len(), 1);
    assert_eq!(
        seq_events[0].slot_index, None,
        "sequential event must omit slot_index",
    );
    assert_eq!(
        wave_events[0].slot_index,
        Some(2),
        "wave event must carry the slot index it overflowed on",
    );
}

// ---------------------------------------------------------------------------
// AC2: per-slot isolation. In a 4-slot wave where only slot 2 overflows (walked
// through rungs 1 → 2), the per-task recovery channels on ctx contain ONLY slot
// 2's task_id — sibling slot task_ids never leak in. Mirrors
// `overflow_per_slot.rs::slot_2_recovery_keying_excludes_sibling_slot_task_ids`
// but routes through the coordinator.
// ---------------------------------------------------------------------------

#[test]
fn handle_overflow_per_slot_keying_excludes_sibling_slot_task_ids() {
    disable_llm_extraction();
    let (tmp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    let slots = ["OF-WAVE-S0", "OF-WAVE-S1", "OF-WAVE-S2", "OF-WAVE-S3"];
    for id in &slots {
        insert_task_with_model(&conn, id, None);
    }
    let mut ctx = IterationContext::new(10);

    // Slot 2 overflows on rung 1 (effort downgrade)...
    run_handle_overflow(
        &mut conn,
        &mut ctx,
        slots[2],
        Some("xhigh"),
        Some(SONNET_MODEL),
        1,
        tmp.path(),
        Some(2),
        RunnerKind::Claude,
        &ProjectConfig::default(),
    );
    // ...is re-picked (production re-claims the row) and overflows again on
    // rung 2 (model escalation) so we exercise BOTH effort + model channels.
    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
        [slots[2]],
    )
    .unwrap();
    run_handle_overflow(
        &mut conn,
        &mut ctx,
        slots[2],
        Some("high"),
        Some(SONNET_MODEL),
        2,
        tmp.path(),
        Some(2),
        RunnerKind::Claude,
        &ProjectConfig::default(),
    );

    // Exactly one entry in every per-task channel, keyed on slot 2's id.
    assert!(ctx.overflow_recovered.contains(slots[2]));
    assert!(ctx.effort_overrides.contains_key(slots[2]));
    assert!(ctx.model_overrides.contains_key(slots[2]));
    assert!(ctx.overflow_original_model.contains_key(slots[2]));
    for &sibling in &[slots[0], slots[1], slots[3]] {
        assert!(
            !ctx.overflow_recovered.contains(sibling),
            "overflow_recovered leaked sibling {sibling}",
        );
        assert!(
            !ctx.effort_overrides.contains_key(sibling),
            "effort_overrides leaked sibling {sibling}",
        );
        assert!(
            !ctx.model_overrides.contains_key(sibling),
            "model_overrides leaked sibling {sibling}",
        );
        assert!(
            !ctx.overflow_original_model.contains_key(sibling),
            "overflow_original_model leaked sibling {sibling}",
        );
    }
    // Set sizes pin the count — a wave-level or empty-string key would bump these.
    assert_eq!(ctx.overflow_recovered.len(), 1);
    assert_eq!(ctx.effort_overrides.len(), 1);
    assert_eq!(ctx.model_overrides.len(), 1);
    assert_eq!(ctx.overflow_original_model.len(), 1);
}

// ---------------------------------------------------------------------------
// AC3: path-traversal defense preserved. A `..`-laden task id is collapsed by
// `sanitize_id_for_filename` before it ever reaches the filesystem, so the
// dump file `handle_overflow` writes stays inside `overflow-dumps/` — no `..`
// or `/` survives into the on-disk filename or the JSONL dump_path.
// ---------------------------------------------------------------------------

#[test]
fn handle_overflow_sanitizes_path_traversal_task_id() {
    disable_llm_extraction();
    let (tmp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    let evil_id = "../../../etc/passwd";
    insert_task_with_model(&conn, evil_id, None);
    let mut ctx = IterationContext::new(10);

    run_handle_overflow(
        &mut conn,
        &mut ctx,
        evil_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        1,
        tmp.path(),
        None,
        RunnerKind::Claude,
        &ProjectConfig::default(),
    );

    // The dump lands inside overflow-dumps/ with a collapsed name (no traversal).
    let dumps_dir = tmp.path().join("overflow-dumps");
    let entries: Vec<_> = std::fs::read_dir(&dumps_dir)
        .expect("overflow-dumps dir exists")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(entries.len(), 1, "exactly one dump file written");
    let name = &entries[0];
    assert!(
        !name.contains("..") && !name.contains('/'),
        "dump filename must collapse traversal segments; got {name}",
    );
    let sanitized = sanitize_id_for_filename(evil_id);
    assert!(
        name.starts_with(&sanitized),
        "dump filename must begin with the sanitized id {sanitized}; got {name}",
    );

    // The JSONL dump_path points inside the dumps dir — never above base_dir.
    let events = read_overflow_events(tmp.path());
    assert_eq!(events.len(), 1);
    assert!(
        events[0]
            .dump_path
            .starts_with(&dumps_dir.to_string_lossy().into_owned()),
        "JSONL dump_path must stay within overflow-dumps/; got {}",
        events[0].dump_path,
    );
    assert!(
        !events[0].dump_path.contains(".."),
        "JSONL dump_path must not contain a traversal segment",
    );
}

// ---------------------------------------------------------------------------
// AC4: the OverflowEvent JSONL line still carries the core fields
// (task_id / iteration / model / recovery) through the coordinator.
// ---------------------------------------------------------------------------

#[test]
fn handle_overflow_emits_jsonl_event_with_core_fields() {
    disable_llm_extraction();
    let (tmp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_task_with_model(&conn, "OF-JSONL", None);
    let mut ctx = IterationContext::new(10);

    let action = run_handle_overflow(
        &mut conn,
        &mut ctx,
        "OF-JSONL",
        Some("xhigh"),
        Some(SONNET_MODEL),
        42,
        tmp.path(),
        None,
        RunnerKind::Claude,
        &ProjectConfig::default(),
    );

    let events = read_overflow_events(tmp.path());
    assert_eq!(events.len(), 1, "exactly one JSONL event emitted");
    let ev = &events[0];
    assert_eq!(ev.task_id, "OF-JSONL", "task_id field must be populated");
    assert_eq!(ev.iteration, 42, "iteration field must be populated");
    assert_eq!(
        ev.model.as_deref(),
        Some(SONNET_MODEL),
        "model field must reflect the effective model at overflow time",
    );
    assert_eq!(
        ev.recovery, action,
        "the JSONL recovery field must match the returned RecoveryAction",
    );
}

// ---------------------------------------------------------------------------
// AC5 (RUNS LIVE): known-bad discriminator. A stub overflow ladder that SKIPS
// rung 1 (effort downgrade) jumps straight to model escalation, so on the
// (xhigh, sonnet) input it yields EscalateModel{opus} instead of
// DowngradeEffort{high}. This proves the rung-1 assertion in
// `handle_overflow_rung_ladder_matches_pre_relocation` actually discriminates:
// a regression that drops the effort-downgrade rung is rejected.
// ---------------------------------------------------------------------------

/// A deliberately-wrong ladder: ignores `effort` and never tries rung 1.
fn naive_overflow_skipping_rung1(_effort: Option<&str>, model: Option<&str>) -> RecoveryAction {
    if let Some(next) = escalate_below_opus(model) {
        RecoveryAction::EscalateModel {
            new_model: next.to_string(),
        }
    } else if let Some(m1m) = to_1m_model(model) {
        RecoveryAction::To1mModel {
            new_model: m1m.to_string(),
        }
    } else {
        RecoveryAction::Blocked
    }
}

#[test]
fn known_bad_overflow_skipping_rung1_fails_the_downgrade_assertion() {
    // Same (effort, model) the rung-1 case feeds the real coordinator.
    let action = naive_overflow_skipping_rung1(Some("xhigh"), Some(SONNET_MODEL));

    // The real coordinator returns DowngradeEffort{high}; the rung-1-skipping
    // stub returns EscalateModel{opus}, so it WOULD fail that assertion.
    assert_eq!(
        action,
        RecoveryAction::EscalateModel {
            new_model: OPUS_MODEL.to_string()
        },
        "rung-1-skipping stub jumps to model escalation on xhigh effort",
    );
    assert_ne!(
        action,
        RecoveryAction::DowngradeEffort {
            new_effort: "high".to_string()
        },
        "...which violates the rung-1-first contract the real coordinator satisfies",
    );
}

// ===========================================================================
// FEAT-014 — transient-backend reaction (`react_to_transient`) parity. Pins
// the contract that the converged coordinator
// `reactions::account::react_to_transient_inner` performs a BOUNDED
// backoff-retry on `IterationOutcome::TransientBackend` (HTTP 5xx / overloaded),
// reusing the rate-limit reset+wait scaffold: reset affected `in_progress`
// task(s) to `todo`, wait EXACTLY ONCE per wave (honoring `retry_after_secs`
// when present, else exponential `base*2^attempt` capped at `max`), report
// `WaitedAndRetry`, escalate after the attempt cap, and `Stop` on `.stop`.
//
// Like TEST-INIT-001's rate-limit cases these are hermetic: the wait is
// injected via the same `WaitFn` seam (a `WaitSpy`), and the bounded-attempt
// counter is a plain `&mut u32` the test owns. Sequential (1-item) and wave
// (N-item) shapes are exercised, plus a negative control.
// ===========================================================================

use task_mgr::loop_engine::reactions::account::{
    TRANSIENT_BACKOFF_BASE_SECS, TRANSIENT_BACKOFF_MAX_SECS, TRANSIENT_MAX_ATTEMPTS,
    TransientReaction, TransientReactionParams, react_to_transient_inner,
};

/// Standard transient params for a hermetic inner call, using the production
/// constants for the cap + backoff curve.
fn transient_params(tasks_dir: &Path) -> TransientReactionParams<'_> {
    TransientReactionParams {
        tasks_dir,
        prefix: PREFIX,
        run_id: RUN_ID,
        max_attempts: TRANSIENT_MAX_ATTEMPTS,
        base_wait_secs: TRANSIENT_BACKOFF_BASE_SECS,
        max_wait_secs: TRANSIENT_BACKOFF_MAX_SECS,
    }
}

/// An `IterationOutcome::TransientBackend` with no parsed `Retry-After`.
fn transient_no_retry() -> IterationOutcome {
    IterationOutcome::TransientBackend {
        retry_after_secs: None,
    }
}

// ---------------------------------------------------------------------------
// AC: Wait-once — a 3-slot wave with 2 TransientBackend items fires the
// injected wait closure EXACTLY once (never once per transient slot).
// ---------------------------------------------------------------------------

#[test]
fn transient_wait_once_for_multi_transient_wave() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-TR-0");
    insert_done_task(&conn, "RP-DONE-1");
    insert_in_progress_task(&conn, "RP-TR-2");

    let (tr, done) = (transient_no_retry(), IterationOutcome::Completed);
    let items = [
        OutputReactionItem {
            task_id: Some("RP-TR-0"),
            outcome: &tr,
            output: "502 bad gateway",
        },
        OutputReactionItem {
            task_id: Some("RP-DONE-1"),
            outcome: &done,
            output: "<completed>RP-DONE-1</completed>",
        },
        OutputReactionItem {
            task_id: Some("RP-TR-2"),
            outcome: &tr,
            output: "502 bad gateway",
        },
    ];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    let mut attempts: u32 = 0;
    let reaction = react_to_transient_inner(
        &mut conn,
        &items,
        &transient_params(db_temp.path()),
        &mut attempts,
        &wait as WaitFn,
    );

    assert_eq!(
        spy.calls.get(),
        1,
        "the transient backoff must fire EXACTLY once per wave, not once per transient slot",
    );
    assert_eq!(reaction, TransientReaction::WaitedAndRetry);
    assert_eq!(
        attempts, 1,
        "WaitedAndRetry must increment the attempt counter"
    );
}

// ---------------------------------------------------------------------------
// AC: Mixed wave — one TransientBackend + one Completed leaves the completed
// task `done` and resets only the transient task to `todo` (B1).
// ---------------------------------------------------------------------------

#[test]
fn transient_mixed_wave_resets_only_transient_and_preserves_completed() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-TR-1");
    insert_done_task(&conn, "RP-DONE-1");

    let (tr, done) = (transient_no_retry(), IterationOutcome::Completed);
    let items = [
        OutputReactionItem {
            task_id: Some("RP-TR-1"),
            outcome: &tr,
            output: "503 service unavailable",
        },
        OutputReactionItem {
            task_id: Some("RP-DONE-1"),
            outcome: &done,
            output: "<completed>RP-DONE-1</completed>",
        },
    ];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    let mut attempts: u32 = 0;
    let reaction = react_to_transient_inner(
        &mut conn,
        &items,
        &transient_params(db_temp.path()),
        &mut attempts,
        &wait as WaitFn,
    );

    assert_eq!(reaction, TransientReaction::WaitedAndRetry);
    assert_eq!(
        task_status(&conn, "RP-DONE-1").as_deref(),
        Some("done"),
        "the completed task must remain `done` — the reset must not clobber it (B1)",
    );
    assert_eq!(
        task_status(&conn, "RP-TR-1").as_deref(),
        Some("todo"),
        "the transient in_progress task must be reset to `todo` for retry",
    );
}

// ---------------------------------------------------------------------------
// AC: honor retry_after_secs when present (overrides the exponential backoff).
// ---------------------------------------------------------------------------

#[test]
fn transient_honors_retry_after_secs() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-TR-1");

    // attempt=0 would otherwise back off TRANSIENT_BACKOFF_BASE_SECS; the
    // Retry-After must win.
    let tr = IterationOutcome::TransientBackend {
        retry_after_secs: Some(45),
    };
    let items = [OutputReactionItem {
        task_id: Some("RP-TR-1"),
        outcome: &tr,
        output: "529 overloaded; Retry-After: 45",
    }];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    let mut attempts: u32 = 0;
    react_to_transient_inner(
        &mut conn,
        &items,
        &transient_params(db_temp.path()),
        &mut attempts,
        &wait as WaitFn,
    );

    assert_eq!(
        spy.last_secs.get(),
        Some(45),
        "a present Retry-After must override the exponential backoff",
    );
    assert_ne!(
        spy.last_secs.get(),
        Some(TRANSIENT_BACKOFF_BASE_SECS),
        "...and NOT use the exponential base",
    );
}

// ---------------------------------------------------------------------------
// AC: exponential backoff base*2^attempt when no Retry-After — sequential
// (1-item) and wave (N-item) shapes compute the SAME wait for the same attempt.
// ---------------------------------------------------------------------------

#[test]
fn transient_exponential_backoff_and_both_shapes_agree() {
    disable_llm_extraction();

    // Sequential shape: single transient item, attempt counter at 2.
    let (seq_temp, mut seq_conn) = setup_migrated_db();
    insert_run(&seq_conn);
    insert_in_progress_task(&seq_conn, "RP-TR-1");
    let tr = transient_no_retry();
    let seq_items = [OutputReactionItem {
        task_id: Some("RP-TR-1"),
        outcome: &tr,
        output: "bad gateway, no retry-after",
    }];
    let seq_spy = WaitSpy::completing();
    let seq_wait = seq_spy.closure();
    let mut seq_attempts: u32 = 2;
    react_to_transient_inner(
        &mut seq_conn,
        &seq_items,
        &transient_params(seq_temp.path()),
        &mut seq_attempts,
        &seq_wait as WaitFn,
    );

    // Wave shape: same transient item (attempt 2) plus a completed sibling.
    let (wave_temp, mut wave_conn) = setup_migrated_db();
    insert_run(&wave_conn);
    insert_in_progress_task(&wave_conn, "RP-TR-1");
    insert_done_task(&wave_conn, "RP-DONE-2");
    let done = IterationOutcome::Completed;
    let wave_items = [
        OutputReactionItem {
            task_id: Some("RP-TR-1"),
            outcome: &tr,
            output: "bad gateway, no retry-after",
        },
        OutputReactionItem {
            task_id: Some("RP-DONE-2"),
            outcome: &done,
            output: "<completed>RP-DONE-2</completed>",
        },
    ];
    let wave_spy = WaitSpy::completing();
    let wave_wait = wave_spy.closure();
    let mut wave_attempts: u32 = 2;
    react_to_transient_inner(
        &mut wave_conn,
        &wave_items,
        &transient_params(wave_temp.path()),
        &mut wave_attempts,
        &wave_wait as WaitFn,
    );

    // attempt 2 ⇒ base * 2^2 = base * 4 (well under the cap for the defaults).
    let expected = TRANSIENT_BACKOFF_BASE_SECS * 4;
    assert_eq!(
        seq_spy.last_secs.get(),
        Some(expected),
        "attempt 2 must back off base*2^2",
    );
    assert_eq!(
        seq_spy.last_secs.get(),
        wave_spy.last_secs.get(),
        "sequential (1-item) and wave (N-item) shapes must compute the SAME backoff",
    );
    assert_eq!(seq_spy.calls.get(), 1);
    assert_eq!(wave_spy.calls.get(), 1);
}

// ---------------------------------------------------------------------------
// AC: bounded attempts — at the cap, the reaction Escalates (no wait, no
// reset) so the caller falls through to the crash/abort path.
// ---------------------------------------------------------------------------

#[test]
fn transient_escalates_at_attempt_cap_without_waiting_or_resetting() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-TR-1");
    let before = all_task_statuses(&conn);

    let tr = transient_no_retry();
    let items = [OutputReactionItem {
        task_id: Some("RP-TR-1"),
        outcome: &tr,
        output: "502 bad gateway",
    }];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    // Counter already AT the cap.
    let mut attempts: u32 = TRANSIENT_MAX_ATTEMPTS;
    let reaction = react_to_transient_inner(
        &mut conn,
        &items,
        &transient_params(db_temp.path()),
        &mut attempts,
        &wait as WaitFn,
    );

    assert_eq!(
        reaction,
        TransientReaction::Escalate,
        "at the attempt cap the reaction must escalate, not keep waiting",
    );
    assert_eq!(spy.calls.get(), 0, "escalation must NOT wait");
    assert_eq!(
        all_task_statuses(&conn),
        before,
        "escalation must NOT reset the in_progress task — the crash path handles it",
    );
}

// ---------------------------------------------------------------------------
// AC: Stop — injected `.stop` during the backoff → TransientReaction::Stop.
// ---------------------------------------------------------------------------

#[test]
fn transient_stop_signal_during_wait_returns_stop() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn);
    insert_in_progress_task(&conn, "RP-TR-1");

    let tr = transient_no_retry();
    let items = [OutputReactionItem {
        task_id: Some("RP-TR-1"),
        outcome: &tr,
        output: "504 gateway timeout",
    }];

    let spy = WaitSpy::stopping();
    let wait = spy.closure();
    let mut attempts: u32 = 0;
    let reaction = react_to_transient_inner(
        &mut conn,
        &items,
        &transient_params(db_temp.path()),
        &mut attempts,
        &wait as WaitFn,
    );

    assert_eq!(
        spy.calls.get(),
        1,
        "the backoff wait must have been attempted once"
    );
    assert_eq!(
        reaction,
        TransientReaction::Stop,
        "a backoff interrupted by `.stop` must return Stop",
    );
}

// ---------------------------------------------------------------------------
// AC: Negative control — a slice with no TransientBackend returns
// TransientReaction::None, resets the attempt counter, performs ZERO DB writes,
// and never waits.
// ---------------------------------------------------------------------------

#[test]
fn no_transient_returns_none_resets_counter_and_writes_nothing() {
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
            output: "still working, no backend error",
        },
    ];

    let spy = WaitSpy::completing();
    let wait = spy.closure();
    // A non-zero counter from a prior outage must be reset by the None branch.
    let mut attempts: u32 = 3;
    let reaction = react_to_transient_inner(
        &mut conn,
        &items,
        &transient_params(db_temp.path()),
        &mut attempts,
        &wait as WaitFn,
    );

    assert_eq!(
        reaction,
        TransientReaction::None,
        "no TransientBackend item ⇒ TransientReaction::None",
    );
    assert_eq!(
        attempts, 0,
        "a non-transient wave breaks the streak — the attempt counter resets to 0",
    );
    assert_eq!(
        spy.calls.get(),
        0,
        "no transient ⇒ the wait must NEVER fire"
    );
    assert_eq!(
        all_task_statuses(&conn),
        before,
        "no transient ⇒ ZERO DB writes (in_progress task must NOT be reset)",
    );
}

// ===========================================================================
// TEST-INIT-004 — human-review (#10) + budget accounting (#13) parity.
//
// Pins TWO contracts:
//
//   #10  `reactions::post_completion::react_to_completions_inner` fires the
//        human-review seam EXACTLY once per `requires_human` completed task,
//        for the SAME completed task on both the sequential (1 completed id)
//        and the wave (N completed ids) shapes — INCLUDING on a partial
//        WaitedAndRetry wave (a slot completed a requires_human task while a
//        sibling slot is rate-limited; the intentional behavior addition). It
//        is input-driven: only ids IN the provided `completed_ids` set are
//        reviewed — it does NOT rediscover completions by timestamp, which is
//        what preserves the intra-wave ordering (post-merge reconcile feeds the
//        id set before the external-git shadow). FEAT-010 fills the body and
//        removes the `#[ignore]` attributes.
//
//   #13  `reactions::account_iteration_budget` is the single home for the
//        iteration-budget rule: a `RateLimit` / `WaitedAndRetry` (give-back)
//        outcome does NOT consume the loop-bound `iteration` against
//        `max_iterations` on EITHER path, and a consuming outcome advances the
//        `iterations_completed` stat. A persistently rate-limited run stays
//        bounded — it terminates on `.stop`/signal, never on the iteration
//        ceiling (give-back pins the loop-bound counter). FEAT-013 fills the
//        body.
//
// Hermetic, mirroring TEST-INIT-001/002/003: the human-review is injected as a
// recording [`ReviewSpy`] closure (`ReviewFn`) so no stdin is read and no
// Claude subprocess spawns; the budget cases are pure counter arithmetic. Two
// LIVE known-bad discriminators (a DB-scan reviewer that ignores
// `completed_ids`; a budget helper that skips only the stat) prove the ignored
// assertions actually discriminate, and a LIVE harness-validity marker proves
// the fixtures + new pub contract types wire up.
//
// Per-AC mapping:
//   AC1 → react_to_completions_human_review_fires_on_both_shapes
//   AC2 → react_to_completions_human_review_fires_on_partial_waited_and_retry_wave
//   AC3 → react_to_completions_is_input_driven_not_timestamp_rediscovery
//         + known_bad_db_scan_review_fires_for_out_of_set_task (LIVE)
//   AC4 → budget_rate_limit_wave_does_not_consume_iteration_on_both_shapes
//   AC5 → budget_persistent_rate_limit_terminates_on_stop_not_max_iterations
//   AC6 → known_bad_budget_skipping_only_stat_fails_the_no_consumption_assertion (LIVE)
// ===========================================================================

use std::cell::RefCell;

use task_mgr::loop_engine::reactions::post_completion::{
    HumanReviewTask, PostCompletionParams, ReviewFn, react_to_completions_inner,
};
use task_mgr::loop_engine::reactions::{IterationBudgetParams, account_iteration_budget};

/// Insert a `requires_human = 1`, `status = 'done'` task with a recent
/// `completed_at` — the shape `query_human_review_tasks` selects today, and the
/// row `react_to_completions` must review iff its id is in the completed set.
fn insert_requires_human_done(
    conn: &Connection,
    task_id: &str,
    notes: Option<&str>,
    timeout: Option<u32>,
) {
    conn.execute(
        "INSERT INTO tasks \
         (id, title, status, priority, requires_human, notes, human_review_timeout, completed_at) \
         VALUES (?1, ?2, 'done', 50, 1, ?3, ?4, '2099-01-01T12:00:00')",
        rusqlite::params![task_id, format!("Review {task_id}"), notes, timeout],
    )
    .expect("insert requires_human done task row");
}

/// Spy implementing the injected human-review seam. `RefCell`-based (the seam is
/// `Fn`, not `FnMut`) so a `&self` closure can record each reviewed task id.
/// `had_feedback` is what the review reports — kept `false` in every test so the
/// PRD-mutation path is never exercised (the cases stay hermetic).
struct ReviewSpy {
    reviewed: RefCell<Vec<String>>,
    had_feedback: bool,
}

impl ReviewSpy {
    fn no_feedback() -> Self {
        Self {
            reviewed: RefCell::new(Vec::new()),
            had_feedback: false,
        }
    }

    /// A `Fn(HumanReviewTask) -> bool` closure that records each invocation.
    fn closure(&self) -> impl Fn(HumanReviewTask<'_>) -> bool + '_ {
        move |task| {
            self.reviewed.borrow_mut().push(task.task_id.to_string());
            self.had_feedback
        }
    }

    fn reviewed_ids(&self) -> Vec<String> {
        self.reviewed.borrow().clone()
    }
}

/// Standard post-completion params for a hermetic inner call. `working_root` and
/// `prd_file` are throwaway paths (the injected review never commits or mutates
/// the PRD because the spy reports no feedback).
fn post_completion_params<'a>(
    working_root: &'a Path,
    prd_file: &'a Path,
) -> PostCompletionParams<'a> {
    PostCompletionParams {
        run_id: RUN_ID,
        iteration: 1,
        working_root,
        prd_file,
        task_prefix: Some(PREFIX),
        default_model: None,
        permission_mode: &PERMISSION_MODE,
        external_repo_path: None,
        external_git_scan_depth: 50,
        wrapper_commit: true,
    }
}

// ---------------------------------------------------------------------------
// AC1: human-review fires for the SAME completed requires_human task on both
// the sequential (1 completed id) and wave (N completed ids) shapes; an
// ordinary completion in the wave set is NOT reviewed.
// ---------------------------------------------------------------------------

#[test]
fn react_to_completions_human_review_fires_on_both_shapes() {
    disable_llm_extraction();

    // Sequential shape: a single completed id, which is requires_human.
    let (seq_db, mut seq_conn) = setup_migrated_db();
    insert_run(&seq_conn);
    insert_requires_human_done(&seq_conn, "RP-HR-1", Some("confirm rate limit"), Some(900));
    let seq_ids = vec!["RP-HR-1".to_string()];
    let seq_spy = ReviewSpy::no_feedback();
    let seq_review = seq_spy.closure();
    let seq_tmp = TempDir::new().expect("tempdir");
    let seq_prd = seq_tmp.path().join("prd.json");
    react_to_completions_inner(
        &mut seq_conn,
        &seq_ids,
        &post_completion_params(seq_db.path(), &seq_prd),
        &seq_review as ReviewFn,
    );

    // Wave shape: N completed ids; only one is requires_human.
    let (wave_db, mut wave_conn) = setup_migrated_db();
    insert_run(&wave_conn);
    insert_requires_human_done(&wave_conn, "RP-HR-1", Some("confirm rate limit"), Some(900));
    insert_done_task(&wave_conn, "RP-DONE-2"); // ordinary completion, not requires_human
    let wave_ids = vec!["RP-HR-1".to_string(), "RP-DONE-2".to_string()];
    let wave_spy = ReviewSpy::no_feedback();
    let wave_review = wave_spy.closure();
    let wave_tmp = TempDir::new().expect("tempdir");
    let wave_prd = wave_tmp.path().join("prd.json");
    react_to_completions_inner(
        &mut wave_conn,
        &wave_ids,
        &post_completion_params(wave_db.path(), &wave_prd),
        &wave_review as ReviewFn,
    );

    assert_eq!(
        seq_spy.reviewed_ids(),
        vec!["RP-HR-1".to_string()],
        "sequential: the single completed requires_human task is reviewed exactly once",
    );
    assert_eq!(
        wave_spy.reviewed_ids(),
        vec!["RP-HR-1".to_string()],
        "wave: the same requires_human task is reviewed; the ordinary completion is NOT",
    );
    assert_eq!(
        seq_spy.reviewed_ids(),
        wave_spy.reviewed_ids(),
        "both shapes agree on which completed tasks trigger human review",
    );
}

// ---------------------------------------------------------------------------
// AC2: human-review fires on a partial WaitedAndRetry wave — a slot completed a
// requires_human task (in the completed set) while a sibling slot is
// rate-limited (in_progress, absent from the set). Intentional behavior
// addition: the wave path gains the human-review trigger.
// ---------------------------------------------------------------------------

#[test]
fn react_to_completions_human_review_fires_on_partial_waited_and_retry_wave() {
    disable_llm_extraction();
    let (db, mut conn) = setup_migrated_db();
    insert_run(&conn);
    // Slot 0 completed a requires_human task; slot 1 rate-limited (the wave
    // reported WaitedAndRetry for it, leaving it in_progress).
    insert_requires_human_done(&conn, "RP-HR-DONE", None, None);
    insert_in_progress_task(&conn, "RP-RATE-1");

    // Only the completed slot's id is in the set; the rate-limited slot is not.
    let completed_ids = vec!["RP-HR-DONE".to_string()];
    let spy = ReviewSpy::no_feedback();
    let review = spy.closure();
    let tmp = TempDir::new().expect("tempdir");
    let prd = tmp.path().join("prd.json");
    react_to_completions_inner(
        &mut conn,
        &completed_ids,
        &post_completion_params(db.path(), &prd),
        &review as ReviewFn,
    );

    assert_eq!(
        spy.reviewed_ids(),
        vec!["RP-HR-DONE".to_string()],
        "human-review fires for the completed requires_human task even on a partial \
         WaitedAndRetry wave (intentional behavior addition)",
    );
    assert_eq!(
        task_status(&conn, "RP-RATE-1").as_deref(),
        Some("in_progress"),
        "the rate-limited slot is untouched by the completion reaction",
    );
}

// ---------------------------------------------------------------------------
// AC3: react_to_completions consumes the PROVIDED completed-id set — only ids
// in the set are reviewed, even when the DB holds another requires_human/done
// task with an equally-recent completed_at. This is what preserves the
// intra-wave ordering (the caller feeds post-merge-reconcile ids before the
// external-git shadow; the reaction never rediscovers by timestamp).
// ---------------------------------------------------------------------------

#[test]
fn react_to_completions_is_input_driven_not_timestamp_rediscovery() {
    disable_llm_extraction();
    let (db, mut conn) = setup_migrated_db();
    insert_run(&conn);
    // TWO requires_human tasks, both done with a recent completed_at — a
    // timestamp-rediscovery impl would review BOTH. Only one is in the set.
    insert_requires_human_done(&conn, "RP-HR-IN", None, None);
    insert_requires_human_done(&conn, "RP-HR-OUT", None, None);

    let completed_ids = vec!["RP-HR-IN".to_string()];
    let spy = ReviewSpy::no_feedback();
    let review = spy.closure();
    let tmp = TempDir::new().expect("tempdir");
    let prd = tmp.path().join("prd.json");
    react_to_completions_inner(
        &mut conn,
        &completed_ids,
        &post_completion_params(db.path(), &prd),
        &review as ReviewFn,
    );

    assert_eq!(
        spy.reviewed_ids(),
        vec!["RP-HR-IN".to_string()],
        "only the requires_human task whose id is IN the provided completed set is \
         reviewed — react_to_completions consumes the input set, it does NOT \
         rediscover completions by timestamp",
    );
}

// ---------------------------------------------------------------------------
// AC3 known-bad discriminator (RUNS LIVE). A naive selector that scans the DB
// for every requires_human/done row — ignoring `completed_ids` — reviews the
// out-of-set task too. The same spy the input-driven test asserts
// `== ["RP-HR-IN"]` on would see `["RP-HR-IN", "RP-HR-OUT"]` here, so the
// discriminator is real: a regression to timestamp/DB rediscovery fails AC3.
// ---------------------------------------------------------------------------

/// A deliberately-wrong selector: reviews EVERY requires_human/done row,
/// ignoring the provided `completed_ids` set (a rediscovery regression).
fn naive_review_by_db_scan(conn: &Connection, _completed_ids: &[String], review: ReviewFn<'_>) {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, notes, human_review_timeout FROM tasks \
             WHERE requires_human = 1 AND status = 'done' ORDER BY id",
        )
        .expect("prepare");
    let rows: Vec<(String, String, Option<String>, Option<u32>)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?
                    .and_then(|v| u32::try_from(v).ok()),
            ))
        })
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    for (id, title, notes, timeout) in &rows {
        review(HumanReviewTask {
            task_id: id,
            title,
            notes: notes.as_deref(),
            timeout_secs: *timeout,
        });
    }
}

#[test]
fn known_bad_db_scan_review_fires_for_out_of_set_task() {
    disable_llm_extraction();
    let (_db, conn) = setup_migrated_db();
    insert_run(&conn);
    insert_requires_human_done(&conn, "RP-HR-IN", None, None);
    insert_requires_human_done(&conn, "RP-HR-OUT", None, None);

    let completed_ids = vec!["RP-HR-IN".to_string()];
    let spy = ReviewSpy::no_feedback();
    let review = spy.closure();
    naive_review_by_db_scan(&conn, &completed_ids, &review as ReviewFn);

    assert_eq!(
        spy.reviewed_ids(),
        vec!["RP-HR-IN".to_string(), "RP-HR-OUT".to_string()],
        "naive DB-scan reviews the out-of-set requires_human task too",
    );
    assert_ne!(
        spy.reviewed_ids(),
        vec!["RP-HR-IN".to_string()],
        "...which violates the input-driven contract the real coordinator satisfies",
    );
}

// ---------------------------------------------------------------------------
// AC4: a RateLimit/WaitedAndRetry (give-back) wave does NOT consume the
// loop-bound iteration against max_iterations on EITHER path; both paths give
// back identically through the one helper. A consuming outcome advances the
// `iterations_completed` stat and leaves the loop-bound counter alone.
// ---------------------------------------------------------------------------

#[test]
fn budget_rate_limit_wave_does_not_consume_iteration_on_both_shapes() {
    // Sequential shape: the orchestrator RateLimit arm gives the loop-bound
    // iteration back (`iteration -= 1`) and does NOT bump iterations_completed.
    let mut seq_iter = 5u32;
    let mut seq_completed = 2u32;
    account_iteration_budget(IterationBudgetParams {
        iteration: &mut seq_iter,
        iterations_completed: &mut seq_completed,
        consumes_budget: false,
    });
    assert_eq!(
        seq_iter, 4,
        "sequential give-back returns the loop-bound iteration"
    );
    assert_eq!(
        seq_completed, 2,
        "a give-back does NOT advance iterations_completed"
    );

    // Wave shape: the `iteration_consumed == false` branch must give back the
    // SAME loop-bound iteration through the SAME helper.
    let mut wave_iter = 5u32;
    let mut wave_completed = 2u32;
    account_iteration_budget(IterationBudgetParams {
        iteration: &mut wave_iter,
        iterations_completed: &mut wave_completed,
        consumes_budget: false,
    });
    assert_eq!(
        wave_iter, seq_iter,
        "both shapes give back the loop-bound iteration identically"
    );
    assert_eq!(
        wave_completed, seq_completed,
        "both shapes leave the stat identical"
    );

    // A consuming outcome advances the stat and leaves the loop-bound counter
    // (already incremented at the loop top) as-is.
    let mut ok_iter = 5u32;
    let mut ok_completed = 2u32;
    account_iteration_budget(IterationBudgetParams {
        iteration: &mut ok_iter,
        iterations_completed: &mut ok_completed,
        consumes_budget: true,
    });
    assert_eq!(
        ok_iter, 5,
        "a consuming outcome leaves the loop-bound iteration unchanged"
    );
    assert_eq!(
        ok_completed, 3,
        "a consuming outcome advances iterations_completed"
    );
}

// ---------------------------------------------------------------------------
// AC5: a persistently rate-limited sequence stays bounded — it terminates on
// `.stop`/signal, NOT on max_iterations. Every pass is a give-back, so the
// loop-bound counter never advances toward the ceiling; the ONLY thing that
// ends the loop is the stop check. (Models the orchestrator loop:
// `while iteration < max { iteration += 1; ...; budget(give_back); if stop { break } }`.)
// ---------------------------------------------------------------------------

#[test]
fn budget_persistent_rate_limit_terminates_on_stop_not_max_iterations() {
    let max_iterations = 3u32;
    let stop_after = 7u32; // .stop arrives on the 7th pass — past max_iterations
    let safety_cap = 1000u32; // would-be-infinite guard; the test fails if it is hit

    let mut iteration = 0u32;
    let mut iterations_completed = 0u32;
    let mut passes = 0u32;
    let mut stopped = false;

    while iteration < max_iterations && passes < safety_cap {
        iteration += 1; // 1-based top-of-pass increment
        passes += 1;
        account_iteration_budget(IterationBudgetParams {
            iteration: &mut iteration,
            iterations_completed: &mut iterations_completed,
            consumes_budget: false, // persistently rate-limited
        });
        if passes >= stop_after {
            stopped = true;
            break; // .stop / signal observed during the wait
        }
    }

    assert!(
        stopped,
        "a persistently rate-limited run terminates via .stop/signal"
    );
    assert!(
        passes < safety_cap,
        "termination is bounded — the safety cap was never hit"
    );
    assert_eq!(
        passes, stop_after,
        "the loop ran until the stop signal; max_iterations never bounded it",
    );
    assert_eq!(
        iteration, 0,
        "give-back pinned the loop-bound counter at 0 — the iteration ceiling never fired",
    );
    assert_eq!(
        iterations_completed, 0,
        "no give-back pass advanced the completed stat",
    );
}

// ---------------------------------------------------------------------------
// AC6 known-bad discriminator (RUNS LIVE). A budget helper that correctly skips
// the `iterations_completed` stat on a give-back but FORGETS to return the
// loop-bound iteration leaves it consumed (the B2 bug FEAT-013 generalizes).
// The no-consumption test asserts `iteration == 4`; this stub leaves it at 5,
// so it WOULD fail that assertion — the discriminator is real.
// ---------------------------------------------------------------------------

/// A deliberately-wrong helper: skips the stat bump on a give-back (correct) but
/// never gives the loop-bound iteration back (BUG).
fn naive_budget_skips_only_stat(params: IterationBudgetParams<'_>) {
    let IterationBudgetParams {
        iteration,
        iterations_completed,
        consumes_budget,
    } = params;
    if consumes_budget {
        *iterations_completed += 1;
    }
    // BUG: the give-back path never does `*iteration = iteration.saturating_sub(1)`.
    let _ = iteration;
}

#[test]
fn known_bad_budget_skipping_only_stat_fails_the_no_consumption_assertion() {
    let mut iteration = 5u32;
    let mut iterations_completed = 2u32;
    naive_budget_skips_only_stat(IterationBudgetParams {
        iteration: &mut iteration,
        iterations_completed: &mut iterations_completed,
        consumes_budget: false,
    });

    assert_eq!(
        iteration, 5,
        "stat-only stub consumes the loop-bound iteration (no give-back)",
    );
    assert_ne!(
        iteration, 4,
        "...which violates the no-consumption contract the real helper satisfies",
    );
    assert_eq!(
        iterations_completed, 2,
        "the stat is correctly left unchanged on a give-back",
    );
}

// ---------------------------------------------------------------------------
// Harness-validity compile-marker (RUNS LIVE). Proves the TEST-INIT-004
// fixtures + new pub contract types wire up: a migrated DB stands up, a
// requires_human/done row inserts, both param structs are constructible (the
// budget one with live `&mut` borrows), and the review seam closure builds.
// Does NOT call the unimplemented coordinator/helper.
// ---------------------------------------------------------------------------

#[test]
fn completion_and_budget_harness_compiles_and_setup_works() {
    disable_llm_extraction();
    let (db, conn) = setup_migrated_db();
    insert_run(&conn);
    insert_requires_human_done(&conn, "RP-HR-MARK", Some("note"), Some(600));
    insert_done_task(&conn, "RP-DONE-MARK");

    // Post-completion params + review seam are constructible from the harness.
    let tmp = TempDir::new().expect("tempdir");
    let prd = tmp.path().join("prd.json");
    let _p = post_completion_params(db.path(), &prd);
    let spy = ReviewSpy::no_feedback();
    let _review = spy.closure();
    let _task = HumanReviewTask {
        task_id: "RP-HR-MARK",
        title: "Review",
        notes: Some("note"),
        timeout_secs: Some(600),
    };

    // Budget params are constructible with live `&mut` borrows.
    let mut iteration = 1u32;
    let mut iterations_completed = 0u32;
    let _bp = IterationBudgetParams {
        iteration: &mut iteration,
        iterations_completed: &mut iterations_completed,
        consumes_budget: true,
    };

    assert_eq!(task_status(&conn, "RP-HR-MARK").as_deref(), Some("done"));
    assert_eq!(task_status(&conn, "RP-DONE-MARK").as_deref(), Some("done"));
}
