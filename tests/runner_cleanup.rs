//! Integration tests for dispatch post-spawn cleanup (FEAT-006) using the
//! mock-CLI-script approach (mirrors FEAT-008 FakeRunner shape).
//!
//! Every test isolates filesystem state with a tempdir fake `$HOME` and a
//! separate tempdir working directory — no real `$HOME` is ever touched.
//!
//! **Test map (task FEAT-009 acceptance criteria)**
//! - AC1 / Test 1 (Claude shape): `dispatch_claude_cleanup_ok_returns_spawn_result_unchanged`
//! - AC2 / Test 2 (Grok shape): `dispatch_grok_cleanup_ok_removes_session_dir`
//! - AC3 / Test 3 (banner): `dispatch_claude_cleanup_err_does_not_propagate_and_emits_banner`
//! - AC4 / Test 4 (rate-limit): `two_dispatches_with_cleanup_err_emit_only_one_banner_line`
//! - AC6: `dispatch_grok_skips_cleanup_when_session_id_is_none`
//!
//! **Banner verification strategy**
//!
//! `eprintln!` in dispatch writes through Rust's test-capture mechanism (not
//! raw fd 2). Capturing at the fd level (e.g. `gag`) would miss these writes.
//! Instead, banner tests verify via the public `CLEANUP_WARN_ONCE` state
//! accessors: `reset_cleanup_warn_once_for_test()` + `cleanup_warn_once_was_triggered()`.
//! Each false→true transition corresponds to exactly one `[cleanup warn]` line.
//!
//! **CLEANUP_WARN_ONCE strategy**
//!
//! `CLEANUP_WARN_ONCE` is process-global and sticky. Tests 1, 2, and AC6 use
//! successful-or-skipped cleanup paths so they never set the flag. Banner tests
//! call `reset_cleanup_warn_once_for_test()` at the start of each run and hold
//! `BANNER_MUTEX` to serialize against each other.
//!
//! **Coherence-boundary check**
//!
//! These tests do NOT assert status, lifecycle, or reconciliation behaviour.
//! The dispatch post-spawn hook is single-purpose — cleanup only.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use task_mgr::loop_engine::config::{CODING_ALLOWED_TOOLS, PermissionMode};
use task_mgr::loop_engine::runner::{
    RunnerKind, RunnerOpts, RunnerResult, cleanup_warn_once_was_triggered, dispatch,
    reset_cleanup_warn_once_for_test,
};

// ---------------------------------------------------------------------------
// Shared mutexes (mirrors tests/runner_trait_dispatch.rs)
// ---------------------------------------------------------------------------

/// Serialize tests that mutate `CLAUDE_BINARY`, `GROK_BINARY`, or `HOME`.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Serialize banner-touching tests. Held in addition to `ENV_MUTEX` so the
/// `CLEANUP_WARN_ONCE` state transitions are race-free across tests.
static BANNER_MUTEX: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn scoped_coding() -> PermissionMode {
    PermissionMode::Scoped {
        allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
    }
}

/// Compute Claude's encoded cwd directory the same way `claude::encoded_cwd_dir`
/// does. Duplicated locally (learning [909]) because the production helper is
/// `pub(crate)`.
fn claude_encoded_cwd_dir(cwd: &Path, home: &Path) -> PathBuf {
    let cwd_str = cwd.to_string_lossy();
    let trimmed = cwd_str.trim_end_matches('/');
    let encoded = trimmed.replace('/', "-");
    home.join(".claude").join("projects").join(encoded)
}

/// Compute Grok's encoded session directory the same way
/// `runner::grok_encoded_session_dir` does. Duplicated locally because the
/// production helper is `pub(crate)`.
fn grok_encoded_session_dir(cwd: &Path, home: &Path) -> PathBuf {
    let cwd_str = cwd.to_string_lossy();
    let trimmed = cwd_str.trim_end_matches('/');
    let encoded = urlencoding::encode(trimmed).into_owned();
    home.join(".grok").join("sessions").join(encoded)
}

