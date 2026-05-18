//! TDD scaffolding for FR-007 — Grok auth-failure sniff.
//!
//! These tests pin the contract for `GrokRunner`'s auth-failure detection
//! BEFORE the runner exists. Most are `#[ignore]`'d because they route through
//! `dispatch(RunnerKind::Grok, ...)`, which currently panics with
//! `unimplemented!()` in `src/loop_engine/runner.rs` (FEAT-003 will replace
//! that arm with a real `GrokRunner` impl).
//!
//! The lone live test — `grok_auth_failure_variant_carries_non_empty_hint` —
//! verifies the [`TaskMgrError::GrokAuthFailure`] variant exists and has the
//! expected `{ hint: String }` shape. It's compile-time scaffolding: a future
//! signature change (e.g., dropping `hint`, renaming, or making it
//! non-exhaustive) breaks this test loudly instead of letting a downstream
//! pattern silently fail to match.
//!
//! ## Contract under test
//!
//! `GrokRunner.spawn` must return `Err(TaskMgrError::GrokAuthFailure { hint })`
//! when **both** conditions hold:
//!
//! 1. The grok subprocess exits non-zero **within 3 seconds** of spawn.
//! 2. Its stderr contains one of the well-known auth substrings
//!    (case-insensitive): `not authenticated`, `please run grok login`,
//!    `grok login required`.
//!
//! Either condition alone is **not** sufficient:
//!
//! - Substring + exit 0 → treat as a benign warning, return success.
//! - Substring + non-zero exit after the 3-second window → generic IoError,
//!   because a late substring is more likely a tool-use runtime error than
//!   an auth lapse.
//!
//! The 3-second threshold is the architect's recommendation in PRD §6 FR-007
//! and matches the fast-fail behavior of CLIs that bail before any heavyweight
//! work has started.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Mutex;

use task_mgr::error::TaskMgrError;
use task_mgr::loop_engine::config::{CODING_ALLOWED_TOOLS, PermissionMode};
use task_mgr::loop_engine::runner::{RunnerKind, RunnerOpts, dispatch};

/// Serializes `GROK_BINARY` env-var mutations within this integration-test
/// binary. Independent of the unit-test `CLAUDE_BINARY_MUTEX`; both serialize
/// only within their own process. Tests that also touch `CLAUDE_BINARY` must
/// take an additional lock — none here do.
static GROK_BINARY_MUTEX: Mutex<()> = Mutex::new(());

/// Drop guard that removes a temporary script file on scope exit.
/// Prevents file leaks when tests panic before the explicit remove_file call.
struct ScriptGuard(std::path::PathBuf);

