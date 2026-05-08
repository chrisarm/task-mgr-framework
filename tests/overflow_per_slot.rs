//! Tests for per-slot prompt-overflow ladder dispatch (TEST-INIT-005).
//!
//! These tests pin down the contract FEAT-008 (Phase D) will deliver:
//! 1. When a slot's outcome is `Crash(PromptTooLong)`, the engine MUST invoke
//!    `overflow::handle_prompt_too_long` keyed on **that slot's** `task_id`.
//! 2. Per-slot recovery state on `IterationContext` (`model_overrides`,
//!    `overflow_recovered`, `overflow_original_model`) MUST be keyed on the
//!    crashing slot's `task_id` only — sibling slot task_ids in the same wave
//!    MUST NOT appear in those maps.
//! 3. `OverflowEvent` will gain `slot_index: Option<usize>` with
//!    `#[serde(skip_serializing_if = "Option::is_none")]`. Wave events serialize
//!    with the field; sequential events omit it (or render it as `null`).
//!
//! ### TDD scaffolding (learning #862)
//!
//! Tests that validate behavior currently exercised by `handle_prompt_too_long`
//! itself (per-task keying isolation, sequential JSONL shape) run today and
//! protect that contract from regression. Tests that require FEAT-008's new
//! `slot_index` plumbing (#3 / #4 in this file) are marked `#[ignore]` with a
//! note pointing at FEAT-008; FEAT-008 removes the ignore + implements the
//! field.
//!
//! Per `notes` on TEST-INIT-005: tests use synthetic `IterationContext` and
//! call `handle_prompt_too_long` directly — no real waves are spawned. The
//! production type names (`IterationContext`, `OverflowEvent`,
//! `RecoveryAction`) are imported so refactors propagate type errors here.

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::loop_engine::config::{CrashType, IterationOutcome};
use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::model::SONNET_MODEL;
use task_mgr::loop_engine::overflow::{self, OverflowEvent, RecoveryAction};
use task_mgr::loop_engine::prompt::PromptResult;

// ---------- Test fixtures ---------------------------------------------------

/// Minimal in-memory `tasks` schema with N pre-seeded `in_progress` rows so
/// the SQL UPDATE inside `handle_prompt_too_long` actually flips a row.
fn make_conn_with_tasks(task_ids: &[&str]) -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute(
        r#"CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'todo',
            started_at TEXT
        )"#,
        [],
    )
    .expect("create tasks table");
    for id in task_ids {
        conn.execute(
            "INSERT INTO tasks (id, title, status, started_at) \
             VALUES (?1, 'fixture', 'in_progress', '2026-05-07T00:00:00Z')",
            [*id],
        )
        .expect("seed task row");
    }
    conn
}

fn task_status(conn: &Connection, task_id: &str) -> String {
    conn.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
        row.get::<_, String>(0)
    })
    .expect("status query")
}

fn make_prompt_result(task_id: &str) -> PromptResult {
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

fn read_events(base_dir: &std::path::Path) -> Vec<OverflowEvent> {
    let path = base_dir.join("overflow-events.jsonl");
    let raw = std::fs::read_to_string(&path).expect("jsonl exists");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<OverflowEvent>(l).expect("parse jsonl line"))
        .collect()
}

fn read_event_values(base_dir: &std::path::Path) -> Vec<serde_json::Value> {
    let raw =
        std::fs::read_to_string(base_dir.join("overflow-events.jsonl")).expect("jsonl exists");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("parse jsonl line"))
        .collect()
}

/// 4-slot wave: slot 0/1/3 run their tasks fine; slot 2 hits PromptTooLong.
/// Returns the four task IDs in slot order.
fn four_slot_task_ids() -> [&'static str; 4] {
    [
        "WAVE-SLOT0-TASK",
        "WAVE-SLOT1-TASK",
        "WAVE-SLOT2-TASK",
        "WAVE-SLOT3-TASK",
    ]
}

// ---------- AC #1: handler invocation keyed on slot 2's task_id -------------

