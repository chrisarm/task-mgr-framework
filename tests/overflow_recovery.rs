//! Integration tests for the four-rung prompt-overflow recovery ladder.
//!
//! These tests drive `overflow::handle_prompt_too_long` directly with synthetic
//! `IterationContext` / `PromptResult` inputs — there is no Claude subprocess
//! involved, just the recovery state machine and its diagnostics side effects.
//!
//! The full ladder under test (per PRD section 4):
//! 1. `downgrade_effort` — `xhigh` → `high` (effort floor; preserves model)
//! 2. `escalate_below_opus` — Sonnet → Opus (at floor effort; preserves effort)
//! 3. `to_1m_model` — Opus → Opus[1M] (already at Opus; expands context window)
//! 4. `Blocked` — Opus[1M] at `high` effort has no further escape hatch
//!
//! AC #11: every test uses the production types
//! (`config::CrashType::PromptTooLong`, `engine::IterationContext`,
//! `overflow::OverflowEvent`). The synthetic outcome stays in the test as
//! a documentation marker that this branch is what the production code
//! observes before delegating to `handle_prompt_too_long`.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::loop_engine::config::{CrashType, IterationOutcome};
use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::model::{OPUS_MODEL, OPUS_MODEL_1M, SONNET_MODEL};
use task_mgr::loop_engine::overflow::{
    self, OverflowEvent, RecoveryAction, sanitize_id_for_filename,
};
use task_mgr::loop_engine::prompt::PromptResult;

// ---------- Test fixtures ---------------------------------------------------

/// Build an in-memory sqlite Connection with the minimal `tasks` schema and a
/// single `in_progress` task row so that `handle_prompt_too_long`'s status
/// UPDATE actually flips a row (verifies the SQL contract end-to-end).
fn make_conn_with_task(task_id: &str) -> Connection {
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
    conn.execute(
        "INSERT INTO tasks (id, title, status, started_at) VALUES (?1, 'fixture', 'in_progress', '2026-05-04T00:00:00Z')",
        [task_id],
    )
    .expect("seed task row");
    conn
}

/// Read the `status` column for a task (panics if the row is missing).
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
        dropped_sections: vec!["progress".to_string()],
        task_difficulty: Some("medium".to_string()),
        cluster_effort: None,
        section_sizes: vec![("task", 12), ("learnings", 17), ("base_prompt", 19)],
    }
}

/// Read every line of `<base_dir>/overflow-events.jsonl` parsed back into
/// `OverflowEvent`. Panics on parse error so a malformed line is fatal.
fn read_events(base_dir: &Path) -> Vec<OverflowEvent> {
    let path = base_dir.join("overflow-events.jsonl");
    let raw = std::fs::read_to_string(&path).expect("jsonl exists");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<OverflowEvent>(l).expect("parse jsonl line"))
        .collect()
}

fn list_dump_files(base_dir: &Path, sanitized_task: &str) -> Vec<std::path::PathBuf> {
    let dumps_dir = base_dir.join("overflow-dumps");
    let prefix = format!("{sanitized_task}-iter");
    match std::fs::read_dir(&dumps_dir) {
        Err(_) => Vec::new(),
        Ok(rd) => rd
            .filter_map(Result::ok)
            .filter(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.starts_with(&prefix) && s.ends_with(".txt")
            })
            .map(|e| e.path())
            .collect(),
    }
}

/// Discriminant on the `RecoveryAction` enum, used for compact assertions.
fn rung_label(a: &RecoveryAction) -> &'static str {
    match a {
        RecoveryAction::DowngradeEffort { .. } => "downgrade_effort",
        RecoveryAction::EscalateModel { .. } => "escalate_model",
        RecoveryAction::To1mModel { .. } => "to_1m_model",
        RecoveryAction::Blocked => "blocked",
    }
}

// ---------- AC #1: ladder walk ---------------------------------------------

