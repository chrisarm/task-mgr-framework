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
#[ignore = "unblocked by FEAT-002: resolve_task_execution body"]
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
#[ignore = "unblocked by FEAT-002: resolve_task_execution body"]
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
#[ignore = "unblocked by FEAT-002: resolve_task_execution body"]
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
#[ignore = "unblocked by FEAT-002: resolve_task_execution body"]
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
#[ignore = "unblocked by FEAT-003: account_usage_gate_inner body"]
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
