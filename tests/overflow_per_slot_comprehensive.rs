//! Comprehensive multi-slot stress tests for per-slot overflow isolation (TEST-005).
//!
//! Extends the baseline coverage in `tests/overflow_per_slot.rs` with:
//! - 8-slot waves with 1, 2, and 4 simultaneous PromptTooLong outcomes
//! - JSONL deserialization tolerating missing `slot_index` (additive serde)
//! - Dump rotation across wave iterations for the same task_id
//! - ctx.overflow_recovered persistence across wave → sequential transitions

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, OPUS_MODEL_1M, SONNET_MODEL};
use task_mgr::loop_engine::overflow::{
    self, OverflowEvent, RecoveryAction, sanitize_id_for_filename,
};
use task_mgr::loop_engine::prompt::PromptResult;

// ---------- helpers ----------------------------------------------------------

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

fn reset_to_in_progress(conn: &Connection, task_id: &str) {
    conn.execute(
        "UPDATE tasks SET status = 'in_progress', started_at = '2026-05-07T00:00:00Z' WHERE id = ?1",
        [task_id],
    )
    .expect("reset task to in_progress");
}

/// Simulates the wave dispatcher: for each crashing slot, invoke
/// `handle_prompt_too_long` with that slot's task_id. Returns `(slot, action)`
/// pairs for the crashing slots only.
#[allow(clippy::too_many_arguments)]
fn dispatch_wave_overflows(
    ctx: &mut IterationContext,
    conn: &Connection,
    all_task_ids: &[&str],
    crashing_slots: &[usize],
    effort: Option<&str>,
    model: Option<&str>,
    iteration: u32,
    tmp_dir: &std::path::Path,
) -> Vec<(usize, RecoveryAction)> {
    let mut results = Vec::new();
    for &slot in crashing_slots {
        let task_id = all_task_ids[slot];
        let pr = make_prompt_result(task_id);
        let action = overflow::handle_prompt_too_long(
            ctx,
            conn,
            task_id,
            effort,
            model,
            &pr,
            iteration,
            Some("run-wave"),
            tmp_dir,
            Some(slot),
        );
        results.push((slot, action));
    }
    results
}

fn dump_count_for_task(dumps_dir: &std::path::Path, task_id: &str) -> usize {
    let sanitized = sanitize_id_for_filename(task_id);
    let prefix = format!("{sanitized}-iter");
    std::fs::read_dir(dumps_dir)
        .expect("overflow-dumps dir must exist")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let n = name.to_string_lossy();
            n.starts_with(&prefix) && n.ends_with(".txt")
        })
        .count()
}

// ---------- AC 1: 8-slot wave, 1 simultaneous PromptTooLong ------------------

/// 8-slot wave: only slot 3 hits PromptTooLong. Recovery is isolated to slot 3;
/// all other slots' task_ids are absent from every per-task recovery map.
#[test]
fn eight_slot_wave_one_overflow_isolated_to_slot_3() {
    let tmp = TempDir::new().expect("tempdir");
    let task_ids: [&str; 8] = [
        "W8-1-SLOT0",
        "W8-1-SLOT1",
        "W8-1-SLOT2",
        "W8-1-SLOT3",
        "W8-1-SLOT4",
        "W8-1-SLOT5",
        "W8-1-SLOT6",
        "W8-1-SLOT7",
    ];
    let conn = make_conn_with_tasks(&task_ids);
    let mut ctx = IterationContext::new(10);

    let results = dispatch_wave_overflows(
        &mut ctx,
        &conn,
        &task_ids,
        &[3],
        Some("xhigh"),
        Some(SONNET_MODEL),
        1,
        tmp.path(),
    );

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, 3);
    assert!(
        matches!(results[0].1, RecoveryAction::DowngradeEffort { .. }),
        "rung 1 must fire for xhigh: {:?}",
        results[0].1
    );

    // Slot 3's task_id is in all per-task maps.
    assert!(ctx.overflow_recovered.contains("W8-1-SLOT3"));
    assert!(ctx.effort_overrides.contains_key("W8-1-SLOT3"));
    assert!(ctx.overflow_original_model.contains_key("W8-1-SLOT3"));
    assert_eq!(
        ctx.model_overrides.len(),
        0,
        "rung 1 must not touch model_overrides"
    );

    // Exactly one entry in each recovery map.
    assert_eq!(ctx.overflow_recovered.len(), 1);
    assert_eq!(ctx.effort_overrides.len(), 1);
    assert_eq!(ctx.overflow_original_model.len(), 1);

    // All non-crashing slots remain in_progress.
    for (i, &tid) in task_ids.iter().enumerate() {
        let expected = if i == 3 { "todo" } else { "in_progress" };
        assert_eq!(
            task_status(&conn, tid),
            expected,
            "slot {i} DB status mismatch"
        );
    }

    // Exactly one JSONL event with slot_index = 3.
    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].task_id, "W8-1-SLOT3");
    assert_eq!(events[0].slot_index, Some(3));
    assert!(matches!(
        events[0].recovery,
        RecoveryAction::DowngradeEffort { .. }
    ));
}