/// Sonnet+xhigh starting state: four synthetic `PromptTooLong` events should
/// produce exactly the rung sequence
/// [downgrade_effort → escalate_model → to_1m_model → blocked].
///
/// Iteration-by-iteration the next-iteration model/effort are derived by the
/// engine from `ctx.effort_overrides` / `ctx.model_overrides`; the test
/// mirrors that derivation here so we can feed the helper realistic
/// `effective_*` values for each subsequent call.
#[test]
fn ladder_walk_sonnet_xhigh_to_blocked() {
    // The synthetic outcome is the trigger condition the production
    // `PromptTooLong` arm checks; we keep it in the test to anchor AC #11.
    let outcome = IterationOutcome::Crash(CrashType::PromptTooLong);
    assert!(matches!(
        outcome,
        IterationOutcome::Crash(CrashType::PromptTooLong)
    ));

    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    let task_id = "FOO-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Iter 1: Sonnet + xhigh → downgrade_effort to high.
    let a1 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        Some("run-test"),
        base,
        None,
    );
    assert_eq!(rung_label(&a1), "downgrade_effort");
    assert!(
        matches!(a1, RecoveryAction::DowngradeEffort { ref new_effort } if new_effort == "high")
    );
    assert_eq!(
        task_status(&conn, task_id),
        "todo",
        "rung 1 must reset to todo"
    );

    // Re-claim (production: task selection picks the task back up as in_progress).
    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
        [task_id],
    )
    .unwrap();

    // Iter 2: Sonnet + high → escalate_below_opus to Opus.
    let a2 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(SONNET_MODEL),
        &pr,
        2,
        Some("run-test"),
        base,
        None,
    );
    assert_eq!(rung_label(&a2), "escalate_model");
    assert!(
        matches!(a2, RecoveryAction::EscalateModel { ref new_model } if new_model == OPUS_MODEL)
    );
    assert_eq!(
        task_status(&conn, task_id),
        "todo",
        "rung 2 must reset to todo"
    );

    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
        [task_id],
    )
    .unwrap();

    // Iter 3: Opus + high → to_1m_model.
    let a3 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL),
        &pr,
        3,
        Some("run-test"),
        base,
        None,
    );
    assert_eq!(rung_label(&a3), "to_1m_model");
    assert!(
        matches!(a3, RecoveryAction::To1mModel { ref new_model } if new_model == OPUS_MODEL_1M)
    );
    assert_eq!(
        task_status(&conn, task_id),
        "todo",
        "rung 3 must reset to todo"
    );

    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
        [task_id],
    )
    .unwrap();

    // Iter 4: Opus[1M] + high → blocked (no further escape).
    let a4 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL_1M),
        &pr,
        4,
        Some("run-test"),
        base,
        None,
    );
    assert_eq!(rung_label(&a4), "blocked");
    assert!(matches!(a4, RecoveryAction::Blocked));
    assert_eq!(
        task_status(&conn, task_id),
        "blocked",
        "rung 4 must mark blocked"
    );
}

// ---------- AC #2: explicit-Opus skip ---------------------------------------

/// A task whose model is already Opus on the very first overflow at the
/// `high` effort floor must skip rung 2 (escalate_below_opus returns None
/// at the Opus tier) and land on rung 3 (`to_1m_model`).
#[test]
fn explicit_opus_at_floor_skips_to_1m_rung() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "BAR-FEAT-099";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL),
        &pr,
        1,
        None,
        tmp.path(),
        None,
    );
    assert_eq!(
        rung_label(&action),
        "to_1m_model",
        "Opus at high effort must skip directly to 1M variant",
    );
    assert!(
        matches!(action, RecoveryAction::To1mModel { ref new_model } if new_model == OPUS_MODEL_1M)
    );
    assert_eq!(task_status(&conn, task_id), "todo");
}

// ---------- AC #3 + AC #10: filename sanitization + tempdir isolation ------