/// Slot 2 hitting `Crash(PromptTooLong)` invokes `handle_prompt_too_long`
/// against slot 2's task_id and produces the expected rung action +
/// DB-row state. Slots 0/1/3 are not crashed and the helper is not invoked
/// for them; their task rows must remain `in_progress`.
#[test]
fn slot_2_prompt_too_long_invokes_handler_with_slot_2_task_id() {
    let tmp = TempDir::new().expect("tempdir");
    let task_ids = four_slot_task_ids();
    let conn = make_conn_with_tasks(&task_ids);
    let mut ctx = IterationContext::new(10);

    // The synthetic per-slot outcomes a wave dispatcher would observe.
    // Anchors AC #11-style assertion that the trigger condition is exactly
    // `Crash(PromptTooLong)` on slot 2.
    let slot_outcomes: [IterationOutcome; 4] = [
        IterationOutcome::Empty,
        IterationOutcome::Empty,
        IterationOutcome::Crash(CrashType::PromptTooLong),
        IterationOutcome::Empty,
    ];
    assert!(matches!(
        slot_outcomes[2],
        IterationOutcome::Crash(CrashType::PromptTooLong)
    ));

    let pr = make_prompt_result(task_ids[2]);

    // Wave dispatcher's job: for each slot whose outcome is PromptTooLong,
    // call handle_prompt_too_long with that slot's task_id. We simulate the
    // dispatcher inline.
    let mut last_action: Option<RecoveryAction> = None;
    for (slot_idx, outcome) in slot_outcomes.iter().enumerate() {
        if matches!(outcome, IterationOutcome::Crash(CrashType::PromptTooLong)) {
            let action = overflow::handle_prompt_too_long(
                &mut ctx,
                &conn,
                task_ids[slot_idx],
                Some("xhigh"),
                Some(SONNET_MODEL),
                &pr,
                1,
                Some("run-wave"),
                tmp.path(),
                Some(slot_idx),
            );
            last_action = Some(action);
        }
    }

    let action = last_action.expect("slot 2 must have triggered the handler");

    // Action: Sonnet+xhigh → rung 1 (downgrade_effort to high).
    assert!(
        matches!(action, RecoveryAction::DowngradeEffort { ref new_effort } if new_effort == "high"),
        "expected DowngradeEffort, got {action:?}",
    );

    // DB: slot 2's task row is reset to 'todo'; slots 0/1/3 untouched.
    assert_eq!(
        task_status(&conn, task_ids[2]),
        "todo",
        "slot 2 task must be reset to todo after rung 1",
    );
    for &untouched in &[task_ids[0], task_ids[1], task_ids[3]] {
        assert_eq!(
            task_status(&conn, untouched),
            "in_progress",
            "slot 0/1/3 task {untouched} must NOT be touched by slot-2 recovery",
        );
    }
}

// ---------- AC #2: per-slot keying isolation --------------------------------

/// After slot 2 recovery, `model_overrides` / `overflow_recovered` /
/// `overflow_original_model` contain ONLY slot 2's task_id — slots 0/1/3
/// task_ids are absent from those maps.
///
/// This is the critical test that catches a wave-mode dispatch that
/// accidentally writes the override under the wrong key (e.g. uses the
/// last-claimed task_id, or stores a wave-level marker).
#[test]
fn slot_2_recovery_keying_excludes_sibling_slot_task_ids() {
    let tmp = TempDir::new().expect("tempdir");
    let task_ids = four_slot_task_ids();
    let conn = make_conn_with_tasks(&task_ids);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_ids[2]);

    // Walk slot 2 from rung 1 through rung 2 so we exercise BOTH
    // effort_overrides AND model_overrides keying.
    let _r1 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_ids[2],
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        Some("run-wave"),
        tmp.path(),
        Some(2),
    );
    // Re-claim (production: task selection re-picks the row).
    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
        [task_ids[2]],
    )
    .unwrap();
    let _r2 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_ids[2],
        Some("high"),
        Some(SONNET_MODEL),
        &pr,
        2,
        Some("run-wave"),
        tmp.path(),
        Some(2),
    );

    // Only slot 2's task_id is present in the per-task state maps.
    assert!(
        ctx.overflow_recovered.contains(task_ids[2]),
        "overflow_recovered must contain slot 2's task_id",
    );
    assert!(
        ctx.effort_overrides.contains_key(task_ids[2]),
        "effort_overrides must contain slot 2's task_id after rung 1",
    );
    assert!(
        ctx.model_overrides.contains_key(task_ids[2]),
        "model_overrides must contain slot 2's task_id after rung 2",
    );
    assert!(
        ctx.overflow_original_model.contains_key(task_ids[2]),
        "overflow_original_model must contain slot 2's task_id",
    );

    // Sibling slot task_ids MUST NOT appear in any per-task map.
    for &sibling in &[task_ids[0], task_ids[1], task_ids[3]] {
        assert!(
            !ctx.overflow_recovered.contains(sibling),
            "overflow_recovered leaked sibling task_id {sibling}",
        );
        assert!(
            !ctx.effort_overrides.contains_key(sibling),
            "effort_overrides leaked sibling task_id {sibling}",
        );
        assert!(
            !ctx.model_overrides.contains_key(sibling),
            "model_overrides leaked sibling task_id {sibling}",
        );
        assert!(
            !ctx.overflow_original_model.contains_key(sibling),
            "overflow_original_model leaked sibling task_id {sibling}",
        );
    }

    // Set sizes confirm exactly one entry — guards against silent contamination
    // if a future change accidentally inserts an empty-string or wave-level key.
    assert_eq!(
        ctx.overflow_recovered.len(),
        1,
        "overflow_recovered must have exactly 1 entry, got {:?}",
        ctx.overflow_recovered,
    );
    assert_eq!(
        ctx.effort_overrides.len(),
        1,
        "effort_overrides must have exactly 1 entry, got {:?}",
        ctx.effort_overrides,
    );
    assert_eq!(
        ctx.model_overrides.len(),
        1,
        "model_overrides must have exactly 1 entry, got {:?}",
        ctx.model_overrides,
    );
    assert_eq!(
        ctx.overflow_original_model.len(),
        1,
        "overflow_original_model must have exactly 1 entry, got {:?}",
        ctx.overflow_original_model,
    );
}