// ---------- AC 1: 8-slot wave, 2 simultaneous PromptTooLong ------------------

/// 8-slot wave: slots 1 and 5 crash simultaneously. Both recoveries are
/// independent; slots 0, 2-4, 6-7 are untouched.
#[test]
fn eight_slot_wave_two_overflows_isolated_to_slots_1_and_5() {
    let tmp = TempDir::new().expect("tempdir");
    let task_ids: [&str; 8] = [
        "W8-2-SLOT0",
        "W8-2-SLOT1",
        "W8-2-SLOT2",
        "W8-2-SLOT3",
        "W8-2-SLOT4",
        "W8-2-SLOT5",
        "W8-2-SLOT6",
        "W8-2-SLOT7",
    ];
    let conn = make_conn_with_tasks(&task_ids);
    let mut ctx = IterationContext::new(10);

    let results = dispatch_wave_overflows(
        &mut ctx,
        &conn,
        &task_ids,
        &[1, 5],
        Some("xhigh"),
        Some(SONNET_MODEL),
        1,
        tmp.path(),
    );

    assert_eq!(results.len(), 2);
    for (_, action) in &results {
        assert!(
            matches!(action, RecoveryAction::DowngradeEffort { .. }),
            "rung 1 expected: {action:?}"
        );
    }

    for &crashing in &["W8-2-SLOT1", "W8-2-SLOT5"] {
        assert!(
            ctx.overflow_recovered.contains(crashing),
            "{crashing} missing from overflow_recovered"
        );
        assert!(
            ctx.effort_overrides.contains_key(crashing),
            "{crashing} missing from effort_overrides"
        );
        assert!(
            ctx.overflow_original_model.contains_key(crashing),
            "{crashing} missing from overflow_original_model"
        );
    }

    assert_eq!(ctx.overflow_recovered.len(), 2);
    assert_eq!(ctx.effort_overrides.len(), 2);
    assert_eq!(ctx.overflow_original_model.len(), 2);
    assert_eq!(ctx.model_overrides.len(), 0);

    for (i, &tid) in task_ids.iter().enumerate() {
        let expected = if i == 1 || i == 5 {
            "todo"
        } else {
            "in_progress"
        };
        assert_eq!(
            task_status(&conn, tid),
            expected,
            "slot {i} status mismatch"
        );
    }

    let events = read_events(tmp.path());
    assert_eq!(events.len(), 2);
    let mut slot_indices: Vec<usize> = events
        .iter()
        .map(|e| e.slot_index.expect("wave event must have slot_index"))
        .collect();
    slot_indices.sort_unstable();
    assert_eq!(slot_indices, vec![1, 5]);
}

// ---------- AC 1: 8-slot wave, 4 simultaneous PromptTooLong ------------------