/// Path-traversal-style task IDs are sanitized to a safe filename component
/// (no `/`, no `..`) — the dump file lands inside `.task-mgr/overflow-dumps/`
/// under the sanitized basename, never escaping the tempdir.
#[test]
fn filename_sanitization_neutralizes_traversal() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "FOO/BAR..baz";
    let conn = make_conn_with_task(task_id);
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
        None,
        tmp.path(),
        None,
    );

    let sanitized = sanitize_id_for_filename(task_id);
    assert_eq!(sanitized, "FOO-BAR--baz", "sanitization mismatch");

    let dump_files = list_dump_files(tmp.path(), &sanitized);
    assert_eq!(
        dump_files.len(),
        1,
        "expected exactly one dump for sanitized task id, got {dump_files:?}",
    );
    let dump_path = &dump_files[0];
    let name = dump_path.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("FOO-BAR--baz-iter1-"),
        "dump filename `{name}` does not start with sanitized prefix",
    );
    assert!(
        name.ends_with(".txt"),
        "dump filename `{name}` must end with .txt"
    );
    assert!(
        !name.contains('/'),
        "dump filename `{name}` must not contain `/`"
    );
    assert!(
        !name.contains(".."),
        "dump filename `{name}` must not contain `..`"
    );

    // Tempdir isolation: dump path must be a descendant of the tempdir.
    let canon_dump = dump_path.canonicalize().expect("canonicalize dump");
    let canon_root = tmp.path().canonicalize().expect("canonicalize root");
    assert!(
        canon_dump.starts_with(&canon_root),
        "dump escaped tempdir: {canon_dump:?} not under {canon_root:?}",
    );
}

// ---------- AC #4: dump content + auto-load NOTE ---------------------------

/// Every dump contains: total bytes, per-section breakdown, the auto-load
/// NOTE about CLAUDE.md/skills, and the assembled prompt body verbatim.
#[test]
fn dump_content_includes_breakdown_note_and_verbatim_prompt() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "QUX-FEAT-007";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let prompt_body = pr.prompt.clone();
    let total = prompt_body.len();

    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        None,
        tmp.path(),
        None,
    );

    let sanitized = sanitize_id_for_filename(task_id);
    let dump_files = list_dump_files(tmp.path(), &sanitized);
    assert_eq!(dump_files.len(), 1);
    let body = std::fs::read_to_string(&dump_files[0]).expect("read dump");

    // Header — total bytes line.
    assert!(
        body.contains(&format!("Total assembled bytes: {total}")),
        "dump missing total-bytes line; got:\n{body}",
    );
    // Header — section breakdown lines (one per declared section).
    for (name, size) in [("task", 12), ("learnings", 17), ("base_prompt", 19)] {
        assert!(
            body.contains(&format!("  {name}: {size} bytes")),
            "dump missing breakdown line for `{name}`; got:\n{body}",
        );
    }
    // Header — auto-load NOTE.
    assert!(
        body.contains("NOTE: Claude Code auto-loads"),
        "dump missing auto-load NOTE; got:\n{body}",
    );
    // Body — assembled prompt is present verbatim.
    assert!(
        body.ends_with(&prompt_body) || body.contains(&prompt_body),
        "dump missing assembled prompt verbatim; got:\n{body}",
    );
}

// ---------- AC #5: JSONL append per iteration -------------------------------

/// Each call to `handle_prompt_too_long` appends exactly one parseable JSONL
/// line whose `recovery.action` matches the rung that fired.
#[test]
fn jsonl_appends_one_line_per_iteration_with_matching_action() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "ZAP-FEAT-002";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Walk three rungs; pre-blocked so the loop ends in 3 lines.
    let scenarios: [(Option<&str>, Option<&str>, &str); 3] = [
        (Some("xhigh"), Some(SONNET_MODEL), "downgrade_effort"),
        (Some("high"), Some(SONNET_MODEL), "escalate_model"),
        (Some("high"), Some(OPUS_MODEL), "to_1m_model"),
    ];
    for (i, (effort, model, expected)) in scenarios.iter().enumerate() {
        // Re-claim between iterations (production: task selection re-picks the row).
        conn.execute(
            "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
            [task_id],
        )
        .unwrap();
        let action = overflow::handle_prompt_too_long(
            &mut ctx,
            &conn,
            task_id,
            *effort,
            *model,
            &pr,
            (i as u32) + 1,
            None,
            tmp.path(),
            None,
        );
        assert_eq!(rung_label(&action), *expected, "scenario {i} rung mismatch");
    }

    let events = read_events(tmp.path());
    assert_eq!(events.len(), 3, "expected one JSONL line per iteration");
    let actions: Vec<&'static str> = events.iter().map(|e| rung_label(&e.recovery)).collect();
    assert_eq!(
        actions,
        vec!["downgrade_effort", "escalate_model", "to_1m_model"]
    );

    // Each line carries the iteration index it was emitted from.
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.iteration, (i as u32) + 1);
        assert_eq!(e.task_id, task_id);
    }
}