impl std::ops::Deref for ScriptGuard {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for ScriptGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn scoped_coding() -> PermissionMode {
    PermissionMode::Scoped {
        allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
    }
}

/// Build a mock grok CLI script. The script:
///
/// 1. Sleeps for `delay_secs` seconds (integer — portable across BusyBox sh).
/// 2. Writes `stderr_str` to stderr (single line, newline-terminated).
/// 3. Exits with `exit_code`.
///
/// `delay_secs = 0` means "fire immediately" (within the fast-fail window);
/// `delay_secs >= 4` puts the exit comfortably past the 3-second threshold.
/// Returns the absolute path of the executable script.
fn make_grok_mock(name: &str, stderr_str: &str, exit_code: i32, delay_secs: u64) -> ScriptGuard {
    let path = std::env::temp_dir().join(format!("task_mgr_it_grok_{name}.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create mock grok script");
        writeln!(f, "#!/bin/sh").unwrap();
        if delay_secs > 0 {
            writeln!(f, "sleep {delay_secs}").unwrap();
        }
        // Single-quote and escape any embedded single-quotes for /bin/sh.
        let escaped = stderr_str.replace('\'', "'\\''");
        writeln!(f, "printf '%s\\n' '{escaped}' 1>&2").unwrap();
        writeln!(f, "exit {exit_code}").unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod mock grok script");
    ScriptGuard(path)
}

/// Run `dispatch(RunnerKind::Grok, ...)` with `GROK_BINARY` pointed at the
/// given script. Restores env state before returning. Holds the
/// `GROK_BINARY_MUTEX` for the duration. The `ScriptGuard` is auto-cleaned
/// on drop, so callers need not call `remove_file` explicitly.
///
/// `auth_window_secs`: when `Some(n)`, sets `TASK_MGR_GROK_AUTH_WINDOW_SECS`
/// to `n` for the duration of the call so tests can control timing without
/// real sleeps. `None` leaves the env var unset (defaults to 3 s in prod).
fn dispatch_grok_with_mock(
    script: &std::path::Path,
    auth_window_secs: Option<u64>,
) -> task_mgr::error::TaskMgrResult<task_mgr::loop_engine::runner::RunnerResult> {
    let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: env mutation is process-global; serialized via GROK_BINARY_MUTEX.
    unsafe { std::env::set_var("GROK_BINARY", script) };
    if let Some(secs) = auth_window_secs {
        unsafe { std::env::set_var("TASK_MGR_GROK_AUTH_WINDOW_SECS", secs.to_string()) };
    }
    let perm = scoped_coding();
    let result = dispatch(
        RunnerKind::Grok,
        "auth-failure-probe",
        &perm,
        RunnerOpts::default(),
    );
    unsafe { std::env::remove_var("GROK_BINARY") };
    unsafe { std::env::remove_var("TASK_MGR_GROK_AUTH_WINDOW_SECS") };
    result
}

// -----------------------------------------------------------------------------
// AC 1-6: dispatch-routed contract tests. Ignored until FEAT-003 replaces the
// `unimplemented!()` arm in dispatch with a real GrokRunner. The `should_match`
// helpers express the expected shape so a stub `Ok(default())` would fail
// loudly, not silently pass.
// -----------------------------------------------------------------------------

fn assert_is_grok_auth_failure(
    result: &task_mgr::error::TaskMgrResult<task_mgr::loop_engine::runner::RunnerResult>,
) {
    match result {
        Err(TaskMgrError::GrokAuthFailure { hint }) => {
            assert!(
                !hint.is_empty(),
                "GrokAuthFailure must carry a non-empty operator hint"
            );
        }
        Err(other) => panic!("expected GrokAuthFailure, got Err({other:?})"),
        Ok(r) => panic!("expected GrokAuthFailure, got Ok({r:?})"),
    }
}

/// AC 1: stderr `not authenticated` + non-zero exit < 3s → GrokAuthFailure.
#[test]
fn grok_auth_failure_on_not_authenticated_fast_fail() {
    let script = make_grok_mock("not_auth_fast", "Error: not authenticated", 1, 0);
    let result = dispatch_grok_with_mock(&script, None);
    assert_is_grok_auth_failure(&result);
}

/// AC 2: stderr `please run grok login` + non-zero exit < 3s → GrokAuthFailure.
#[test]
fn grok_auth_failure_on_please_run_grok_login_fast_fail() {
    let script = make_grok_mock(
        "please_run_login",
        "auth check failed; please run grok login to continue",
        1,
        0,
    );
    let result = dispatch_grok_with_mock(&script, None);
    assert_is_grok_auth_failure(&result);
}

/// AC 3: stderr `grok login required` + non-zero exit < 3s → GrokAuthFailure.
#[test]
fn grok_auth_failure_on_grok_login_required_fast_fail() {
    let script = make_grok_mock(
        "login_required",
        "401 Unauthorized: grok login required",
        1,
        0,
    );
    let result = dispatch_grok_with_mock(&script, None);
    assert_is_grok_auth_failure(&result);
}

/// AC 4: stderr `NOT AUTHENTICATED` (uppercase) + non-zero exit < 3s →
/// GrokAuthFailure. The sniff MUST be case-insensitive; the architect's
/// rationale is that the grok CLI's wording is not contractually stable.
#[test]
fn grok_auth_failure_is_case_insensitive() {
    let script = make_grok_mock("uppercase", "FATAL: NOT AUTHENTICATED", 1, 0);
    let result = dispatch_grok_with_mock(&script, None);
    assert_is_grok_auth_failure(&result);
}

/// AC 5: stderr `not authenticated` BUT non-zero exit AFTER 3s → generic
/// IoError (or non-auth Err), NOT GrokAuthFailure. The fast-fail timing
/// window is what distinguishes a real auth lapse from a tool-use runtime
/// error that happens to mention auth strings in passing.
///
/// Slow: sleeps ~4 seconds. The integration-test binary skips this by default
/// (it's `#[ignore]`'d for FEAT-003 reasons anyway); CI must run with
/// `--include-ignored` once un-#[ignore]'d. See also
/// `auth_failure_past_env_window_fast` for a deterministic timing-free variant.
#[test]
#[ignore = "slow (>3s sleep) — run with `cargo test -- --ignored` to exercise the timing window"]
fn grok_auth_substring_past_window_is_not_auth_failure() {
    let script = make_grok_mock(
        "past_window",
        "Error: not authenticated",
        1,
        4, // > 3s threshold
    );
    let result = dispatch_grok_with_mock(&script, None);
    // Contract is purely negative: anything BUT GrokAuthFailure is acceptable
    // (generic IoError, unrecognized non-zero exit, etc.) — a late substring
    // is more likely a tool-use runtime error than an auth lapse.
    if let Err(TaskMgrError::GrokAuthFailure { .. }) = result {
        panic!(
            "auth substring past the fast-fail window must NOT be classified \
             as GrokAuthFailure — it is more likely a tool-use runtime error"
        );
    }
}

/// M5 fast case: with `TASK_MGR_GROK_AUTH_WINDOW_SECS=10`, an instant exit
/// (delay=0) is well within the window → GrokAuthFailure. Exercises the env
/// override without depending on real wall-clock timing.
#[test]
fn auth_failure_within_env_window_fast() {
    let script = make_grok_mock("env_window_fast", "Error: not authenticated", 1, 0);
    // 10-second window; instant script is guaranteed within it.
    let result = dispatch_grok_with_mock(&script, Some(10));
    assert_is_grok_auth_failure(&result);
}

/// M5 slow case: with `TASK_MGR_GROK_AUTH_WINDOW_SECS=0`, even an instant
/// exit is past the (zero-length) window → NOT GrokAuthFailure. Exercises
/// the timing-gate without a real 4-second sleep.
#[test]
fn auth_failure_past_env_window_fast() {
    let script = make_grok_mock("env_window_past", "Error: not authenticated", 1, 0);
    // Zero-second window: any positive elapsed time is past the window.
    let result = dispatch_grok_with_mock(&script, Some(0));
    if let Err(TaskMgrError::GrokAuthFailure { .. }) = result {
        panic!(
            "with TASK_MGR_GROK_AUTH_WINDOW_SECS=0 no exit can be within the \
             window — must NOT be classified as GrokAuthFailure"
        );
    }
}

/// AC 6: stderr contains an auth substring AND the subprocess exits 0
/// (warning, not error) → treat as success, NOT GrokAuthFailure. The grok
/// CLI is permitted to print to stderr during normal operation (progress,
/// deprecation warnings, etc.); only the combination of substring + non-zero
/// exit is a credible auth-failure signal.
#[test]
fn grok_auth_substring_with_clean_exit_is_success() {
    let script = make_grok_mock(
        "warning_clean_exit",
        "deprecation: 'not authenticated' is the new name for the auth-required field",
        0,
        0,
    );
    let result = dispatch_grok_with_mock(&script, None);
    match result {
        Err(TaskMgrError::GrokAuthFailure { .. }) => {
            panic!(
                "an auth substring on stderr with a clean exit is a warning, \
                 not an auth failure — must NOT be classified as GrokAuthFailure"
            );
        }
        Ok(r) => {
            assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
        }
        Err(other) => {
            panic!("expected Ok(success) on clean exit, got Err({other:?})");
        }
    }
}

// -----------------------------------------------------------------------------
// AC 7 (LIVE): variant-shape pin. Runs every `cargo test` invocation.
// -----------------------------------------------------------------------------

/// AC 7: [`TaskMgrError::GrokAuthFailure`] is a struct variant with a single
/// `hint: String` field, and constructing it with a non-empty hint preserves
/// the value. This is the contract test FEAT-003's emitter must satisfy: any
/// future change that drops `hint`, renames it, or alters its type breaks
/// compilation here and surfaces the regression at the type-system boundary.
///
/// Not `#[ignore]`'d: this exercises the type itself, not the dispatch path,
/// so it is meaningful even before FEAT-003.
#[test]
fn grok_auth_failure_variant_carries_non_empty_hint() {
    let hint_text = "Run `grok login` to authenticate, then retry the task.";
    let err = TaskMgrError::GrokAuthFailure {
        hint: hint_text.to_string(),
    };

    // Destructure to pin the field name and type (compile-time assertion).
    match &err {
        TaskMgrError::GrokAuthFailure { hint } => {
            assert!(!hint.is_empty(), "hint must be non-empty");
            assert_eq!(hint, hint_text);
        }
        other => panic!("constructed GrokAuthFailure but matched {other:?}"),
    }

    // The Display impl must mention the hint so operators see the
    // remediation text in logs, not just a generic "Grok auth failed".
    let rendered = format!("{err}");
    assert!(
        rendered.contains(hint_text),
        "Display impl must surface the hint to the operator; got: {rendered:?}"
    );
}

/// Compile-only pin: `TaskMgrError::GrokAuthFailure` is reachable from the
/// crate's public `error` module. If FEAT-003 ever moves the variant to a
/// non-public location (or the variant is renamed), this stops compiling.
#[allow(dead_code)]
fn _assert_grok_auth_failure_is_public(hint: String) -> TaskMgrError {
    TaskMgrError::GrokAuthFailure { hint }
}