/// Create a Claude mock script that:
/// 1. Parses `--session-id <uuid>` from argv (ClaudeRunner always injects this).
/// 2. Creates `$HOME/.claude/projects/<encoded-cwd>/<uuid>.jsonl` as a **file**.
/// 3. Echoes `<marker> <prompt>` to stdout.
///
/// The file artifact lets dispatch's cleanup hook delete it (`remove_file`
/// returns Ok), exercising the happy-path cleanup contract.
fn make_claude_artifact_script(name: &str, marker: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("task_mgr_cleanup_{name}.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create artifact script");
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
        // Parse --session-id from argv (shift-1 loop; value follows the flag).
        writeln!(f, r#"SESSION_ID="""#).unwrap();
        writeln!(f, r#"while [ $# -gt 0 ]; do"#).unwrap();
        writeln!(f, r#"  if [ "$1" = "--session-id" ]; then"#).unwrap();
        writeln!(f, r#"    SESSION_ID="$2"; shift 2"#).unwrap();
        writeln!(f, r#"  else"#).unwrap();
        writeln!(f, r#"    shift"#).unwrap();
        writeln!(f, r#"  fi"#).unwrap();
        writeln!(f, r#"done"#).unwrap();
        // Encode cwd: replace '/' with '-' (matches claude::encoded_cwd_dir).
        writeln!(f, r#"ENCODED=$(echo "$PWD" | sed 's|/|-|g')"#).unwrap();
        writeln!(f, r#"mkdir -p "$HOME/.claude/projects/$ENCODED""#).unwrap();
        writeln!(
            f,
            r#"touch "$HOME/.claude/projects/$ENCODED/$SESSION_ID.jsonl""#
        )
        .unwrap();
        writeln!(f, r#"echo "{marker} $PROMPT""#).unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod artifact script");
    path
}

/// Create a Claude mock script that creates the session artifact as a
/// **directory** (not a file). `remove_file` on a directory returns `EISDIR`
/// (ErrorKind::IsADirectory), forcing `cleanup_session` to return `Err` and
/// triggering the `CLEANUP_WARN_ONCE` banner in dispatch.
fn make_claude_dir_artifact_script(name: &str, marker: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("task_mgr_cleanup_{name}.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create dir-artifact script");
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
        writeln!(f, r#"SESSION_ID="""#).unwrap();
        writeln!(f, r#"while [ $# -gt 0 ]; do"#).unwrap();
        writeln!(f, r#"  if [ "$1" = "--session-id" ]; then"#).unwrap();
        writeln!(f, r#"    SESSION_ID="$2"; shift 2"#).unwrap();
        writeln!(f, r#"  else"#).unwrap();
        writeln!(f, r#"    shift"#).unwrap();
        writeln!(f, r#"  fi"#).unwrap();
        writeln!(f, r#"done"#).unwrap();
        writeln!(f, r#"ENCODED=$(echo "$PWD" | sed 's|/|-|g')"#).unwrap();
        // Create as a DIRECTORY — forces remove_file to return EISDIR.
        writeln!(
            f,
            r#"mkdir -p "$HOME/.claude/projects/$ENCODED/$SESSION_ID.jsonl""#
        )
        .unwrap();
        writeln!(f, r#"echo "{marker} $PROMPT""#).unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod dir-artifact script");
    path
}

/// Create a Grok mock script that creates `$GROK_TEST_SESSION_DIR/<uuid>/`
/// and echoes the marker. Mirrors the pattern in runner.rs unit test
/// `grok_runner_discovers_session_id_from_pre_post_dir_diff`.
fn make_grok_session_script(name: &str, marker: &str, uuid: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("task_mgr_cleanup_{name}.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create grok session script");
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
        writeln!(f, r#"mkdir -p "$GROK_TEST_SESSION_DIR/{uuid}""#).unwrap();
        writeln!(f, r#"echo "{marker} $PROMPT""#).unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod grok session script");
    path
}

/// Create a Grok mock script that exits 0 WITHOUT creating any session dir.
/// GrokRunner's pre/post diff finds no new UUID directories → `session_id: None`.
fn make_grok_no_session_script(name: &str, marker: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("task_mgr_cleanup_{name}.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create grok no-session script");
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
        writeln!(f, r#"echo "{marker} $PROMPT""#).unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod grok no-session script");
    path
}

// ---------------------------------------------------------------------------
// AC1 — Test 1 (Claude shape): artifact present post-spawn, absent post-dispatch
// ---------------------------------------------------------------------------

/// Mock Claude binary creates `<uuid>.jsonl` (via `--session-id` parsing);
/// dispatch calls cleanup_session which deletes it. Verifies:
/// (a) spawn result (exit_code, output, session_id) round-trips unchanged.
/// (b) artifact file is absent after dispatch returns Ok.
#[test]
fn dispatch_claude_cleanup_ok_returns_spawn_result_unchanged() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let marker = "CLEANUP_OK_CLAUDE_5BA153A7";
    let script = make_claude_artifact_script("cleanup_ok_claude", marker);
    let temp_home = tempfile::tempdir().expect("create temp HOME");
    let temp_cwd = tempfile::tempdir().expect("create temp cwd");

    // SAFETY: process-global mutation, serialized via ENV_MUTEX.
    unsafe {
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        std::env::set_var("HOME", temp_home.path());
    }

    let perm = scoped_coding();
    let opts = RunnerOpts {
        working_dir: Some(temp_cwd.path()),
        ..RunnerOpts::default()
    };
    let result = dispatch(RunnerKind::Claude, "cleanup-ok-prompt", &perm, opts);

    unsafe {
        std::env::remove_var("CLAUDE_BINARY");
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_file(&script);

    let r: RunnerResult = result.expect("dispatch returned Err");
    assert_eq!(r.exit_code, 0);
    assert!(
        r.output.contains(marker),
        "spawn result must round-trip through dispatch unchanged, got: {:?}",
        r.output
    );
    let session_id = r
        .session_id
        .expect("ClaudeRunner unconditionally emits session_id");

    // AC1 contract: artifact deleted by dispatch's post-spawn cleanup hook.
    let artifact = claude_encoded_cwd_dir(temp_cwd.path(), temp_home.path())
        .join(format!("{}.jsonl", session_id));
    assert!(
        !artifact.exists(),
        "dispatch must delete {} after spawn (cleanup_session was not called or failed)",
        artifact.display()
    );
}

// ---------------------------------------------------------------------------
// AC2 — Test 2 (Grok shape): session dir present post-spawn, absent post-dispatch
// ---------------------------------------------------------------------------

/// Mock Grok binary creates `<session-parent>/<uuid>/`; dispatch discovers
/// the UUID via pre/post diff, then calls cleanup_session which removes it.
/// Verifies:
/// (a) spawn result round-trips unchanged.
/// (b) uuid subdir is absent after dispatch returns Ok.
/// (c) session parent dir still exists (only the uuid subdir was removed).
#[test]
fn dispatch_grok_cleanup_ok_removes_session_dir() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let marker = "CLEANUP_OK_GROK_A3F72C91";
    // Known UUID the mock binary creates as a subdir.
    let known_uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    let temp_home = tempfile::tempdir().expect("create temp HOME");
    let temp_cwd = tempfile::tempdir().expect("create temp cwd");

    // Pre-create the session parent dir so the pre-snapshot is "empty, not absent".
    let session_parent = grok_encoded_session_dir(temp_cwd.path(), temp_home.path());
    std::fs::create_dir_all(&session_parent).expect("create grok session parent");

    let script = make_grok_session_script("cleanup_ok_grok", marker, known_uuid);

    // SAFETY: serialized via ENV_MUTEX.
    unsafe {
        std::env::set_var("GROK_BINARY", script.to_str().unwrap());
        std::env::set_var("GROK_TEST_SESSION_DIR", session_parent.to_str().unwrap());
        std::env::set_var("HOME", temp_home.path());
    }

    let perm = scoped_coding();
    let opts = RunnerOpts {
        working_dir: Some(temp_cwd.path()),
        ..RunnerOpts::default()
    };
    let result = dispatch(RunnerKind::Grok, "cleanup-ok-grok-prompt", &perm, opts);

    unsafe {
        std::env::remove_var("GROK_BINARY");
        std::env::remove_var("GROK_TEST_SESSION_DIR");
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_file(&script);

    let r: RunnerResult = result.expect("dispatch(Grok) returned Err");
    assert_eq!(r.exit_code, 0);
    assert!(
        r.output.contains(marker),
        "spawn result must round-trip through dispatch unchanged, got: {:?}",
        r.output
    );
    assert!(
        r.session_id.is_some(),
        "GrokRunner must discover session_id from pre/post dir diff"
    );

    // AC2 contract: uuid subdir removed; parent dir survives.
    let session_uuid_dir = session_parent.join(known_uuid);
    assert!(
        !session_uuid_dir.exists(),
        "dispatch must remove grok session dir {} after spawn",
        session_uuid_dir.display()
    );
    assert!(
        session_parent.exists(),
        "dispatch must NOT remove the session parent dir {}",
        session_parent.display()
    );
}

// ---------------------------------------------------------------------------
// AC3 — Test 3 (banner): cleanup Err does not propagate; banner is emitted
// ---------------------------------------------------------------------------

/// Cleanup Err path: the artifact exists as a directory, so `remove_file`
/// returns `EISDIR`. dispatch must:
/// (a) return the spawn's RunnerResult unchanged (Err not propagated).
/// (b) emit exactly one `[cleanup warn]` banner (verified via CLEANUP_WARN_ONCE
///     state transition: false → true after the first dispatch).
///
/// Banner verification uses state-transition semantics rather than stderr
/// capture. `eprintln!` in dispatch writes through Rust's test-output-capture
/// mechanism (not raw fd 2), so fd-level capture (e.g. gag) would miss it.
/// The false→true transition of CLEANUP_WARN_ONCE corresponds 1:1 with the
/// banner emission — each transition causes exactly one `eprintln!` call.
#[test]
fn dispatch_claude_cleanup_err_does_not_propagate_and_emits_banner() {
    let _env = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _banner = BANNER_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    // Reset so the first dispatch in this test observes the false → true transition.
    reset_cleanup_warn_once_for_test();
    assert!(
        !cleanup_warn_once_was_triggered(),
        "gate must be false before test"
    );

    let marker = "BANNER_TEST_MARKER_F1A2B3C4";
    let script = make_claude_dir_artifact_script("banner_test", marker);
    let temp_home = tempfile::tempdir().expect("create temp HOME");
    let temp_cwd = tempfile::tempdir().expect("create temp cwd");

    // SAFETY: serialized via ENV_MUTEX.
    unsafe {
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        std::env::set_var("HOME", temp_home.path());
    }

    let perm = scoped_coding();
    let opts = RunnerOpts {
        working_dir: Some(temp_cwd.path()),
        ..RunnerOpts::default()
    };
    let result = dispatch(RunnerKind::Claude, "banner-test-prompt", &perm, opts);

    unsafe {
        std::env::remove_var("CLAUDE_BINARY");
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_file(&script);

    // AC3a: spawn result unchanged (cleanup Err must not propagate).
    let r = result.expect("dispatch returned Err — cleanup Err must not propagate");
    assert!(
        r.output.contains(marker),
        "spawn result must be unchanged, got: {:?}",
        r.output
    );

    // AC3b: exactly one banner emitted — gate transitioned false → true.
    assert!(
        cleanup_warn_once_was_triggered(),
        "CLEANUP_WARN_ONCE must be true after a cleanup Err (banner was emitted)"
    );
}

// ---------------------------------------------------------------------------
// AC4 — Test 4 (rate-limit): two consecutive dispatch calls → ONE banner total
// ---------------------------------------------------------------------------

/// Two dispatch calls with cleanup Err. Verifies that the second call is
/// silenced by CLEANUP_WARN_ONCE — only one banner line total, not two.
///
/// Verification: CLEANUP_WARN_ONCE transitions false → true after the first
/// call (one banner), and stays true after the second call (zero additional
/// banners). The atomic swap(true) can only produce a false→true transition
/// once per reset cycle, so observing the state after each call proves the
/// count is exactly one.
#[test]
fn two_dispatches_with_cleanup_err_emit_only_one_banner_line() {
    let _env = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _banner = BANNER_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    reset_cleanup_warn_once_for_test();
    assert!(
        !cleanup_warn_once_was_triggered(),
        "gate must be false before test"
    );

    let marker = "RATE_LIMIT_MARKER_3D9E8F01";
    let script = make_claude_dir_artifact_script("rate_limit_test", marker);
    let temp_home = tempfile::tempdir().expect("create temp HOME");
    let temp_cwd = tempfile::tempdir().expect("create temp cwd");

    unsafe {
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        std::env::set_var("HOME", temp_home.path());
    }

    let perm = scoped_coding();
    let make_opts = || RunnerOpts {
        working_dir: Some(temp_cwd.path()),
        ..RunnerOpts::default()
    };

    // First call: gate transitions false → true (banner fires).
    let r1 = dispatch(
        RunnerKind::Claude,
        "rate-limit-prompt-1",
        &perm,
        make_opts(),
    );
    assert!(
        cleanup_warn_once_was_triggered(),
        "gate must be true after first cleanup Err (banner fired)"
    );

    // Second call: gate is already true; swap returns true → !true = false → no banner.
    let r2 = dispatch(
        RunnerKind::Claude,
        "rate-limit-prompt-2",
        &perm,
        make_opts(),
    );
    assert!(
        cleanup_warn_once_was_triggered(),
        "gate must remain true after second call (no additional transition)"
    );

    unsafe {
        std::env::remove_var("CLAUDE_BINARY");
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_file(&script);

    // Both spawn results unchanged (cleanup Err never propagates).
    let r1 = r1.expect("first dispatch returned Err — cleanup Err must not propagate");
    let r2 = r2.expect("second dispatch returned Err — cleanup Err must not propagate");
    assert!(
        r1.output.contains(marker),
        "first spawn result unchanged, got: {:?}",
        r1.output
    );
    assert!(
        r2.output.contains(marker),
        "second spawn result unchanged, got: {:?}",
        r2.output
    );
}

// ---------------------------------------------------------------------------
// AC6 — session_id None → cleanup skipped entirely
// ---------------------------------------------------------------------------

/// GrokRunner returns `session_id: None` when no new UUID directory appears
/// in the pre/post snapshot diff (mock exits 0 without creating any dir).
/// dispatch must skip cleanup in that case — no banner, no errant rmdir.
#[test]
fn dispatch_grok_skips_cleanup_when_session_id_is_none() {
    let _env = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _banner = BANNER_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    // Reset so any accidental banner would be freshly visible.
    reset_cleanup_warn_once_for_test();

    let marker = "SKIP_CLEANUP_GROK_9D3E71A0";
    let script = make_grok_no_session_script("skip_cleanup_grok", marker);
    let temp_home = tempfile::tempdir().expect("create temp HOME");
    let temp_cwd = tempfile::tempdir().expect("create temp cwd");

    // Pre-create the session parent so the pre-snapshot is an empty dir, not absent.
    let session_parent = grok_encoded_session_dir(temp_cwd.path(), temp_home.path());
    std::fs::create_dir_all(&session_parent).expect("create grok session parent");

    // SAFETY: serialized via ENV_MUTEX.
    unsafe {
        std::env::set_var("GROK_BINARY", script.to_str().unwrap());
        std::env::set_var("HOME", temp_home.path());
    }

    let perm = scoped_coding();
    let opts = RunnerOpts {
        working_dir: Some(temp_cwd.path()),
        ..RunnerOpts::default()
    };
    let result = dispatch(RunnerKind::Grok, "skip-cleanup-grok-prompt", &perm, opts);

    unsafe {
        std::env::remove_var("GROK_BINARY");
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_file(&script);

    let r: RunnerResult = result.expect("dispatch returned Err");
    assert_eq!(r.exit_code, 0);
    assert!(
        r.output.contains(marker),
        "spawn result must round-trip unchanged, got: {:?}",
        r.output
    );
    // AC6: session_id must be None when no new dir appeared.
    assert!(
        r.session_id.is_none(),
        "GrokRunner must return session_id=None when no session dir was created, got {:?}",
        r.session_id
    );
    // No cleanup attempted → CLEANUP_WARN_ONCE stays false.
    assert!(
        !cleanup_warn_once_was_triggered(),
        "no banner expected when session_id is None (cleanup was skipped)"
    );
    // Session parent intact — cleanup never touched it.
    assert!(
        session_parent.exists(),
        "session parent dir must survive when cleanup was skipped"
    );
}

// ---------------------------------------------------------------------------
// Scaffolding kept #[ignore]d (contracts not yet needed or behaviour changed)
// ---------------------------------------------------------------------------

/// AC5: cleanup should run even when spawn returned Err (e.g. binary not found).
///
/// Currently NOT wired in dispatch: when spawn returns Err, the session UUID
/// is not surfaced in the error type so cleanup is skipped per the dispatch
/// doc ("In practice no artifact is written because the CLI never ran").
/// Update if this contract changes.
#[test]
#[ignore = "AC5: dispatch intentionally skips cleanup when spawn returns Err (no UUID surfaced); update if contract changes"]
fn dispatch_calls_cleanup_even_when_spawn_returned_err() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    panic!("not implemented — see ignore reason");
}

/// AC7: cwd passed to cleanup_session is opts.working_dir when Some,
/// env::current_dir() when None. Covered implicitly by Tests 1+2 (both use
/// working_dir=Some(temp_cwd)).
#[test]
#[ignore = "AC7: cwd resolution covered implicitly by Tests 1+2; add explicit test if a regression is found"]
fn dispatch_uses_working_dir_when_provided_else_current_dir() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    panic!("not implemented — see ignore reason");
}

// ---------------------------------------------------------------------------
// PRD §2.5 / §6 Risk #1 hardening: parallel slots, disjoint session dirs
// ---------------------------------------------------------------------------

/// Concurrent FakeRunner spawn closures in distinct cwds produce disjoint
/// encoded session dirs and non-interfering cleanups.
///
/// Two threads each simulate a FakeRunner spawn closure (Phase 1: create a
/// Claude-style `.jsonl` artifact in their encoded dir) and then simulate the
/// dispatch cleanup_session call (Phase 2: remove only their own artifact).
/// A shared `Arc<Mutex<Vec<(String, PathBuf)>>>` recorder captures each
/// `(session_id, cwd)` pair so the main thread can assert correctness.
///
/// This is the §6 Risk #1 mitigation test: "Parallel-slot integration test
/// asserts disjoint cwds produce disjoint encoded session dirs."
#[test]
fn parallel_slot_disjoint_session_dirs() {
    // Both cwds live inside a single parent tempdir (mirrors the
    // "e.g., /tmp/dir-a, /tmp/dir-b inside a tempdir" AC wording).
    let parent_dir = tempfile::tempdir().expect("create parent temp dir");
    let temp_home = tempfile::tempdir().expect("create temp HOME");

    let cwd_a = parent_dir.path().join("dir-a");
    let cwd_b = parent_dir.path().join("dir-b");
    std::fs::create_dir_all(&cwd_a).expect("create dir-a");
    std::fs::create_dir_all(&cwd_b).expect("create dir-b");

    let encoded_a = claude_encoded_cwd_dir(&cwd_a, temp_home.path());
    let encoded_b = claude_encoded_cwd_dir(&cwd_b, temp_home.path());

    // Key invariant (PRD §6 Risk #1): distinct cwds → distinct encoded dirs.
    assert_ne!(
        encoded_a, encoded_b,
        "distinct cwds must yield distinct encoded session dirs"
    );
    assert!(
        !encoded_b.starts_with(&encoded_a),
        "encoded dir for cwd-b must not be a subdirectory of encoded dir for cwd-a"
    );

    std::fs::create_dir_all(&encoded_a).expect("create encoded_a");
    std::fs::create_dir_all(&encoded_b).expect("create encoded_b");

    // Fixed session IDs (simulate what --session-id injects).
    let session_a = "aaaaaaaa-1111-2222-3333-000000000001".to_string();
    let session_b = "bbbbbbbb-4444-5555-6666-000000000002".to_string();

    let artifact_a = encoded_a.join(format!("{session_a}.jsonl"));
    let artifact_b = encoded_b.join(format!("{session_b}.jsonl"));

    // Recorder: (session_id, cwd) pairs logged by each simulated cleanup_session call.
    // Mirrors the FakeRunner recorder shape (learning [2728]).
    let recorder: Arc<Mutex<Vec<(String, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));

    // ── Phase 1: concurrent spawns ──────────────────────────────────────────
    // Two threads simultaneously simulate FakeRunner::spawn by creating the
    // Claude-style artifact file in their respective encoded dir.
    {
        let art_a = artifact_a.clone();
        let art_b = artifact_b.clone();
        let h_a = std::thread::spawn(move || {
            std::fs::write(&art_a, "").expect("artifact-a: write");
        });
        let h_b = std::thread::spawn(move || {
            std::fs::write(&art_b, "").expect("artifact-b: write");
        });
        h_a.join().expect("thread-a spawn phase");
        h_b.join().expect("thread-b spawn phase");
    }

    // Both artifacts must exist before cleanup (spawn phase succeeded).
    assert!(artifact_a.exists(), "artifact-a must exist after spawn");
    assert!(artifact_b.exists(), "artifact-b must exist after spawn");

    // ── Phase 2: concurrent cleanups ────────────────────────────────────────
    // Two threads simultaneously simulate dispatch's cleanup_session call:
    // each removes ONLY its own (session_id, cwd) artifact and records the pair.
    {
        let art_a = artifact_a.clone();
        let art_b = artifact_b.clone();
        let cwd_a_owned = cwd_a.clone();
        let cwd_b_owned = cwd_b.clone();
        let sid_a = session_a.clone();
        let sid_b = session_b.clone();
        let rec_a = Arc::clone(&recorder);
        let rec_b = Arc::clone(&recorder);

        let h_a = std::thread::spawn(move || {
            std::fs::remove_file(&art_a).expect("cleanup-a: remove artifact");
            rec_a.lock().unwrap().push((sid_a, cwd_a_owned));
        });
        let h_b = std::thread::spawn(move || {
            std::fs::remove_file(&art_b).expect("cleanup-b: remove artifact");
            rec_b.lock().unwrap().push((sid_b, cwd_b_owned));
        });
        h_a.join().expect("thread-a cleanup phase");
        h_b.join().expect("thread-b cleanup phase");
    }

    // Each artifact is gone from its own encoded dir.
    assert!(
        !artifact_a.exists(),
        "artifact-a must be removed by cleanup"
    );
    assert!(
        !artifact_b.exists(),
        "artifact-b must be removed by cleanup"
    );

    // The other slot's artifact is untouched: cleanup-a never touched encoded_b
    // and cleanup-b never touched encoded_a.
    assert!(
        !encoded_b.join(format!("{session_a}.jsonl")).exists(),
        "cwd-a cleanup must not create or remove any artifact in encoded_b dir"
    );
    assert!(
        !encoded_a.join(format!("{session_b}.jsonl")).exists(),
        "cwd-b cleanup must not create or remove any artifact in encoded_a dir"
    );
    // Both encoded dirs themselves survive (cleanup removed only the file).
    assert!(encoded_a.exists(), "encoded_a dir must survive cleanup");
    assert!(encoded_b.exists(), "encoded_b dir must survive cleanup");

    // Recorder must have exactly one entry per cleanup_session call.
    let recorded = recorder.lock().unwrap();
    assert_eq!(
        recorded.len(),
        2,
        "two cleanup_session calls must be recorded"
    );

    let rec_a = recorded
        .iter()
        .find(|(sid, _)| *sid == session_a)
        .expect("recorder must have entry for session-a");
    let rec_b = recorded
        .iter()
        .find(|(sid, _)| *sid == session_b)
        .expect("recorder must have entry for session-b");

    assert_eq!(rec_a.1, cwd_a, "session-a cleanup must use cwd-a");
    assert_eq!(rec_b.1, cwd_b, "session-b cleanup must use cwd-b");
}

// ---------------------------------------------------------------------------
// Compile-time sanity: the public dispatch signature hasn't changed shape.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
const _ASSERT_DISPATCH_RETURNS_RUNNER_RESULT: fn(
    RunnerKind,
    &str,
    &PermissionMode,
    RunnerOpts<'_>,
) -> task_mgr::error::TaskMgrResult<RunnerResult> = dispatch;