// ---------- AC #6: rotation keeps newest 3 ----------------------------------

/// After ≥4 overflows on one task, only the 3 newest dump files (by mtime)
/// remain for that task.
#[test]
fn rotation_keeps_only_newest_three_dumps() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "ROT-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Five iterations, each writing one dump; rotation runs at the end of
    // every call so after the 5th invocation the dir holds 3 files.
    for i in 1..=5u32 {
        // Reset status to in_progress for each iteration so the helper's
        // UPDATE has a row to flip (unrelated to rotation, but keeps the
        // fixture self-consistent).
        conn.execute(
            "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
            [task_id],
        )
        .unwrap();
        let _ = overflow::handle_prompt_too_long(
            &mut ctx,
            &conn,
            task_id,
            Some("xhigh"),
            Some(SONNET_MODEL),
            &pr,
            i,
            None,
            tmp.path(),
            None,
        );
        // Sleep enough for distinguishable mtimes on coarse filesystems —
        // resolution is at least 1s on common Linux setups (ext4 with
        // default options).
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }

    let sanitized = sanitize_id_for_filename(task_id);
    let dump_files = list_dump_files(tmp.path(), &sanitized);
    assert_eq!(
        dump_files.len(),
        3,
        "expected exactly 3 dumps after rotation, got {dump_files:?}",
    );

    // The 3 retained must be the newest by mtime.
    let mut with_mtime: Vec<(std::time::SystemTime, std::path::PathBuf)> = dump_files
        .iter()
        .map(|p| {
            (
                p.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                p.clone(),
            )
        })
        .collect();
    with_mtime.sort_by_key(|b| std::cmp::Reverse(b.0));
    // The newest dump must correspond to iteration 5 (filename embeds iter).
    let newest = with_mtime[0]
        .1
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        newest.starts_with(&format!("{sanitized}-iter5-")),
        "newest retained dump should be iter5, got {newest}",
    );
}

// ---------- AC #7: overflow_recovered marker -------------------------------

/// After iteration 1, the task ID is in `ctx.overflow_recovered`.
#[test]
fn overflow_recovered_set_populated_after_first_overflow() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "OR-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    assert!(
        !ctx.overflow_recovered.contains(task_id),
        "precondition: set must start empty",
    );

    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        None,
        tmp.path(),
        None,
    );

    assert!(
        ctx.overflow_recovered.contains(task_id),
        "ctx.overflow_recovered must contain task_id after first overflow",
    );
}

// ---------- AC #8: original-model captured on first overflow only ----------

/// `overflow_original_model` snapshots the ORIGINAL model (Sonnet) on the
/// first overflow and is NOT overwritten on iterations 2/3/4 even though
/// the effective model has escalated.
#[test]
fn original_model_captured_on_first_overflow_only() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "OM-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Iter 1: original model = Sonnet.
    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        None,
        tmp.path(),
        None,
    );
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
    );

    // Iter 2/3/4: subsequent overflows arrive on escalated models, but the
    // original-model snapshot stays pinned at Sonnet.
    let later: [(Option<&str>, Option<&str>); 3] = [
        (Some("high"), Some(SONNET_MODEL)),
        (Some("high"), Some(OPUS_MODEL)),
        (Some("high"), Some(OPUS_MODEL_1M)),
    ];
    for (i, (effort, model)) in later.iter().enumerate() {
        // Reset so the SQL UPDATE has a row to flip.
        conn.execute(
            "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
            [task_id],
        )
        .unwrap();
        let _ = overflow::handle_prompt_too_long(
            &mut ctx,
            &conn,
            task_id,
            *effort,
            *model,
            &pr,
            (i as u32) + 2,
            None,
            tmp.path(),
            None,
        );
        assert_eq!(
            ctx.overflow_original_model.get(task_id).map(String::as_str),
            Some(SONNET_MODEL),
            "iter {} must NOT overwrite original_model snapshot",
            (i as u32) + 2,
        );
    }
}

