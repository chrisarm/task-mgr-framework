//! Integration tests for `loop_engine::runner::dispatch`.
//!
//! Mirrors `claude::tests::spawn_claude_echo` shape (claude.rs:1221) but
//! routes through the new public `dispatch` API instead of calling
//! `spawn_claude` directly. Confirms:
//!
//! 1. `dispatch(RunnerKind::Claude, ...)` runs the binary pointed at by
//!    `CLAUDE_BINARY` and returns its stdout in the `RunnerResult`.
//! 2. Known-bad discriminator — the test asserts on a specific marker
//!    emitted by the mock binary, so a future stub `dispatch` that returns
//!    `Ok(RunnerResult::default())` would FAIL (no marker in output).
//! 3. The legacy `SpawnOpts` / `ClaudeResult` names continue to work as
//!    type aliases for `RunnerOpts` / `RunnerResult` — exercised at compile
//!    time via the explicit type-annotated bindings in
//!    `dispatch_claude_with_alias_names_compiles_and_runs`.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Mutex;
use task_mgr::loop_engine::claude::{ClaudeResult, SpawnOpts};
use task_mgr::loop_engine::config::{CODING_ALLOWED_TOOLS, PermissionMode};
use task_mgr::loop_engine::runner::{RunnerKind, RunnerOpts, RunnerResult, dispatch};

/// Serialize tests that mutate `CLAUDE_BINARY`. Integration tests run in
/// their own binary, so this mutex is independent of the unit-test
/// `CLAUDE_BINARY_MUTEX` in `src/loop_engine/test_utils.rs` — both serialize
/// within their own process.
static CLAUDE_BINARY_MUTEX: Mutex<()> = Mutex::new(());

fn scoped_coding() -> PermissionMode {
    PermissionMode::Scoped {
        allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
    }
}

/// Create a mock CLI script that prints a deterministic marker line +
/// the prompt read from stdin. The marker is what makes this a "known-bad
/// discriminator" — a stub `dispatch` returning `Ok(default())` would
/// produce an empty `output` field, failing the `contains(marker)` check.
fn make_marker_script(name: &str, marker: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("task_mgr_it_{name}_marker.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create mock script");
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
        writeln!(f, r#"echo "{marker} $PROMPT""#).unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod mock script");
    path
}

/// AC 1 + 2: dispatch(Claude) runs subprocess, returns echoed stdout.
/// Known-bad discriminator: assertion on the marker string fails if
/// dispatch ever stops spawning the underlying subprocess.
#[test]
fn dispatch_claude_runs_subprocess_and_returns_echoed_stdout() {
    let _guard = CLAUDE_BINARY_MUTEX
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let marker = "IT_DISPATCH_CLAUDE_5BA153A7";
    let script = make_marker_script("dispatch_claude", marker);
    // SAFETY: process-global mutation, serialized via CLAUDE_BINARY_MUTEX.
    unsafe { std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap()) };

    let perm = scoped_coding();
    let result = dispatch(
        RunnerKind::Claude,
        "integration-prompt",
        &perm,
        RunnerOpts::default(),
    );

    unsafe { std::env::remove_var("CLAUDE_BINARY") };
    let _ = std::fs::remove_file(&script);

    let r: RunnerResult = result.expect("dispatch returned Err");
    assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
    assert!(
        r.output.contains(marker),
        "known-bad discriminator failed: expected output to contain {marker:?}, got {:?}",
        r.output,
    );
    assert!(
        r.output.contains("integration-prompt"),
        "expected piped prompt in stdout, got {:?}",
        r.output,
    );
}

/// AC 3: legacy `SpawnOpts` / `ClaudeResult` names are type-aliases of
/// `RunnerOpts` / `RunnerResult`. The explicit type annotations are the
/// compile-only assertion — if FEAT-001 ever breaks the alias chain, this
/// test stops compiling, surfacing the regression at the build boundary
/// rather than at runtime.
#[test]
fn dispatch_claude_with_alias_names_compiles_and_runs() {
    let _guard = CLAUDE_BINARY_MUTEX
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let marker = "IT_ALIAS_CHECK_5BA153A7";
    let script = make_marker_script("alias_check", marker);
    unsafe { std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap()) };

    let perm = scoped_coding();
    // Build with legacy name, pass through to dispatch (which expects
    // RunnerOpts). If these are not the same type, this line does not
    // compile.
    let opts: SpawnOpts<'_> = SpawnOpts::default();
    let result = dispatch(RunnerKind::Claude, "alias-prompt", &perm, opts);

    unsafe { std::env::remove_var("CLAUDE_BINARY") };
    let _ = std::fs::remove_file(&script);

    // Bind under both names to exercise both aliases at the type level.
    let runner_r: RunnerResult = result.expect("dispatch returned Err");
    let legacy_r: ClaudeResult = runner_r;

    assert_eq!(legacy_r.exit_code, 0);
    assert!(legacy_r.output.contains(marker));
    assert!(legacy_r.output.contains("alias-prompt"));
}

/// Compile-only assertion: by-name confirmation that the legacy types
/// resolve to the runner module's types. This is a `const` block so it
/// is evaluated at compile time — no runtime cost, no need for `#[test]`.
#[allow(dead_code)]
const _ASSERT_RUNNER_OPTS_IS_SPAWN_OPTS: fn(SpawnOpts<'_>) -> RunnerOpts<'_> = |opts| opts;
#[allow(dead_code)]
const _ASSERT_RUNNER_RESULT_IS_CLAUDE_RESULT: fn(ClaudeResult) -> RunnerResult = |r| r;
