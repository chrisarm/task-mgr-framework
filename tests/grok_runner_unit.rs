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
//! 2. **`cleanup_title_artifact` is silently ignored**: the option exists on
//!    `RunnerOpts` for Claude's ai-title-jsonl workaround (Claude 2.1.110).
//!    Grok has no equivalent artifact, so the GrokRunner must NOT pass
//!    `--session-id` or any related flag — and must NOT fail if the option
//!    is true. Verified by checking that the printed argv from the mock
//!    contains no `--session-id` token even when `cleanup_title_artifact:
//!    true`.
//!
//! 3. **`PermissionMode::Dangerous`** emits a single permission-bypass flag.
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
fn make_argv_echo_script(name: &str, marker: &str) -> std::path::PathBuf {
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
    path
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
#[ignore = "FEAT-003: dispatch(Grok) currently unimplemented!() — un-ignore when GrokRunner lands"]
fn grok_runner_returns_echoed_stdout_via_mock_binary() {
    let marker = "GROK_RUNNER_ECHO_MARKER_5BA153A7";
    let script = make_argv_echo_script("echo_stdout", marker);
    let perm = scoped_coding();

    let result = dispatch_grok_with_mock(&script, "hello-grok", &perm, RunnerOpts::default());

    let _ = std::fs::remove_file(&script);

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

// ── AC 10: cleanup_title_artifact is silently ignored ─────────────────────────

/// AC 10: `cleanup_title_artifact: true` must NOT produce a `--session-id`
/// flag on the grok command line. The option exists on `RunnerOpts` for
/// Claude's ai-title-jsonl workaround; Grok has no equivalent artifact, so
/// the runner must accept the option without complaint and omit the flag.
///
/// Verified by inspecting the echoed argv lines from the mock: no `argv:
/// --session-id` line should appear regardless of the opt-in.
#[test]
#[ignore = "FEAT-003: dispatch(Grok) currently unimplemented!() — un-ignore when GrokRunner lands"]
fn grok_runner_silently_ignores_cleanup_title_artifact() {
    let marker = "GROK_RUNNER_CLEANUP_MARKER_5BA153A7";
    let script = make_argv_echo_script("cleanup_ignored", marker);
    let perm = scoped_coding();

    let result = dispatch_grok_with_mock(
        &script,
        "cleanup-probe",
        &perm,
        RunnerOpts {
            cleanup_title_artifact: true,
            ..RunnerOpts::default()
        },
    );

    let _ = std::fs::remove_file(&script);

    let r = result.expect("dispatch(Grok) returned Err");
    assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
    assert!(
        !r.output.contains("argv: --session-id"),
        "GrokRunner must NOT emit --session-id even when cleanup_title_artifact=true; \
         the option is a Claude-only workaround. Got argv:\n{}",
        r.output,
    );
    // Also confirm the runner didn't silently drop the prompt — it should still
    // execute the binary and round-trip stdin.
    assert!(
        r.output.contains("PROMPT: cleanup-probe"),
        "prompt must still round-trip even when cleanup_title_artifact is set",
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
#[ignore = "FEAT-003: dispatch(Grok) currently unimplemented!() — un-ignore when GrokRunner lands"]
fn grok_runner_dangerous_permission_mode_emits_bypass_flag() {
    let marker = "GROK_RUNNER_DANGEROUS_MARKER_5BA153A7";
    let script = make_argv_echo_script("dangerous", marker);
    let perm = PermissionMode::Dangerous;

    let result = dispatch_grok_with_mock(&script, "dangerous-probe", &perm, RunnerOpts::default());

    let _ = std::fs::remove_file(&script);

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

// ── v1 behavior pin: dispatch(Grok) is unimplemented until FEAT-003 ───────────

/// Until FEAT-003 lands, `dispatch(RunnerKind::Grok, ...)` panics with the
/// documented `unimplemented!("...FEAT-003 will land GrokRunner")` message.
/// This test runs unconditionally and FAILS once FEAT-003 lands — the
/// FEAT-003 author must then either delete this test or invert it (assert
/// `Ok(...)`), in the same commit that un-#[ignore]s the AC 9-11 tests
/// above. The forcing function ensures the un-ignore step is never silently
/// missed.
#[test]
#[should_panic(expected = "FEAT-003")]
fn grok_runner_dispatch_variant_is_reachable() {
    // The mock binary is irrelevant — dispatch panics before any spawn.
    let perm = scoped_coding();
    let _ = dispatch(
        RunnerKind::Grok,
        "this-will-panic-until-feat-003",
        &perm,
        RunnerOpts::default(),
    );
}

/// AC 12: compile-marker — runs every `cargo test --test grok_runner_unit`
/// invocation so a build break surfaces as a missing-test signal rather
/// than getting silently grouped with the `#[ignore]`'d tests.
#[test]
fn grok_runner_test_file_compiles() {
    // Touching the symbols the rest of the file depends on is enough.
    let _opts = RunnerOpts {
        cleanup_title_artifact: true,
        ..RunnerOpts::default()
    };
    assert_eq!(EXPECTED_DANGEROUS_FLAG, "--permission-mode");
}