// ---------- AC #9: blocked rung still writes dump + JSONL -------------------

/// Even on the terminal `Blocked` rung, the helper must produce both a dump
/// file and a JSONL line whose `recovery.action == "blocked"`. Diagnostics
/// must not silently disappear at the dead end.
#[test]
fn blocked_rung_writes_both_dump_and_jsonl() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "BLK-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL_1M),
        &pr,
        1,
        Some("run-blk"),
        tmp.path(),
        None,
    );
    assert!(matches!(action, RecoveryAction::Blocked));
    assert_eq!(task_status(&conn, task_id), "blocked");

    // Dump file: exactly one matching the sanitized prefix.
    let sanitized = sanitize_id_for_filename(task_id);
    let dump_files = list_dump_files(tmp.path(), &sanitized);
    assert_eq!(dump_files.len(), 1, "blocked rung must still write a dump");

    // JSONL: exactly one line; recovery.action == "blocked".
    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].recovery, RecoveryAction::Blocked));
    let raw = std::fs::read_to_string(tmp.path().join("overflow-events.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(
        v["recovery"]["action"],
        serde_json::Value::String("blocked".into())
    );
}

// ---------- Cross-cutting: tempdir isolation (AC #10) ----------------------

/// Sanity check: nothing the helper writes should leak outside the supplied
/// `base_dir`. We collect the set of paths that exist inside the tempdir
/// after a representative run and require they all live under it.
#[test]
fn no_pollution_outside_tempdir() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "ISO-FEAT-001";
    let conn = make_conn_with_task(task_id);
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
        None,
        tmp.path(),
        None,
    );

    let canon_root = tmp.path().canonicalize().expect("canonicalize root");

    // Walk only what we expect the helper to create — dumps dir + JSONL.
    let mut produced: HashSet<std::path::PathBuf> = HashSet::new();
    let dumps_dir = tmp.path().join("overflow-dumps");
    if dumps_dir.exists() {
        for entry in std::fs::read_dir(&dumps_dir).unwrap() {
            produced.insert(entry.unwrap().path().canonicalize().unwrap());
        }
    }
    let jsonl = tmp.path().join("overflow-events.jsonl");
    if jsonl.exists() {
        produced.insert(jsonl.canonicalize().unwrap());
    }
    assert!(
        !produced.is_empty(),
        "helper must produce at least one artifact"
    );
    for p in produced {
        assert!(
            p.starts_with(&canon_root),
            "produced path {p:?} escaped tempdir {canon_root:?}",
        );
    }
}

// ---------- AC TEST-002-1: override persistence across iterations -----------

/// After rung 2 (escalate_model) fires, `ctx.model_overrides[task_id]` must
/// still hold Opus even when subsequent iterations complete without any further
/// overflow — the override is sticky for the lifetime of the loop slot.
///
/// Production engines never clear `model_overrides` on success; this test
/// anchors that invariant by calling `handle_prompt_too_long` once, verifying
/// the override was set, then asserting it's still present without further
/// calls (simulating 3 more iterations that succeeded).
#[test]
fn override_persists_across_iterations() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "OVR-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Rung 2 fires: Sonnet at effort floor (high) → escalate to Opus.
    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(SONNET_MODEL),
        &pr,
        2,
        None,
        tmp.path(),
        None,
    );
    assert_eq!(rung_label(&action), "escalate_model");
    assert_eq!(
        ctx.model_overrides.get(task_id).map(String::as_str),
        Some(OPUS_MODEL),
        "rung 2 must insert Opus into model_overrides",
    );

    // Simulate iterations 3, 4, 5 completing without overflow — no further
    // calls to handle_prompt_too_long. The override must survive.
    for simulated_iter in 3..=5u32 {
        assert_eq!(
            ctx.model_overrides.get(task_id).map(String::as_str),
            Some(OPUS_MODEL),
            "model_overrides must still contain Opus on simulated iteration {simulated_iter}",
        );
    }
}