// ---------- AC #3: JSONL contains slot_index for the slot 2 event -----------

/// Wave-mode JSONL: slot 2's overflow event has `"slot_index": 2`.
#[test]
fn slot_index_present_in_jsonl_for_wave_event() {
    let tmp = TempDir::new().expect("tempdir");
    let task_ids = four_slot_task_ids();
    let conn = make_conn_with_tasks(&task_ids);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_ids[2]);

    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_ids[2],
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        Some("run-wave"),
        tmp.path(),
        Some(2),
    );

    let raw_events = read_event_values(tmp.path());
    assert_eq!(raw_events.len(), 1, "expected one JSONL line for slot 2");
    let v = &raw_events[0];

    // Field must be present and equal to 2 — NOT null, NOT omitted.
    let slot_idx = v
        .get("slot_index")
        .expect("slot_index field must be present in wave-mode JSONL");
    assert_eq!(
        slot_idx,
        &serde_json::Value::Number(serde_json::Number::from(2u64)),
        "slot_index must serialize as numeric 2 for slot 2's event",
    );

    // Round-trip via the typed struct also recovers the value.
    let typed = read_events(tmp.path());
    assert_eq!(typed.len(), 1);
    assert_eq!(
        typed[0].slot_index,
        Some(2),
        "typed OverflowEvent.slot_index must be Some(2) for slot 2's wave event",
    );
}

// ---------- AC #4: slot_index omitted for sequential events -----------------

/// Sequential overflow events MUST NOT serialize a `slot_index` field.
///
/// This contract holds **today** (no field exists at all) AND post-FEAT-008
/// (`Option::is_none` triggers `skip_serializing_if`). The test is enabled
/// now to lock in the invariant before the field is introduced.
#[test]
fn slot_index_omitted_for_sequential_jsonl_event() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "SEQ-OVERFLOW-001";
    let conn = make_conn_with_tasks(&[task_id]);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Sequential overflow path — no slot context.
    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        Some("run-seq"),
        tmp.path(),
        None,
    );

    let events = read_event_values(tmp.path());
    assert_eq!(events.len(), 1, "expected one JSONL line");
    match events[0].get("slot_index") {
        None => {} // omitted — correct (today AND post-FEAT-008 with None)
        Some(serde_json::Value::Null) => {} // explicit null — also acceptable
        Some(other) => panic!(
            "sequential JSONL line must NOT carry a slot_index field; got {other:?}\n\
             full line: {}",
            events[0]
        ),
    }
}

// ---------- AC #5: sequential PromptTooLong unchanged -----------------------