/// 8-slot wave: even slots (0, 2, 4, 6) crash simultaneously. Four independent
/// rung-1 recoveries; odd slots are untouched. No cross-contamination.
#[test]
fn eight_slot_wave_four_overflows_even_slots_independent_recovery() {
    let tmp = TempDir::new().expect("tempdir");
    let task_ids: [&str; 8] = [
        "W8-4-SLOT0",
        "W8-4-SLOT1",
        "W8-4-SLOT2",
        "W8-4-SLOT3",
        "W8-4-SLOT4",
        "W8-4-SLOT5",
        "W8-4-SLOT6",
        "W8-4-SLOT7",
    ];
    let conn = make_conn_with_tasks(&task_ids);
    let mut ctx = IterationContext::new(10);

    let crashing = [0usize, 2, 4, 6];
    let results = dispatch_wave_overflows(
        &mut ctx,
        &conn,
        &task_ids,
        &crashing,
        Some("xhigh"),
        Some(SONNET_MODEL),
        1,
        tmp.path(),
    );

    assert_eq!(results.len(), 4);
    for (_, action) in &results {
        assert!(
            matches!(action, RecoveryAction::DowngradeEffort { new_effort } if new_effort == "high"),
            "each crashing slot must hit rung 1 independently: {action:?}"
        );
    }

    assert_eq!(ctx.overflow_recovered.len(), 4);
    assert_eq!(ctx.effort_overrides.len(), 4);
    assert_eq!(ctx.overflow_original_model.len(), 4);
    assert_eq!(ctx.model_overrides.len(), 0);

    for (i, tid) in task_ids.iter().enumerate().take(8) {
        let tid = *tid;
        if crashing.contains(&i) {
            assert!(
                ctx.overflow_recovered.contains(tid),
                "slot {i} missing from recovered"
            );
            assert_eq!(
                ctx.effort_overrides.get(tid).copied(),
                Some("high"),
                "slot {i} effort override must be 'high'"
            );
        } else {
            assert!(
                !ctx.overflow_recovered.contains(tid),
                "odd slot {i} must not be in recovered"
            );
            assert!(
                !ctx.effort_overrides.contains_key(tid),
                "odd slot {i} must not have effort override"
            );
        }
    }

    for (i, &tid) in task_ids.iter().enumerate() {
        let expected = if crashing.contains(&i) {
            "todo"
        } else {
            "in_progress"
        };
        assert_eq!(task_status(&conn, tid), expected, "slot {i} DB status");
    }

    let events = read_events(tmp.path());
    assert_eq!(events.len(), 4);

    let mut slot_indices: Vec<usize> = events
        .iter()
        .map(|e| e.slot_index.expect("wave event must have slot_index"))
        .collect();
    slot_indices.sort_unstable();
    assert_eq!(slot_indices, vec![0, 2, 4, 6]);

    let mut event_task_ids: Vec<&str> = events.iter().map(|e| e.task_id.as_str()).collect();
    event_task_ids.sort_unstable();
    let mut expected_ids: Vec<&str> = crashing.iter().map(|&i| task_ids[i]).collect();
    expected_ids.sort_unstable();
    assert_eq!(event_task_ids, expected_ids);
}

// ---------- AC 2: JSONL deserialization tolerates missing slot_index ----------

/// A JSONL line without a `slot_index` field deserializes as `slot_index: None`.
/// Validates the additive serde change: old sequential events from before the
/// field existed still parse cleanly.
#[test]
fn jsonl_deserialization_tolerates_missing_slot_index() {
    let json = serde_json::json!({
        "ts": "2026-05-07T10:00:00+00:00",
        "task_id": "OLD-SEQ-TASK",
        "run_id": "run-old",
        "iteration": 3,
        "model": SONNET_MODEL,
        "effort": "high",
        "prompt_bytes": 5000,
        "sections": [["task", 100], ["learnings", 200]],
        "dropped_sections": [],
        "recovery": {"action": "downgrade_effort", "new_effort": "high"},
        "dump_path": "/tmp/old-dump.txt"
    });

    let event: OverflowEvent =
        serde_json::from_str(&json.to_string()).expect("deserialize old sequential JSONL");
    assert_eq!(
        event.slot_index, None,
        "missing slot_index field must deserialize as None"
    );
    assert_eq!(event.task_id, "OLD-SEQ-TASK");
    assert_eq!(event.iteration, 3);
    assert!(matches!(
        event.recovery,
        RecoveryAction::DowngradeEffort { .. }
    ));
}

/// A JSONL line with `"slot_index": null` deserializes as `slot_index: None`.
#[test]
fn jsonl_deserialization_tolerates_explicit_null_slot_index() {
    let json = serde_json::json!({
        "ts": "2026-05-07T10:00:00+00:00",
        "task_id": "NULL-SLOT-TASK",
        "iteration": 1,
        "slot_index": null,
        "model": SONNET_MODEL,
        "effort": "xhigh",
        "prompt_bytes": 3000,
        "sections": [["task", 50]],
        "dropped_sections": [],
        "recovery": {"action": "blocked"},
        "dump_path": "/tmp/null-slot.txt"
    });

    let event: OverflowEvent =
        serde_json::from_str(&json.to_string()).expect("deserialize null slot_index");
    assert_eq!(
        event.slot_index, None,
        "explicit null slot_index must deserialize as None"
    );
}

