//! Stub-binary end-to-end tests for `CodexStreamFormat`.
//!
//! These tests exercise the full `dispatch(RunnerKind::Codex, ...)` path
//! against a stub binary controlled by `CODEX_BINARY`. The stub emits
//! realistic `--json` JSONL using the schema confirmed in FEAT-001
//! (codex-cli 0.135.0, 2026-05-31 live transcript):
//!
//! - Per-item kind carried in `item.type` (not `item_type`).
//! - Events: `thread.started` / `turn.started` / `item.started` /
//!   `item.completed` / `turn.completed`.
//! - `agent_message` arrives as one complete block via `item.completed`
//!   with a `text` field.
//! - Errors: `type:"error"` (top-level `message`) or `type:"turn.failed"`
//!   (nested `error.message`).
//!
//! ## Known-bad discriminator
//!
//! Every assertion targets the extracted TEXT in `RunnerResult.output`,
//! not just the exit code — a future stub `dispatch` returning
//! `Ok(RunnerResult { exit_code: 0, ..default() })` would fail because the
//! output would be empty, not the marker string.
//!
//! ## Real-binary test
//!
//! One `#[ignore]`-tagged test at the bottom exercises the real `codex`
//! binary (resolved via `$CODEX_BINARY` or PATH). Opt in with:
//! ```sh
//! cargo test --test codex_runner_integration -- --ignored
//! ```

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Mutex;

use task_mgr::loop_engine::config::{CODING_ALLOWED_TOOLS, PermissionMode};
use task_mgr::loop_engine::runner::{RunnerKind, RunnerOpts, dispatch};

/// Serialize tests that mutate `CODEX_BINARY`. Integration tests run in their
/// own binary, so this mutex is independent of the unit-test mutex in
/// `src/loop_engine/runner.rs`.
static CODEX_BINARY_MUTEX: Mutex<()> = Mutex::new(());

fn scoped_coding() -> PermissionMode {
    PermissionMode::Scoped {
        allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
    }
}

/// Write a chmod+x shell script that drains stdin then emits each JSONL line
/// to stdout via `printf '%s\n'`. No `'` appears in any fixture line, so the
/// single-quote wrapper is shell-safe. Returns the script path for cleanup.
fn make_codex_jsonl_stub(name: &str, jsonl_lines: &[&str]) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("task_mgr_it_codex_{name}.sh"));
    let mut f = std::fs::File::create(&path).expect("create codex stub script");
    writeln!(f, "#!/bin/sh").unwrap();
    // Drain stdin so the prompt-writer thread in CodexRunner::spawn doesn't
    // see EPIPE. The write is best-effort by design, but draining first avoids
    // noisy broken-pipe log lines in test output.
    writeln!(f, "cat > /dev/null").unwrap();
    for line in jsonl_lines {
        writeln!(f, "printf '%s\\n' '{line}'").unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod codex stub script");
    path
}

// ---------------------------------------------------------------------------
// Stub-binary tests (no #[ignore] — run in every `cargo test` invocation)
// ---------------------------------------------------------------------------

/// AC (positive): a stub emitting `agent_message` JSONL drives
/// `dispatch(RunnerKind::Codex)` and `CodexStreamFormat` returns the final
/// `agent_message` text in `RunnerResult.output`.
///
/// Known-bad discriminator: asserts on the MARKER string from the stub's
/// `text` field — a dispatch that returns empty output fails here.
#[test]
fn codex_stub_agent_message_extraction_returns_correct_text() {
    let _guard = CODEX_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let marker = "IT_CODEX_DISPATCH_B09EECB1";
    let agent_msg_line = format!(
        r#"{{"type":"item.completed","item":{{"id":"m0","type":"agent_message","text":"{marker} prompt received"}}}}"#
    );
    let stub = make_codex_jsonl_stub(
        "agent_message",
        &[
            r#"{"type":"thread.started","thread_id":"t001"}"#,
            r#"{"type":"turn.started"}"#,
            &agent_msg_line,
            r#"{"type":"turn.completed","usage":{"input_tokens":5,"output_tokens":3}}"#,
        ],
    );
    // SAFETY: process-global mutation, serialized via CODEX_BINARY_MUTEX.
    unsafe { std::env::set_var("CODEX_BINARY", stub.to_str().unwrap()) };

    let result = dispatch(
        RunnerKind::Codex,
        "integration-test-prompt",
        &scoped_coding(),
        RunnerOpts {
            stream_json: true,
            ..RunnerOpts::default()
        },
    );

    unsafe { std::env::remove_var("CODEX_BINARY") };
    let _ = std::fs::remove_file(&stub);

    let r = result.expect("dispatch returned Err");
    assert_eq!(
        r.exit_code, 0,
        "expected clean exit; got exit_code={}",
        r.exit_code
    );
    assert!(
        r.output.contains(marker),
        "known-bad discriminator: expected output to contain {marker:?}, got {:?}",
        r.output,
    );
}

