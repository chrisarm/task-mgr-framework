//! End-to-end capability contract for `loop_engine::runner::dispatch`.
//!
//! Walks the full (RunnerKind × capability-field) matrix through the public
//! `dispatch` API:
//!
//! - Unsupported pairs MUST return `TaskMgrError::UnsupportedRunnerCapability`
//!   with the matching `runner_kind`, `capability_name`, and `field_name`
//!   BEFORE any subprocess is spawned (fail-closed contract).
//! - Supported pairs MUST NOT return `UnsupportedRunnerCapability` — they
//!   pass the capability gate and proceed to spawn. We point
//!   `CLAUDE_BINARY` / `GROK_BINARY` at a nonexistent path so spawn fails
//!   with a backend-specific error, and the test asserts only that the
//!   returned error is **not** `UnsupportedRunnerCapability`. Subprocess
//!   success is covered by `tests/runner_trait_dispatch.rs`; the contract
//!   under test here is solely the capability gate.
//! - `RunnerOpts::default()` MUST never trigger the gate on either runner.
//!
//! The expected matrix is hand-rolled (not derived from `LlmRunner::supports`)
//! so a copy-paste flip of any bit in either production impl produces a test
//! failure that names the offending pair.

use std::sync::Mutex;

use task_mgr::TaskMgrError;
use task_mgr::loop_engine::config::{CODING_ALLOWED_TOOLS, PermissionMode};
use task_mgr::loop_engine::runner::{RunnerKind, RunnerOpts, dispatch};

static BINARY_MUTEX: Mutex<()> = Mutex::new(());

fn scoped_coding() -> PermissionMode {
    PermissionMode::Scoped {
        allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
    }
}

fn opts_for_field(field: &str) -> RunnerOpts<'static> {
    match field {
        "use_pty" => RunnerOpts {
            use_pty: true,
            ..RunnerOpts::default()
        },
        "stream_json" => RunnerOpts {
            stream_json: true,
            ..RunnerOpts::default()
        },
        "effort" => RunnerOpts {
            effort: Some("high"),
            ..RunnerOpts::default()
        },
        "disallowed_tools" => RunnerOpts {
            disallowed_tools: Some("BashTool"),
            ..RunnerOpts::default()
        },
        "cleanup_title_artifact" => RunnerOpts {
            cleanup_title_artifact: true,
            ..RunnerOpts::default()
        },
        other => panic!("unknown capability field: {other}"),
    }
}

#[test]
fn capability_matrix_dispatch_contract() {
    let _guard = BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    // Point both CLI envs at a nonexistent path. Supported (runner × capability)
    // pairs pass the gate then fail at spawn; unsupported pairs never reach
    // spawn. Either way no subprocess actually runs — keeps the test fast and
    // independent of PTY / stream-json subprocess interaction.
    let bogus_binary = std::env::temp_dir().join("task_mgr_capcontract_does_not_exist_a8c4d2");
    let _ = std::fs::remove_file(&bogus_binary);
    // SAFETY: process-global env mutation, serialized via BINARY_MUTEX.
    unsafe { std::env::set_var("CLAUDE_BINARY", bogus_binary.to_str().unwrap()) };
    unsafe { std::env::set_var("GROK_BINARY", bogus_binary.to_str().unwrap()) };

    // (capability_name, field_name, claude_supports, grok_supports)
    // Mirrors the production support table in
    // src/loop_engine/CLAUDE.md → "Capability surface".
    let matrix: &[(&str, &str, bool, bool)] = &[
        ("Effort", "effort", true, true),
        ("StreamJson", "stream_json", true, true),
        ("Pty", "use_pty", true, false),
        ("DisallowedTools", "disallowed_tools", true, true),
        (
            "TitleArtifactCleanup",
            "cleanup_title_artifact",
            true,
            false,
        ),
    ];

    let perm = scoped_coding();

    for (cap_name, field_name, claude_ok, grok_ok) in matrix {
        for (kind, supported) in [
            (RunnerKind::Claude, *claude_ok),
            (RunnerKind::Grok, *grok_ok),
        ] {
            let opts = opts_for_field(field_name);
            let result = dispatch(kind, "cap-contract-prompt", &perm, opts);

            if supported {
                assert!(
                    !matches!(
                        result,
                        Err(TaskMgrError::UnsupportedRunnerCapability { .. })
                    ),
                    "supported pair ({kind:?}, {field_name}) was rejected by capability gate: {result:?}"
                );
            } else {
                match result {
                    Err(TaskMgrError::UnsupportedRunnerCapability {
                        runner_kind,
                        capability_name,
                        field_name: fname,
                    }) => {
                        assert_eq!(runner_kind, kind);
                        assert_eq!(capability_name, *cap_name);
                        assert_eq!(fname, *field_name);
                    }
                    other => panic!(
                        "expected UnsupportedRunnerCapability for ({kind:?}, {field_name}), got {other:?}"
                    ),
                }
            }
        }
    }

    // RunnerOpts::default() must never trigger the capability gate.
    for kind in [RunnerKind::Claude, RunnerKind::Grok] {
        let result = dispatch(kind, "default-opts", &perm, RunnerOpts::default());
        assert!(
            !matches!(
                result,
                Err(TaskMgrError::UnsupportedRunnerCapability { .. })
            ),
            "RunnerOpts::default() must never trigger capability gate on {kind:?}: {result:?}"
        );
    }

    unsafe { std::env::remove_var("CLAUDE_BINARY") };
    unsafe { std::env::remove_var("GROK_BINARY") };
}
