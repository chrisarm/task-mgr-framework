//! TDD scaffolding for FR-002 — `GrokRunner` CLI flag mapping.
//!
//! These tests pin the GrokRunner subprocess contract BEFORE the runner
//! exists. They route through `dispatch(RunnerKind::Grok, ...)`, which
//! currently panics with `unimplemented!()` in
//! `src/loop_engine/runner.rs:553`. FEAT-003 will replace that arm with a
//! real `GrokRunner` impl; tests here are therefore `#[ignore]`'d with a
//! `FEAT-003` reason string. FEAT-003 must un-#[ignore] them and prove the
//! flag mapping in the same commit.
//!
//! ## What the GrokRunner must do (PRD §6 + AC 9-11)
//!
//! 1. **Binary**: invoke `${GROK_BINARY:-grok}`, stdin-piped prompt,
//!    stdout captured to `RunnerResult::output`, stderr inherited. Mirrors
//!    `ClaudeRunner` for the parts that don't differ between CLIs.
//!
//! 2. **`PermissionMode::Dangerous`** emits a single permission-bypass flag.
//!    The choice between `--permission-mode bypassPermissions` and
//!    `--always-approve` is FEAT-003's call; this scaffold pins the test
//!    against `--permission-mode bypassPermissions` (the Claude-side
//!    convention) and the test will need to be flipped if FEAT-003 picks
//!    `--always-approve` instead. The constant
//!    [`EXPECTED_DANGEROUS_FLAG`] is the single edit point.
//!
//! ## Why all of these route through `dispatch` (and not a hypothetical
//! `GrokRunner.spawn` call)
//!
//! Going through `dispatch` proves both the dispatch-table wiring AND the
//! runner body in a single test. A direct call to a hypothetical
//! `GrokRunner.spawn` would compile (after the type exists) but bypass the
//! `match kind` arm — defeating the contract that "every `RunnerKind`
//! variant is reachable through `dispatch`".
//!
//! ## Variant probe (NOT `#[ignore]`'d)
//!
//! [`grok_runner_dispatch_variant_is_reachable`] is the one live test:
//! it calls `dispatch(RunnerKind::Grok, ...)` and asserts the v1
//! `unimplemented!()` panic. If FEAT-003 lands and replaces the arm but
//! forgets to un-#[ignore] the contract tests above, this test starts
//! FAILING (no panic) and surfaces the missed step. FEAT-003 must
//! delete or invert this test when it lands the real impl.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Mutex;

use task_mgr::loop_engine::config::{CODING_ALLOWED_TOOLS, PermissionMode};
use task_mgr::loop_engine::runner::{RunnerKind, RunnerOpts, dispatch};

/// Serialize `GROK_BINARY` env-var mutations within this integration-test
/// binary. Independent of the `CLAUDE_BINARY` mutex elsewhere; both lock
/// only within their own process. Tests here do not touch `CLAUDE_BINARY`,
/// so no cross-binary mutex coordination is needed.
static GROK_BINARY_MUTEX: Mutex<()> = Mutex::new(());

/// Drop guard that removes a temporary script file on scope exit so test
/// panics don't leak files in temp dir.
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

/// The permission-bypass flag the GrokRunner must emit for
/// `PermissionMode::Dangerous`. Pinned to one option; FEAT-003 may switch
/// to `--always-approve` if that matches the grok-cli convention better —
/// the change is a one-line edit here plus an un-#[ignore] of the related
/// test, with no ambiguity propagating into downstream call sites.
const EXPECTED_DANGEROUS_FLAG: &str = "--permission-mode";
const EXPECTED_DANGEROUS_VALUE: &str = "bypassPermissions";

fn scoped_coding() -> PermissionMode {
    PermissionMode::Scoped {
        allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
    }
}