/// The sequential overflow contract is the backstop: same call shape, same
/// rung action, same DB transition, same JSONL line count. This complements
/// the comprehensive ladder tests in `tests/overflow_recovery.rs` — failure
/// here means the per-slot dispatch work has regressed sequential mode.
#[test]
fn sequential_prompt_too_long_unchanged() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "SEQ-UNCHANGED-001";
    let conn = make_conn_with_tasks(&[task_id]);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        Some("run-seq"),
        tmp.path(),
        None,
    );

    // Rung 1 fired: effort downgrade.
    assert!(
        matches!(action, RecoveryAction::DowngradeEffort { ref new_effort } if new_effort == "high"),
        "sequential rung 1 must produce DowngradeEffort, got {action:?}",
    );
    // DB: task reset to todo.
    assert_eq!(task_status(&conn, task_id), "todo");
    // ctx: per-task keying populated.
    assert!(ctx.overflow_recovered.contains(task_id));
    assert_eq!(ctx.effort_overrides.get(task_id).copied(), Some("high"));
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
    );
    // JSONL: exactly one event line emitted; slot_index must be absent.
    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1, "sequential mode must emit one JSONL line");
    assert_eq!(events[0].task_id, task_id);
    assert_eq!(events[0].iteration, 1);
    assert_eq!(
        events[0].slot_index, None,
        "sequential event must have no slot_index"
    );
    assert!(matches!(
        events[0].recovery,
        RecoveryAction::DowngradeEffort { .. }
    ));
}

// ---------- AC #6: known-bad discriminator ----------------------------------

/// Negative test: simulates the **pre-FEAT-008** behavior where the wave-mode
/// dispatcher does NOT call `handle_prompt_too_long` on `Crash(PromptTooLong)`.
///
/// In that broken-dispatch world, slot 2's task_id NEVER appears in the
/// per-slot recovery maps. This test asserts that absence — proving the
/// keying assertion in `slot_2_recovery_keying_excludes_sibling_slot_task_ids`
/// has discriminating power: it would FAIL the positive-direction assertion
/// (`contains_key(slot_2_task_id)`) under a regression that drops the
/// dispatch.
///
/// Together with the positive test, this nails the contract: slot 2 MUST be
/// in the maps after a real dispatch, and CANNOT be in the maps without one.
#[test]
fn known_bad_no_dispatch_leaves_slot_2_keying_empty() {
    let tmp = TempDir::new().expect("tempdir");
    let task_ids = four_slot_task_ids();
    // Conn + ctx exist (the wave engine created them) but the broken
    // dispatcher never reaches the handler. Underscore-prefixed to silence
    // the unused-binding lint while documenting the production shape.
    let _conn = make_conn_with_tasks(&task_ids);
    let ctx = IterationContext::new(10);

    // The wave produced a Crash(PromptTooLong) on slot 2 — but the broken
    // dispatcher does NOT call handle_prompt_too_long. Simulate that by
    // observing the outcome and skipping the call.
    let slot_2_outcome = IterationOutcome::Crash(CrashType::PromptTooLong);
    assert!(matches!(
        slot_2_outcome,
        IterationOutcome::Crash(CrashType::PromptTooLong)
    ));
    // <-- intentionally NO call to overflow::handle_prompt_too_long here -->

    // Discriminator: the positive-direction keying assertion would fail.
    assert!(
        !ctx.model_overrides.contains_key(task_ids[2]),
        "without dispatch, slot 2's task_id MUST NOT appear in model_overrides",
    );
    assert!(
        !ctx.overflow_recovered.contains(task_ids[2]),
        "without dispatch, slot 2's task_id MUST NOT appear in overflow_recovered",
    );
    assert!(
        !ctx.effort_overrides.contains_key(task_ids[2]),
        "without dispatch, slot 2's task_id MUST NOT appear in effort_overrides",
    );
    assert!(
        !ctx.overflow_original_model.contains_key(task_ids[2]),
        "without dispatch, slot 2's task_id MUST NOT appear in overflow_original_model",
    );

    // No JSONL file should have been created either.
    let jsonl = tmp.path().join("overflow-events.jsonl");
    assert!(
        !jsonl.exists(),
        "without dispatch, no JSONL event file should exist",
    );

    // The positive assertion from `slot_2_recovery_keying_excludes_sibling_slot_task_ids`
    // is `assert!(ctx.model_overrides.contains_key(slot_2_id))`. We confirm
    // here that the negation holds in the no-dispatch scenario — meaning the
    // positive assertion would FAIL. That's exactly the discriminator AC #6
    // calls for.
    let positive_would_pass = ctx.model_overrides.contains_key(task_ids[2]);
    assert!(
        !positive_would_pass,
        "discriminator: positive keying assertion must FAIL when dispatch is absent",
    );
}
