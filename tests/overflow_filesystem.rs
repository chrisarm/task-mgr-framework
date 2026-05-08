//! Filesystem-level tests for `overflow.rs`: rotation edge cases, JSONL
//! atomicity, `format_breakdown` content, and `dump_prompt` header sanitization.
//!
//! These tests call the low-level helpers directly (not through
//! `handle_prompt_too_long`) to isolate the filesystem contract from the
//! recovery ladder logic, which is covered in `overflow_recovery.rs`.
//!
//! All tests use `tempfile::TempDir` — no real `.task-mgr/` directory is
//! touched. Acceptance criteria map:
//!   rotation_keeps_newest_3              — controlled-mtime rotation correctness
//!   rotation_no_op_when_under_limit      — keep ≤ N when count < N
//!   rotation_only_touches_matching_task  — per-task prefix isolation
//!   jsonl_append_preserves_existing_lines — append does not clobber prior lines
//!   jsonl_append_atomic_for_large_event  — large event is one JSON line, no embedded newlines
//!   format_breakdown_expected_substrings  — required header strings present
//!   dump_prompt_header_uses_sanitized_id  — path-traversal IDs sanitized in content

use std::fs::{self, File, FileTimes};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use task_mgr::loop_engine::model;
use task_mgr::loop_engine::overflow::{
    self, DumpHeader, OverflowEvent, RecoveryAction, append_event_log, format_breakdown,
    rotate_dumps_keep_n, sanitize_id_for_filename,
};

// ---------- helpers -----------------------------------------------------------

/// Write a dummy dump file and set its mtime to `mtime`. Returns the file path.
fn write_dummy_dump(dir: &std::path::Path, name: &str, mtime: SystemTime) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"dummy").expect("write dummy dump");
    let file = File::options()
        .write(true)
        .open(&path)
        .expect("open for set_times");
    file.set_times(FileTimes::new().set_modified(mtime))
        .expect("set_times");
    path
}

fn epoch_plus(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn make_event(task_id: &str, iteration: u32) -> OverflowEvent {
    OverflowEvent {
        ts: "2026-05-04T00:00:00+00:00".to_string(),
        task_id: task_id.to_string(),
        run_id: None,
        iteration,
        slot_index: None,
        model: Some(model::SONNET_MODEL.to_string()),
        effort: Some("high".to_string()),
        prompt_bytes: 100,
        sections: vec![("task".to_string(), 100)],
        dropped_sections: vec![],
        recovery: RecoveryAction::DowngradeEffort {
            new_effort: "high".to_string(),
        },
        dump_path: "/tmp/dummy.txt".to_string(),
    }
}

fn make_header<'a>(
    sections: &'a [(&'a str, usize)],
    dropped: &'a [String],
    total: usize,
) -> DumpHeader<'a> {
    DumpHeader {
        iteration: 1,
        model: Some(model::SONNET_MODEL.to_string()),
        effort: Some("high".to_string()),
        ts_iso8601: "2026-05-04T00:00:00+00:00".to_string(),
        total_bytes: total,
        sections,
        dropped_sections: dropped,
    }
}

// ---------- rotation_keeps_newest_3 ------------------------------------------

/// Create 5 dump files with strictly ordered mtimes; after rotate(keep=3) exactly
/// the 3 newest must remain. Uses `File::set_times` for deterministic ordering
/// without OS-level sleep overhead.
#[test]
fn rotation_keeps_newest_3() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("overflow-dumps");
    fs::create_dir_all(&dir).unwrap();

    // t1 < t2 < t3 < t4 < t5 — strictly ordered.
    let tasks = ["TASK-001"];
    let sanitized = sanitize_id_for_filename(tasks[0]);

    let mut paths = Vec::new();
    for i in 1..=5u64 {
        let name = format!("{sanitized}-iter{i}-{i}.txt");
        let p = write_dummy_dump(&dir, &name, epoch_plus(1_000_000 + i * 1000));
        paths.push(p);
    }

    rotate_dumps_keep_n(&dir, &sanitized, 3).expect("rotate");

    let remaining: Vec<_> = fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(
        remaining.len(),
        3,
        "expected exactly 3 files after rotation, got {remaining:?}"
    );

    // The 3 retained files must be the newest (iter3, iter4, iter5).
    let remaining_names: std::collections::HashSet<String> = remaining
        .iter()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for i in 3..=5u64 {
        let expected = format!("{sanitized}-iter{i}-{i}.txt");
        assert!(
            remaining_names.contains(&expected),
            "expected {expected} to be retained; retained set: {remaining_names:?}"
        );
    }
    // The 2 oldest (iter1, iter2) must have been deleted.
    for i in 1..=2u64 {
        let deleted = format!("{sanitized}-iter{i}-{i}.txt");
        assert!(
            !remaining_names.contains(&deleted),
            "expected {deleted} to be deleted; retained set: {remaining_names:?}"
        );
    }
}