/// Mock grok binary: echoes the marker string + every argv element on its
/// own stdout line, then echoes the prompt read from stdin. The marker is
/// the known-bad discriminator — a stub `dispatch(Grok, ...)` returning
/// `Ok(default())` would produce an empty `output` field and fail the
/// `contains(marker)` assertion.
///
/// Output shape (one per line):
/// ```text
/// MARKER
/// argv: --some-flag
/// argv: some-value
/// argv: -p
/// PROMPT: <stdin contents>
/// ```
///
/// Tests can then assert on substring presence/absence to verify flag
/// mapping without depending on argv order.
fn make_argv_echo_script(name: &str, marker: &str) -> ScriptGuard {
    let path = std::env::temp_dir().join(format!("task_mgr_grok_{name}.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create grok mock");
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, r#"echo "{marker}""#).unwrap();
        writeln!(f, r#"for a in "$@"; do echo "argv: $a"; done"#).unwrap();
        writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
        writeln!(f, r#"echo "PROMPT: $PROMPT""#).unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod grok mock");
    ScriptGuard(path)
}

/// Run `dispatch(RunnerKind::Grok, ...)` with `GROK_BINARY` pointed at the
/// given script, then restore env state and remove the script. Caller owns
/// the `opts` so they can vary per-test.
fn dispatch_grok_with_mock(
    script: &std::path::Path,
    prompt: &str,
    perm: &PermissionMode,
    opts: RunnerOpts<'_>,
) -> task_mgr::error::TaskMgrResult<task_mgr::loop_engine::runner::RunnerResult> {
    let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: env mutation is process-global; serialized via GROK_BINARY_MUTEX.
    unsafe { std::env::set_var("GROK_BINARY", script) };
    let result = dispatch(RunnerKind::Grok, prompt, perm, opts);
    unsafe { std::env::remove_var("GROK_BINARY") };
    result
}

// ── AC 9: GrokRunner runs the subprocess and returns echoed stdout ────────────

/// AC 9: dispatch(Grok) runs the binary at `GROK_BINARY` and returns its
/// stdout in `RunnerResult::output`. The marker line is the known-bad
/// discriminator — a future stub that returns `Ok(default())` produces no
/// marker and the assertion fails loudly.
#[test]
fn grok_runner_returns_echoed_stdout_via_mock_binary() {
    let marker = "GROK_RUNNER_ECHO_MARKER_5BA153A7";
    let script = make_argv_echo_script("echo_stdout", marker);
    let perm = scoped_coding();

    let result = dispatch_grok_with_mock(&script, "hello-grok", &perm, RunnerOpts::default());
    // script is auto-removed by ScriptGuard on drop; no explicit remove_file needed.
    let r = result.expect("dispatch(Grok) returned Err");
    assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
    assert!(
        r.output.contains(marker),
        "known-bad discriminator: expected marker {marker:?} in output, got {:?}",
        r.output,
    );
    assert!(
        r.output.contains("PROMPT: hello-grok"),
        "expected piped prompt to round-trip into mock stdout, got {:?}",
        r.output,
    );
}

// ── AC 11: PermissionMode::Dangerous emits the documented bypass flag ─────────

/// AC 11: `PermissionMode::Dangerous` produces a single permission-bypass
/// pair on the argv. Pinned today to `--permission-mode bypassPermissions`
/// via [`EXPECTED_DANGEROUS_FLAG`] / [`EXPECTED_DANGEROUS_VALUE`]; FEAT-003
/// may flip to `--always-approve` (one-line edit + un-#[ignore] of this
/// test in the same commit).
///
/// Known-bad discriminator: an implementation that emits BOTH bypass styles
/// (or neither) fails the exact-string assertion. An implementation that
/// silently swallows `Dangerous` (treating it as `Scoped`) lacks the flag
/// pair entirely.
#[test]
fn grok_runner_dangerous_permission_mode_emits_bypass_flag() {
    let marker = "GROK_RUNNER_DANGEROUS_MARKER_5BA153A7";
    let script = make_argv_echo_script("dangerous", marker);
    let perm = PermissionMode::Dangerous;

    let result = dispatch_grok_with_mock(&script, "dangerous-probe", &perm, RunnerOpts::default());
    // script auto-removed by ScriptGuard
    let r = result.expect("dispatch(Grok) returned Err");
    assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
    let want_flag = format!("argv: {EXPECTED_DANGEROUS_FLAG}");
    let want_value = format!("argv: {EXPECTED_DANGEROUS_VALUE}");
    assert!(
        r.output.contains(&want_flag),
        "expected {EXPECTED_DANGEROUS_FLAG:?} in argv, got:\n{}",
        r.output,
    );
    assert!(
        r.output.contains(&want_value),
        "expected {EXPECTED_DANGEROUS_VALUE:?} in argv, got:\n{}",
        r.output,
    );
}

// ── v1 behavior pin: dispatch(Grok) reaches a real GrokRunner ─────────────────

/// FEAT-003 landed: `dispatch(RunnerKind::Grok, ...)` now reaches a real
/// `GrokRunner` impl. The test points `GROK_BINARY` at a mock script so the
/// run does not depend on a real grok install — proving the dispatch arm
/// no longer panics and the runner body actually executes the resolved
/// binary. The inverted shape is the post-FEAT-003 counterpart of the
/// pre-FEAT-003 `#[should_panic]` guard.
#[test]
fn grok_runner_dispatch_variant_is_reachable() {
    let marker = "GROK_RUNNER_REACHABLE_MARKER_5BA153A7";
    let script = make_argv_echo_script("reachable", marker);
    let perm = scoped_coding();
    let result = dispatch_grok_with_mock(&script, "reachable-probe", &perm, RunnerOpts::default());
    // script auto-removed by ScriptGuard
    let r = result.expect("dispatch(Grok) returned Err — runner body not reached");
    assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
    assert!(
        r.output.contains(marker),
        "expected marker {marker:?} in stdout (runner body must have spawned the binary), got {:?}",
        r.output,
    );
}

/// AC 12: compile-marker — runs every `cargo test --test grok_runner_unit`
/// invocation so a build break surfaces as a missing-test signal rather
/// than getting silently grouped with the `#[ignore]`'d tests.
#[test]
fn grok_runner_test_file_compiles() {
    // Touching the symbols the rest of the file depends on is enough.
    let _opts = RunnerOpts::default();
    assert_eq!(EXPECTED_DANGEROUS_FLAG, "--permission-mode");
}

// ── Replacement for the deleted silent-ignore test ────────────────────────────

/// Serialize HOME mutations within this integration-test binary. HOME is
/// process-global; concurrent mutation across tests in this file would race.
/// Lock order with [`GROK_BINARY_MUTEX`]: HOME first, then GROK_BINARY.
static HOME_MUTEX: Mutex<()> = Mutex::new(());

/// Compute the Grok session dir mirroring
/// `runner::grok_encoded_session_dir` (which is `pub(crate)` and so not
/// reachable from integration tests). The encoding is well-defined: trim
/// trailing slash, percent-encode, join under `<HOME>/.grok/sessions/`.
fn expected_grok_session_dir(cwd: &std::path::Path, home: &std::path::Path) -> std::path::PathBuf {
    let cwd_str = cwd.to_string_lossy();
    let trimmed = cwd_str.trim_end_matches('/');
    let encoded = urlencoding::encode(trimmed).into_owned();
    home.join(".grok").join("sessions").join(encoded)
}

/// Mock grok that simulates Grok CLI persistence: creates a fixed UUID
/// subdir under `$GROK_TEST_SESSION_DIR/` with a session.json file, then
/// exits 0. Used by the post-spawn cleanup integration test below.
fn make_session_creating_script(name: &str, uuid_str: &str) -> ScriptGuard {
    let path = std::env::temp_dir().join(format!("task_mgr_grok_{name}.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create grok mock");
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, r#"mkdir -p "$GROK_TEST_SESSION_DIR/{uuid_str}""#).unwrap();
        writeln!(
            f,
            r#"echo '{{}}' > "$GROK_TEST_SESSION_DIR/{uuid_str}/session.json""#
        )
        .unwrap();
        writeln!(f, r#"cat > /dev/null"#).unwrap();
        writeln!(f, "exit 0").unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod grok mock");
    ScriptGuard(path)
}

/// End-to-end verification that `dispatch(RunnerKind::Grok, ...)` invokes
/// `GrokRunner::cleanup_session` after the child exits, and that the
/// implementation removes the per-session directory at
/// `<HOME>/.grok/sessions/<encoded-cwd>/<uuid>/`. Cleanup is the runner's
/// responsibility (FEAT-002 trait + FEAT-005 impl + FEAT-006 dispatch
/// wiring) — no per-call opt-in flag.
#[test]
fn grok_runner_cleanup_session_deletes_session_directory() {
    let _home_guard = HOME_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _bin_guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_home = tmp.path().to_path_buf();
    let fake_cwd = fake_home.join("workspace");
    std::fs::create_dir_all(&fake_cwd).unwrap();

    let session_dir = expected_grok_session_dir(&fake_cwd, &fake_home);
    std::fs::create_dir_all(&session_dir).unwrap();

    let uuid_str = "11111111-2222-4333-8444-555555555555";
    let target_subdir = session_dir.join(uuid_str);

    let script = make_session_creating_script("cleanup_session_e2e", uuid_str);

    let prior_home = std::env::var_os("HOME");
    let prior_session = std::env::var_os("GROK_TEST_SESSION_DIR");
    // SAFETY: env mutations are process-global; serialized via HOME_MUTEX
    // and GROK_BINARY_MUTEX (acquired above in that order).
    unsafe {
        std::env::set_var("HOME", &fake_home);
        std::env::set_var("GROK_TEST_SESSION_DIR", &session_dir);
        std::env::set_var("GROK_BINARY", script.as_os_str());
    }

    let perm = scoped_coding();
    let result = task_mgr::loop_engine::runner::dispatch(
        RunnerKind::Grok,
        "cleanup-session-probe",
        &perm,
        RunnerOpts {
            working_dir: Some(&fake_cwd),
            ..RunnerOpts::default()
        },
    );

    // Restore env before any assertions so a panic doesn't leak state.
    unsafe {
        std::env::remove_var("GROK_BINARY");
        match prior_session {
            Some(v) => std::env::set_var("GROK_TEST_SESSION_DIR", v),
            None => std::env::remove_var("GROK_TEST_SESSION_DIR"),
        }
        match prior_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    let r = result.expect("dispatch(Grok) returned Err");
    assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
    // FEAT-004: pre/post dir diff captured the new UUID.
    assert!(
        r.session_id.is_some(),
        "expected session_id to be captured from pre/post dir diff, got None",
    );
    // FEAT-005 + FEAT-006: dispatch invoked cleanup_session, which removed
    // the per-session directory.
    assert!(
        !target_subdir.exists(),
        "session subdir {:?} must be removed by post-spawn cleanup",
        target_subdir,
    );
    // Defense-in-depth: the parent session dir must survive (cleanup must
    // never widen to delete prompt_history.jsonl's parent).
    assert!(
        session_dir.exists(),
        "encoded-cwd parent dir {:?} must NOT be removed (prompt_history.jsonl lives here)",
        session_dir,
    );
}