/// AC (positive): a stub emitting `turn.failed` is surfaced as an error
/// outcome. `RunnerResult.output` is empty; the conversation transcript
/// contains `[Error: ...]` — the literal prefix the FEAT-003 auth-failure
/// detector scans.
#[test]
fn codex_stub_turn_failed_surfaces_error_in_conversation() {
    let _guard = CODEX_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let stub = make_codex_jsonl_stub(
        "turn_failed",
        &[
            r#"{"type":"thread.started","thread_id":"t001"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"turn.failed","error":{"message":"deliberate test failure"}}"#,
        ],
    );
    unsafe { std::env::set_var("CODEX_BINARY", stub.to_str().unwrap()) };

    let result = dispatch(
        RunnerKind::Codex,
        "turn-failed-test-prompt",
        &scoped_coding(),
        RunnerOpts {
            stream_json: true,
            ..RunnerOpts::default()
        },
    );

    unsafe { std::env::remove_var("CODEX_BINARY") };
    let _ = std::fs::remove_file(&stub);

    let r = result.expect("dispatch returned Err");
    assert!(
        r.output.is_empty(),
        "turn.failed must not produce output text; got {:?}",
        r.output,
    );
    let conv = r
        .conversation
        .expect("stream_json mode must produce a conversation transcript");
    assert!(
        conv.contains("[Error: deliberate test failure]"),
        "turn.failed must appear as [Error: ...] in the conversation transcript; got {:?}",
        conv,
    );
}

/// AC (negative): an unknown event type in the stub output does not break
/// extraction. Unknown lines are silently ignored; the subsequent
/// `agent_message` still produces correct output in `RunnerResult.output`.
#[test]
fn codex_stub_unknown_event_type_does_not_break_extraction() {
    let _guard = CODEX_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let marker = "IT_CODEX_UNKNOWN_EVENT_B09EECB1";
    let agent_msg_line = format!(
        r#"{{"type":"item.completed","item":{{"id":"m0","type":"agent_message","text":"{marker}"}}}}"#
    );
    let stub = make_codex_jsonl_stub(
        "unknown_event",
        &[
            r#"{"type":"thread.started","thread_id":"t001"}"#,
            r#"{"type":"some_future_event","payload":"irrelevant"}"#,
            r#"{"type":"turn.started"}"#,
            &agent_msg_line,
            r#"{"type":"yet_another_future_event","x":1}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1}}"#,
        ],
    );
    unsafe { std::env::set_var("CODEX_BINARY", stub.to_str().unwrap()) };

    let result = dispatch(
        RunnerKind::Codex,
        "unknown-event-test-prompt",
        &scoped_coding(),
        RunnerOpts {
            stream_json: true,
            ..RunnerOpts::default()
        },
    );

    unsafe { std::env::remove_var("CODEX_BINARY") };
    let _ = std::fs::remove_file(&stub);

    let r = result.expect("dispatch must not return Err on unknown event types");
    assert_eq!(r.exit_code, 0);
    assert!(
        r.output.contains(marker),
        "agent_message must be extracted even when surrounded by unknown events; got {:?}",
        r.output,
    );
}

// ---------------------------------------------------------------------------
// Compile-only pin (runs in every `cargo test` invocation)
// ---------------------------------------------------------------------------

/// Compile-time pin: the test file builds and the helper produces a
/// non-empty, executable script file. Runs on every
/// `cargo test --test codex_runner_integration` so a build break surfaces
/// even without a real codex binary.
#[test]
fn codex_runner_integration_test_file_compiles_and_helpers_work() {
    let stub = make_codex_jsonl_stub(
        "compile_pin",
        &[r#"{"type":"turn.completed","usage":{"input_tokens":0}}"#],
    );
    assert!(stub.exists(), "stub script must be written to disk");
    let meta = std::fs::metadata(&stub).expect("stub metadata");
    assert!(meta.len() > 0, "stub script must be non-empty");
    let _ = std::fs::remove_file(&stub);
}

// ---------------------------------------------------------------------------
// Real-binary test (requires codex install — #[ignore] for CI)
// ---------------------------------------------------------------------------

/// Exercises dispatch against a real `codex` binary (resolved via
/// `$CODEX_BINARY` or bare `codex` on PATH). Only verifies that dispatch
/// doesn't panic — real model output is non-deterministic.
///
/// Opt in:
/// ```sh
/// cargo test --test codex_runner_integration -- --ignored codex_real_binary_dispatch_does_not_panic
/// ```
#[test]
#[ignore = "requires real codex binary — set CODEX_BINARY or ensure codex is on PATH"]
fn codex_real_binary_dispatch_does_not_panic() {
    let perm = scoped_coding();
    let result = dispatch(
        RunnerKind::Codex,
        "Say exactly one word: PASS",
        &perm,
        RunnerOpts {
            stream_json: true,
            ..RunnerOpts::default()
        },
    );
    // Only assert no panic — real binary output is non-deterministic.
    // Auth failure is an acceptable outcome (no credentials in CI).
    assert!(
        result.is_ok() || matches!(result, Err(task_mgr::TaskMgrError::CodexAuthFailure { .. })),
        "unexpected error from real codex binary: {:?}",
        result.err()
    );
}