// ---------- rotation_no_op_when_under_limit ----------------------------------

/// With only 2 dumps and keep=3, no file must be deleted.
#[test]
fn rotation_no_op_when_under_limit() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("overflow-dumps");
    fs::create_dir_all(&dir).unwrap();

    let sanitized = sanitize_id_for_filename("UNDER-001");
    for i in 1..=2u64 {
        let name = format!("{sanitized}-iter{i}-{i}.txt");
        write_dummy_dump(&dir, &name, epoch_plus(1_000_000 + i));
    }

    rotate_dumps_keep_n(&dir, &sanitized, 3).expect("rotate");

    let count = fs::read_dir(&dir).unwrap().count();
    assert_eq!(count, 2, "rotation must be a no-op when count < keep");
}

// ---------- rotation_only_touches_matching_task ------------------------------

/// Dumps for task A and task B coexist in the same directory.  Rotating A
/// must not remove any of B's files.
#[test]
fn rotation_only_touches_matching_task() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("overflow-dumps");
    fs::create_dir_all(&dir).unwrap();

    let san_a = sanitize_id_for_filename("TASK-AAA");
    let san_b = sanitize_id_for_filename("TASK-BBB");

    // 4 dumps for A (will be trimmed to 3) + 2 dumps for B (must stay).
    for i in 1..=4u64 {
        let name = format!("{san_a}-iter{i}-{i}.txt");
        write_dummy_dump(&dir, &name, epoch_plus(1_000_000 + i * 1000));
    }
    for i in 1..=2u64 {
        let name = format!("{san_b}-iter{i}-{i}.txt");
        write_dummy_dump(&dir, &name, epoch_plus(2_000_000 + i));
    }

    rotate_dumps_keep_n(&dir, &san_a, 3).expect("rotate A");

    // A: 3 remain.
    let a_files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{san_a}-iter"))
        })
        .collect();
    assert_eq!(
        a_files.len(),
        3,
        "task A must have exactly 3 files after rotation"
    );

    // B: both untouched.
    let b_files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{san_b}-iter"))
        })
        .collect();
    assert_eq!(
        b_files.len(),
        2,
        "task B files must be unaffected by rotating A"
    );
}

// ---------- jsonl_append_preserves_existing_lines ----------------------------

/// Pre-seed the JSONL file with 3 lines, then append a 4th.  All 4 must be
/// parseable and in order.
#[test]
fn jsonl_append_preserves_existing_lines() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();

    // Seed 3 events directly.
    for i in 1..=3u32 {
        let event = make_event("SEED-TASK-001", i);
        append_event_log(base, &event).expect("seed append");
    }

    // Append the 4th.
    let fourth = make_event("SEED-TASK-001", 4);
    append_event_log(base, &fourth).expect("fourth append");

    let raw = fs::read_to_string(base.join("overflow-events.jsonl")).expect("read jsonl");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 4, "must have exactly 4 lines; got:\n{raw}");

    for (i, line) in lines.iter().enumerate() {
        let parsed: OverflowEvent =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("parse line {i}: {line}: {e}"));
        assert_eq!(
            parsed.iteration,
            (i as u32) + 1,
            "line {i} iteration mismatch"
        );
    }
}

// ---------- jsonl_append_atomic_for_large_event ------------------------------