/// A JSONL line with a numeric `slot_index` round-trips with the value intact.
#[test]
fn jsonl_deserialization_round_trips_numeric_slot_index() {
    let json = serde_json::json!({
        "ts": "2026-05-07T10:00:00+00:00",
        "task_id": "SLOT-TASK-007",
        "iteration": 2,
        "slot_index": 7,
        "model": OPUS_MODEL,
        "effort": "high",
        "prompt_bytes": 99000,
        "sections": [["task", 1000], ["learnings", 2000], ["base_prompt", 96000]],
        "dropped_sections": ["progress"],
        "recovery": {"action": "to_1m_model", "new_model": OPUS_MODEL_1M},
        "dump_path": "/tmp/slot7.txt"
    });

    let event: OverflowEvent =
        serde_json::from_str(&json.to_string()).expect("deserialize slot JSONL");
    assert_eq!(event.slot_index, Some(7));
    assert_eq!(event.task_id, "SLOT-TASK-007");
    assert!(matches!(event.recovery, RecoveryAction::To1mModel { .. }));

    // Re-serialize: slot_index field must be present and numeric.
    let v = serde_json::to_value(&event).unwrap();
    assert_eq!(
        v.get("slot_index"),
        Some(&serde_json::json!(7)),
        "re-serialized event must carry slot_index = 7"
    );
}

/// Validates that sequential events produced by `handle_prompt_too_long` with
/// `slot_index = None` omit the field from the serialized JSON (or render null),
/// and that the typed field round-trips as `None`.
#[test]
fn sequential_jsonl_event_has_no_slot_index_field() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "SEQ-NO-SLOT-TASK";
    let conn = make_conn_with_tasks(&[task_id]);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

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

    let raw_events = read_event_values(tmp.path());
    assert_eq!(raw_events.len(), 1);
    match raw_events[0].get("slot_index") {
        None | Some(serde_json::Value::Null) => {}
        Some(other) => panic!(
            "sequential event must not have a slot_index field; got {other:?}\nfull line: {}",
            raw_events[0]
        ),
    }

    let typed = read_events(tmp.path());
    assert_eq!(typed[0].slot_index, None);
}

// ---------- AC 3: dump rotation per sanitized_task_id -----------------------

/// A task that overflows 4 times (across 4 wave iterations, same slot) keeps
/// only the 3 newest dump files. The JSONL log retains all 4 events.
#[test]
fn dump_rotation_keeps_newest_3_across_wave_iterations() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "ROTAT-TASK-001";
    let conn = make_conn_with_tasks(&[task_id]);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // 4 overflow calls for the same task_id, ascending iteration numbers.
    // Rungs: 1 (xhigh→high), 2 (Sonnet→Opus), 3 (Opus→1M), 4 (Blocked).
    let calls: &[(Option<&str>, Option<&str>)] = &[
        (Some("xhigh"), Some(SONNET_MODEL)),
        (Some("high"), Some(SONNET_MODEL)),
        (Some("high"), Some(OPUS_MODEL)),
        (Some("high"), Some(OPUS_MODEL_1M)),
    ];
    for (iter, &(effort, model)) in calls.iter().enumerate() {
        reset_to_in_progress(&conn, task_id);
        let _ = overflow::handle_prompt_too_long(
            &mut ctx,
            &conn,
            task_id,
            effort,
            model,
            &pr,
            (iter + 1) as u32,
            Some("run-wave"),
            tmp.path(),
            Some(2),
        );
        // Clear model_overrides so each call re-selects the intended rung.
        ctx.model_overrides.remove(task_id);
        ctx.effort_overrides.remove(task_id);
    }

    // Dump rotation: keep = 3; oldest dump is deleted.
    let dumps_dir = tmp.path().join("overflow-dumps");
    assert_eq!(
        dump_count_for_task(&dumps_dir, task_id),
        3,
        "rotation must keep exactly 3 dumps for {task_id}"
    );

    // JSONL event log is not rotated — all 4 events persist.
    let events = read_events(tmp.path());
    assert_eq!(
        events.len(),
        4,
        "JSONL must retain all 4 events (rotation is dump-only)"
    );
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev.task_id, task_id);
        assert_eq!(ev.slot_index, Some(2), "all events must carry slot_index=2");
        assert_eq!(ev.iteration, (i + 1) as u32);
    }
}