// ---------- AC TEST-002-2: first-overflow-only capture (3-rung trace) -------

/// Walking through Sonnet+xhigh → Sonnet+high → Opus+high, the
/// `overflow_original_model` entry must equal SONNET_MODEL after each rung
/// and MUST NOT be overwritten when the effective model changes to Opus.
///
/// Uses the `entry().or_insert_with()` invariant: only the first caller
/// wins; subsequent calls with a different `effective_model` are no-ops.
#[test]
fn original_model_captured_first_overflow_only() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "OMC-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Observation 1: Sonnet+xhigh → rung 1 (downgrade_effort).
    let a1 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        Some(SONNET_MODEL),
        &pr,
        1,
        None,
        tmp.path(),
        None,
    );
    assert_eq!(rung_label(&a1), "downgrade_effort");
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
        "observation 1: original model must be SONNET_MODEL",
    );

    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
        [task_id],
    )
    .unwrap();

    // Observation 2: Sonnet+high → rung 2 (escalate_model).
    let a2 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(SONNET_MODEL),
        &pr,
        2,
        None,
        tmp.path(),
        None,
    );
    assert_eq!(rung_label(&a2), "escalate_model");
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
        "observation 2: original model must STILL be SONNET_MODEL after rung 2",
    );

    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = ?1",
        [task_id],
    )
    .unwrap();

    // Observation 3: Opus+high → rung 3 (to_1m_model). Even though the
    // *current* effective_model is Opus, the snapshot must remain Sonnet.
    let a3 = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL),
        &pr,
        3,
        None,
        tmp.path(),
        None,
    );
    assert_eq!(rung_label(&a3), "to_1m_model");
    assert_eq!(
        ctx.overflow_original_model.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
        "observation 3: original model must STILL be SONNET_MODEL even when current model is Opus",
    );
}

// ---------- AC TEST-002-3: run_id None through JSONL end-to-end -------------

/// When `handle_prompt_too_long` is called with `run_id=None`, the JSONL line
/// it appends must either omit the `run_id` field entirely OR represent it as
/// JSON `null`. It MUST NOT serialize as an empty string `""`.
#[test]
fn run_id_none_serializes_correctly() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "RNI-FEAT-001";
    let conn = make_conn_with_task(task_id);
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
        None, // <-- run_id is None
        tmp.path(),
        None,
    );

    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1, "expected exactly one JSONL line");
    assert_eq!(events[0].run_id, None, "run_id must deserialize as None");

    // Verify in raw JSON: must not appear as empty string.
    let raw = std::fs::read_to_string(tmp.path().join("overflow-events.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    match v.get("run_id") {
        None => {}                          // field omitted — correct
        Some(serde_json::Value::Null) => {} // explicit null — acceptable
        Some(serde_json::Value::String(s)) if s.is_empty() => {
            panic!("run_id serialized as empty string; must be absent or null")
        }
        Some(other) => panic!("run_id has unexpected value: {other:?}"),
    }
}

// ---------- AC TEST-002-4: effective_model None → '(default)' in dump header