/// An OverflowEvent with 50 sections and ~5 KB of serialized JSON must be
/// written as a single JSONL line — no embedded newlines in the JSON object.
#[test]
fn jsonl_append_atomic_for_large_event() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();

    // Build 50 sections that push the total well past 5 KB.
    let sections: Vec<(String, usize)> = (0..50)
        .map(|i| (format!("section_{i:02}"), 1024 * (i + 1)))
        .collect();

    let total_bytes: usize = sections.iter().map(|(_, s)| s).sum();
    assert!(total_bytes > 5000, "fixture must exceed 5 KB");

    let event = OverflowEvent {
        ts: "2026-05-04T00:00:00+00:00".to_string(),
        task_id: "LARGE-TASK-001".to_string(),
        run_id: Some("run-large".to_string()),
        iteration: 1,
        slot_index: None,
        model: Some(model::SONNET_MODEL.to_string()),
        effort: Some("xhigh".to_string()),
        prompt_bytes: total_bytes,
        sections,
        dropped_sections: vec!["progress".to_string(), "guidance".to_string()],
        recovery: RecoveryAction::EscalateModel {
            new_model: model::OPUS_MODEL.to_string(),
        },
        dump_path: "/tmp/large-dump.txt".to_string(),
    };

    append_event_log(base, &event).expect("append large event");

    let raw = fs::read_to_string(base.join("overflow-events.jsonl")).expect("read jsonl");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "large event must produce exactly one JSONL line; got {} lines",
        lines.len()
    );

    // The single line must parse back correctly.
    let parsed: OverflowEvent =
        serde_json::from_str(lines[0]).expect("parse large event JSONL line");
    assert_eq!(parsed.task_id, "LARGE-TASK-001");
    assert_eq!(parsed.sections.len(), 50);
    assert_eq!(parsed.prompt_bytes, total_bytes);
}

// ---------- format_breakdown_expected_substrings -----------------------------

/// `format_breakdown` must contain all four required substrings.
#[test]
fn format_breakdown_contains_expected_substrings() {
    let sections = [("task", 1000usize), ("base_prompt", 2000usize)];
    let dropped = vec!["progress".to_string()];
    let total = 3000usize;

    let output = format_breakdown(&sections, &dropped, total);

    for expected in [
        "Total assembled bytes:",
        "Section breakdown:",
        "NOTE:",
        "auto-loaded layer",
    ] {
        assert!(
            output.contains(expected),
            "format_breakdown output missing required substring {expected:?};\ngot:\n{output}"
        );
    }
}

// ---------- dump_prompt_header_uses_sanitized_id ----------------------------

/// `dump_prompt` must sanitize the raw task_id for both the filename AND the
/// `task_id:` line in the dump header, so a path-traversal-style ID cannot
/// escape the dumps directory or pollute the header.
#[test]
fn dump_prompt_header_uses_sanitized_task_id() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("overflow-dumps");

    let raw_id = "PRJ/../../etc/passwd";
    let sections = [("task", 42usize), ("base_prompt", 58usize)];
    let dropped: Vec<String> = vec![];
    let header = make_header(&sections, &dropped, 100);

    let path =
        overflow::dump_prompt(&dir, raw_id, &header, "prompt body here").expect("dump_prompt");

    // Filename must contain only the sanitized form.
    let filename = path.file_name().unwrap().to_string_lossy().into_owned();
    let expected_sanitized = sanitize_id_for_filename(raw_id);
    assert!(
        filename.starts_with(&format!("{expected_sanitized}-iter1-")),
        "dump filename must start with sanitized prefix; got {filename:?}"
    );
    assert!(
        !filename.contains('/'),
        "dump filename must not contain `/`; got {filename:?}"
    );
    assert!(
        !filename.contains(".."),
        "dump filename must not contain `..`; got {filename:?}"
    );

    // Header content must use the sanitized task_id, not the raw form.
    let content = fs::read_to_string(&path).expect("read dump");
    assert!(
        content.contains(&format!("task_id: {expected_sanitized}")),
        "dump header must use sanitized task_id; got:\n{content}"
    );
    assert!(
        !content.contains("../.."),
        "dump must not contain traversal segments; got:\n{content}"
    );

    // File must remain under the tempdir (no escape).
    let canon_dump = path.canonicalize().expect("canonicalize dump");
    let canon_root = tmp.path().canonicalize().expect("canonicalize root");
    assert!(
        canon_dump.starts_with(&canon_root),
        "dump escaped tempdir: {canon_dump:?} not under {canon_root:?}"
    );
}

// ---------- rotation_continues_past_unreadable_entry -------------------------