/// Two different task_ids in the same wave each have their own rotation namespace.
/// Slot 0's 4 overflows rotate to 3; slot 1's 2 overflows are all retained.
#[test]
fn dump_rotation_namespaces_are_per_task_id() {
    let tmp = TempDir::new().expect("tempdir");
    let task_a = "ROTAT-TASK-A";
    let task_b = "ROTAT-TASK-B";
    let conn = make_conn_with_tasks(&[task_a, task_b]);
    let mut ctx = IterationContext::new(10);

    let pr_a = make_prompt_result(task_a);
    let pr_b = make_prompt_result(task_b);

    // 4 overflows for task_a (all at rung 1 by resetting effort_overrides).
    for iter in 1u32..=4 {
        reset_to_in_progress(&conn, task_a);
        let _ = overflow::handle_prompt_too_long(
            &mut ctx,
            &conn,
            task_a,
            Some("xhigh"),
            Some(SONNET_MODEL),
            &pr_a,
            iter,
            Some("run-wave"),
            tmp.path(),
            Some(0),
        );
        ctx.effort_overrides.remove(task_a);
        ctx.overflow_recovered.remove(task_a);
        ctx.overflow_original_model.remove(task_a);
    }

    // 2 overflows for task_b.
    for iter in 1u32..=2 {
        reset_to_in_progress(&conn, task_b);
        let _ = overflow::handle_prompt_too_long(
            &mut ctx,
            &conn,
            task_b,
            Some("xhigh"),
            Some(SONNET_MODEL),
            &pr_b,
            iter,
            Some("run-wave"),
            tmp.path(),
            Some(1),
        );
        ctx.effort_overrides.remove(task_b);
        ctx.overflow_recovered.remove(task_b);
        ctx.overflow_original_model.remove(task_b);
    }

    let dumps_dir = tmp.path().join("overflow-dumps");

    // task_a: 4 written, rotation keeps 3.
    assert_eq!(
        dump_count_for_task(&dumps_dir, task_a),
        3,
        "task_a must have 3 dumps after rotation (4 written, keep=3)"
    );

    // task_b: 2 written, both kept (below rotation threshold).
    assert_eq!(
        dump_count_for_task(&dumps_dir, task_b),
        2,
        "task_b must retain both dumps (2 < 3 rotation threshold)"
    );
}

// ---------- AC 4: overflow_recovered persists across wave→sequential ---------

/// A task that overflows first in wave mode retains its `overflow_recovered`
/// membership when it overflows again in sequential mode. The
/// `overflow_original_model` first-insert-wins contract is respected: the
/// sequential re-entry does NOT overwrite the original model.
#[test]
fn overflow_recovered_persists_from_wave_through_sequential_transition() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "TRANS-TASK-001";
    let conn = make_conn_with_tasks(&[task_id]);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Wave overflow: slot 4, Sonnet, xhigh → rung 1 (DowngradeEffort).
    let wave_action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        Some("run-wave"),
        tmp.path(),
        Some(4),
    );
    assert!(
        matches!(wave_action, RecoveryAction::DowngradeEffort { .. }),
        "rung 1 expected: {wave_action:?}"
    );
    assert!(
        ctx.overflow_recovered.contains(task_id),
        "wave overflow must add task_id to recovered"
    );
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
        "wave overflow must capture Sonnet as original model"
    );

    reset_to_in_progress(&conn, task_id);

    // Sequential overflow: same task_id, now at high effort → rung 2 (EscalateModel).
    let seq_action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(SONNET_MODEL),
        &pr,
        2,
        Some("run-seq"),
        tmp.path(),
        None,
    );
    assert!(
        matches!(seq_action, RecoveryAction::EscalateModel { .. }),
        "rung 2 expected: {seq_action:?}"
    );

    // overflow_recovered persists from wave.
    assert!(
        ctx.overflow_recovered.contains(task_id),
        "overflow_recovered must persist task_id across wave→sequential transition"
    );

    // overflow_original_model: first-insert-wins (still Sonnet, not overwritten).
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
        "overflow_original_model must not be overwritten on second overflow"
    );

    // Rung 2 escalates model to Opus.
    assert_eq!(
        ctx.model_overrides.get(task_id).map(String::as_str),
        Some(OPUS_MODEL),
        "rung 2 must escalate to Opus"
    );

    // Two JSONL events: wave event has slot_index, sequential does not.
    let events = read_events(tmp.path());
    assert_eq!(events.len(), 2);
    let wave_ev = events
        .iter()
        .find(|e| e.slot_index == Some(4))
        .expect("wave event with slot_index=4 must exist");
    let seq_ev = events
        .iter()
        .find(|e| e.slot_index.is_none())
        .expect("sequential event with slot_index=None must exist");
    assert_eq!(wave_ev.iteration, 1);
    assert_eq!(seq_ev.iteration, 2);
    assert!(matches!(
        wave_ev.recovery,
        RecoveryAction::DowngradeEffort { .. }
    ));
    assert!(matches!(
        seq_ev.recovery,
        RecoveryAction::EscalateModel { .. }
    ));
}