/// When `effective_model=None` is passed, the dump file header must display
/// `model: (default)` — never `model: None`, never `model: ` (empty).
#[test]
fn effective_model_none_dump_header() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "EMN-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    let _ = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"),
        None, // <-- effective_model is None
        &pr,
        1,
        None,
        tmp.path(),
        None,
    );

    let sanitized = sanitize_id_for_filename(task_id);
    let dump_files = list_dump_files(tmp.path(), &sanitized);
    assert_eq!(dump_files.len(), 1, "expected exactly one dump");
    let body = std::fs::read_to_string(&dump_files[0]).expect("read dump");

    assert!(
        body.contains("model: (default)"),
        "dump header must contain 'model: (default)' when effective_model is None; got:\n{body}",
    );
    assert!(
        !body.contains("model: None"),
        "dump header must not contain 'model: None'; got:\n{body}",
    );
}

// ---------- AC TEST-002-5: rung 4 (blocked) still writes observability ------

/// Terminal `Blocked` rung must create BOTH a dump file AND a JSONL line with
/// `recovery.action == "blocked"`. Diagnostics must not be silently skipped
/// at the dead end.
///
/// Distinct from `blocked_rung_writes_both_dump_and_jsonl` (AC #9) in that
/// this test uses a fresh fixture and verifies the JSONL raw JSON directly.
#[test]
fn rung_4_writes_observability() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "R4OBS-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL_1M),
        &pr,
        1,
        Some("run-r4obs"),
        tmp.path(),
        None,
    );
    assert!(
        matches!(action, RecoveryAction::Blocked),
        "Opus[1M]+high must produce Blocked action",
    );
    assert_eq!(task_status(&conn, task_id), "blocked");

    // Dump must exist.
    let sanitized = sanitize_id_for_filename(task_id);
    let dump_files = list_dump_files(tmp.path(), &sanitized);
    assert_eq!(
        dump_files.len(),
        1,
        "rung 4 must still write a dump; found {dump_files:?}",
    );

    // JSONL must contain one line with action == "blocked".
    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1, "expected one JSONL line for rung 4");
    assert!(
        matches!(events[0].recovery, RecoveryAction::Blocked),
        "JSONL recovery action must be Blocked",
    );
    let raw = std::fs::read_to_string(tmp.path().join("overflow-events.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(
        v["recovery"]["action"],
        serde_json::Value::String("blocked".into()),
        "raw JSONL must have recovery.action == 'blocked'",
    );
}

// ---------- AC TEST-002-6: dump uses sent effort, not resolved effort --------

/// When rung 1 fires (effort downgrade from xhigh → high), the dump header
/// must record the SENT effort (`xhigh`) — not the resolved `high` that will
/// be used on the next attempt. The JSONL event must show
/// `recovery.action == "downgrade_effort"` with `new_effort == "high"`.
#[test]
fn dump_uses_sent_effort_not_resolved() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "DUE-FEAT-001";
    let conn = make_conn_with_task(task_id);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("xhigh"), // sent effort
        Some(SONNET_MODEL),
        &pr,
        1,
        None,
        tmp.path(),
        None,
    );
    assert_eq!(rung_label(&action), "downgrade_effort");
    assert!(
        matches!(action, RecoveryAction::DowngradeEffort { ref new_effort } if new_effort == "high")
    );

    // Dump header must show the sent effort (xhigh), not the new effort (high).
    let sanitized = sanitize_id_for_filename(task_id);
    let dump_files = list_dump_files(tmp.path(), &sanitized);
    assert_eq!(dump_files.len(), 1, "expected exactly one dump");
    let body = std::fs::read_to_string(&dump_files[0]).expect("read dump");
    assert!(
        body.contains("effort: xhigh"),
        "dump header must record sent effort 'xhigh', not the downgraded 'high'; got:\n{body}",
    );

    // JSONL must record the recovery action with the new (resolved) effort.
    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1);
    assert!(
        matches!(
            &events[0].recovery,
            RecoveryAction::DowngradeEffort { new_effort } if new_effort == "high"
        ),
        "JSONL recovery.new_effort must be 'high' (the downgraded value)",
    );
    // The JSONL effort field records the SENT effort.
    assert_eq!(
        events[0].effort.as_deref(),
        Some("xhigh"),
        "JSONL effort field must record the sent effort 'xhigh'",
    );
}