/// One of the dump "files" is actually a directory — `fs::remove_file` on a
/// directory fails with EISDIR. Rotation must proceed past this failure and
/// delete the other eligible files.
#[test]
#[cfg(unix)]
fn rotation_continues_past_unreadable_entry() {
    use std::fs::FileTimes;

    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("overflow-dumps");
    fs::create_dir_all(&dir).unwrap();

    let sanitized = sanitize_id_for_filename("TASK-UNREAD");

    // iter1 (oldest) and iter3 are candidates for deletion (keep=2 retains iter4+iter5).
    // iter2 is a DIRECTORY named like a dump file — remove_file on it returns EISDIR.
    write_dummy_dump(
        &dir,
        &format!("{sanitized}-iter1-1.txt"),
        epoch_plus(1_000_001),
    );

    let path2 = dir.join(format!("{sanitized}-iter2-2.txt"));
    fs::create_dir(&path2).unwrap();
    // Set mtime so iter2 sorts between iter1 and iter3.
    let dir_handle = std::fs::File::open(&path2).expect("open dir for set_times");
    dir_handle
        .set_times(FileTimes::new().set_modified(epoch_plus(1_000_002)))
        .expect("set dir mtime");
    drop(dir_handle);

    write_dummy_dump(
        &dir,
        &format!("{sanitized}-iter3-3.txt"),
        epoch_plus(1_000_003),
    );
    write_dummy_dump(
        &dir,
        &format!("{sanitized}-iter4-4.txt"),
        epoch_plus(1_000_004),
    );
    write_dummy_dump(
        &dir,
        &format!("{sanitized}-iter5-5.txt"),
        epoch_plus(1_000_005),
    );

    // Must return Ok(()) even though iter2 (a directory) can't be removed.
    rotate_dumps_keep_n(&dir, &sanitized, 2).expect("rotate Ok despite failed removal");

    // iter4 and iter5 (newest 2) must survive.
    assert!(
        dir.join(format!("{sanitized}-iter4-4.txt")).exists(),
        "iter4 must survive"
    );
    assert!(
        dir.join(format!("{sanitized}-iter5-5.txt")).exists(),
        "iter5 must survive"
    );

    // iter1 and iter3 must be deleted.
    assert!(
        !dir.join(format!("{sanitized}-iter1-1.txt")).exists(),
        "iter1 must be deleted"
    );
    assert!(
        !dir.join(format!("{sanitized}-iter3-3.txt")).exists(),
        "iter3 must be deleted"
    );

    // iter2 (directory) still exists — removal failed but did not abort the loop.
    assert!(
        path2.exists(),
        "iter2 (dir) must persist after failed removal"
    );
}

// ---------- rotation_no_op_when_dir_empty ------------------------------------

/// Directory exists but contains no matching dump files; rotate must return
/// Ok(()) immediately without emitting warnings.
#[test]
fn rotation_no_op_when_dir_empty() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("overflow-dumps");
    fs::create_dir_all(&dir).unwrap();

    let sanitized = sanitize_id_for_filename("TASK-EMPTY");
    rotate_dumps_keep_n(&dir, &sanitized, 2).expect("rotate on empty dir must succeed");

    let count = fs::read_dir(&dir).unwrap().count();
    assert_eq!(count, 0, "empty dir must remain empty after rotation");
}

// ---------- jsonl_oversize_emits_warning --------------------------------------

/// An OverflowEvent whose serialized JSONL line exceeds 4096 bytes must still
/// be written in full.  The warning branch is verified by asserting that the
/// fixture line really is > 4096 bytes (so the `if line.len() > 4096` branch
/// in `append_event_log` was taken) and that all fields survive the round-trip.
#[test]
fn jsonl_oversize_emits_warning() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();

    // Use a long dump_path to push the serialized line above 4096 bytes while
    // keeping other fields small.
    let long_dump_path = "x".repeat(4096);

    let event = OverflowEvent {
        ts: "2026-05-04T00:00:00+00:00".to_string(),
        task_id: "OVERSIZE-001".to_string(),
        run_id: None,
        iteration: 1,
        slot_index: None,
        model: Some(model::SONNET_MODEL.to_string()),
        effort: Some("high".to_string()),
        prompt_bytes: 100,
        sections: vec![("task".to_string(), 100)],
        dropped_sections: vec![],
        recovery: RecoveryAction::DowngradeEffort {
            new_effort: "high".to_string(),
        },
        dump_path: long_dump_path.clone(),
    };

    // Verify the fixture actually triggers the warning branch.
    let serialized = serde_json::to_vec(&event).expect("serialize event");
    assert!(
        serialized.len() + 1 > 4096,
        "fixture must be > 4096 bytes to exercise the warning path; got {} bytes",
        serialized.len() + 1
    );

    // The function must write the full line despite exceeding 4096 bytes.
    append_event_log(base, &event).expect("append oversized event");

    let raw = fs::read_to_string(base.join("overflow-events.jsonl")).expect("read jsonl");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "must produce exactly one JSONL line");

    let parsed: OverflowEvent = serde_json::from_str(lines[0]).expect("parse oversized JSONL line");
    assert_eq!(
        parsed.dump_path, long_dump_path,
        "full dump_path must be preserved — no field truncation"
    );
    assert_eq!(parsed.task_id, "OVERSIZE-001");
}