/// Sequential overflow followed by wave overflow for the same task. The reverse
/// transition: seq → wave. Set membership and first-insert-wins are both upheld.
#[test]
fn overflow_recovered_persists_from_sequential_through_wave_transition() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "TRANS-TASK-002";
    let conn = make_conn_with_tasks(&[task_id]);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Sequential overflow: Haiku, xhigh → rung 1.
    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(HAIKU_MODEL),
        &pr,
        1,
        Some("run-seq"),
        tmp.path(),
        None,
    );
    assert!(ctx.overflow_recovered.contains(task_id));
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(HAIKU_MODEL),
        "sequential overflow must capture Haiku as original model"
    );

    reset_to_in_progress(&conn, task_id);

    // Wave overflow: Haiku at high → rung 2 (EscalateModel: Haiku → Sonnet).
    let wave_action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(HAIKU_MODEL),
        &pr,
        2,
        Some("run-wave"),
        tmp.path(),
        Some(0),
    );
    assert!(
        matches!(wave_action, RecoveryAction::EscalateModel { ref new_model } if new_model == SONNET_MODEL),
        "Haiku at high → escalate to Sonnet: {wave_action:?}"
    );

    assert!(
        ctx.overflow_recovered.contains(task_id),
        "overflow_recovered must persist through seq→wave transition"
    );
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(HAIKU_MODEL),
        "original model must not be overwritten on wave overflow"
    );
    assert_eq!(
        ctx.model_overrides.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
        "rung 2 must escalate Haiku → Sonnet"
    );
}

// ---------- AC 4 (banner side): recovered set is long-lived across waves -----

/// A task recovered in wave 1 remains in `overflow_recovered` during wave 2.
/// The ctx is long-lived across waves; the banner fires on any iteration where
/// the task_id is in the set, regardless of which wave first populated it.
#[test]
fn overflow_recovered_is_persistent_across_consecutive_waves() {
    let tmp = TempDir::new().expect("tempdir");
    let wave1_task = "PERSIST-W1-TASK";
    let wave2_task = "PERSIST-W2-TASK";
    let conn = make_conn_with_tasks(&[wave1_task, wave2_task]);
    let mut ctx = IterationContext::new(10);

    // Wave 1: wave1_task overflows.
    let pr1 = make_prompt_result(wave1_task);
    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        wave1_task,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr1,
        1,
        Some("run-wave-1"),
        tmp.path(),
        Some(1),
    );
    assert!(ctx.overflow_recovered.contains(wave1_task));

    // Wave 2: wave2_task overflows (different task, different slot).
    reset_to_in_progress(&conn, wave2_task);
    let pr2 = make_prompt_result(wave2_task);
    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        wave2_task,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr2,
        1,
        Some("run-wave-2"),
        tmp.path(),
        Some(0),
    );

    // Both task_ids in overflow_recovered (long-lived ctx, wave-global set).
    assert!(
        ctx.overflow_recovered.contains(wave1_task),
        "wave-1 recovery must persist into wave-2 ctx"
    );
    assert!(
        ctx.overflow_recovered.contains(wave2_task),
        "wave-2 recovery must be added to the same set"
    );
    assert_eq!(ctx.overflow_recovered.len(), 2);

    // No cross-contamination: each task_id has its own effort_overrides entry.
    assert!(ctx.effort_overrides.contains_key(wave1_task));
    assert!(ctx.effort_overrides.contains_key(wave2_task));
    assert_eq!(ctx.effort_overrides.len(), 2);

    // Two JSONL events with independent slot indices.
    let events = read_events(tmp.path());
    assert_eq!(events.len(), 2);
    let mut task_ids: Vec<&str> = events.iter().map(|e| e.task_id.as_str()).collect();
    task_ids.sort_unstable();
    let mut expected = vec![wave1_task, wave2_task];
    expected.sort_unstable();
    assert_eq!(task_ids, expected);
}
