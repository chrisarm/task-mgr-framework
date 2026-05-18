//! LLM runner abstraction.
//!
//! This module provides the trait-object-free abstraction over LLM CLI
//! subprocesses (Claude, Grok, …). Static `enum RunnerKind` dispatch keeps
//! allocation-free behavior and forces exhaustive-match on every variant.
//!
//! v0 (this TDD scaffolding): only `RunnerKind::Claude` is wired through to
//! `claude::spawn_claude`. `RunnerKind::Grok` returns an `Unsupported` error
//! until FEAT-003 lands the `GrokRunner` impl. The public type aliases
//! `RunnerOpts` / `RunnerResult` exist so call-site refactors can use the
//! provider-neutral names while we keep `SpawnOpts` / `ClaudeResult` as
//! ABI-compatible aliases pointing back here.

use crate::error::TaskMgrResult;
use crate::loop_engine::claude::{self, ClaudeResult, SpawnOpts};
use crate::loop_engine::config::PermissionMode;

/// Which LLM CLI to invoke.
///
/// Static-dispatch enum (no `Box<dyn LlmRunner>`); every dispatch site is
/// forced to handle every variant by exhaustive match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunnerKind {
    Claude,
    Grok,
}

/// Options for a runner invocation.
///
/// Type-alias of the existing `SpawnOpts` so all current `spawn_claude`
/// call sites remain compilable; new code prefers the provider-neutral name.
pub type RunnerOpts<'a> = SpawnOpts<'a>;

/// Result of a runner invocation.
///
/// Type-alias of the existing `ClaudeResult`. The shape is provider-neutral
/// already (exit_code / output / conversation / timed_out / completion_killed
/// / permission_denials), so a Grok backend simply populates the same fields.
pub type RunnerResult = ClaudeResult;

/// Route a runner invocation to the correct backend.
///
/// `RunnerKind::Claude` → `claude::spawn_claude` (existing path, byte-identical
/// behavior). `RunnerKind::Grok` → `unimplemented!()` until FEAT-003 lands the
/// `GrokRunner` impl; v0 callers must avoid that variant.
///
/// # Errors
///
/// Returns whatever the underlying backend returns.
///
/// # Panics
///
/// Panics with `unimplemented!()` if invoked with `RunnerKind::Grok` before
/// FEAT-003.
pub fn dispatch(
    kind: RunnerKind,
    prompt: &str,
    permission_mode: &PermissionMode,
    opts: RunnerOpts<'_>,
) -> TaskMgrResult<RunnerResult> {
    match kind {
        RunnerKind::Claude => claude::spawn_claude(prompt, permission_mode, opts),
        RunnerKind::Grok => unimplemented!(
            "RunnerKind::Grok dispatch not implemented (FEAT-003 will land GrokRunner)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::config::CODING_ALLOWED_TOOLS;
    use crate::loop_engine::test_utils::CLAUDE_BINARY_MUTEX;

    /// Compile-only assertion: `SpawnOpts` and `RunnerOpts` are the same
    /// type. If FEAT-001 ever swaps `SpawnOpts` for a non-aliased newtype,
    /// this fails to compile and the parity contract is broken loudly.
    #[allow(dead_code)]
    fn _assert_spawn_opts_is_runner_opts(opts: SpawnOpts<'_>) -> RunnerOpts<'_> {
        opts
    }

    /// Compile-only assertion: `ClaudeResult` and `RunnerResult` are the same
    /// type. Same rationale as above.
    #[allow(dead_code)]
    fn _assert_claude_result_is_runner_result(r: ClaudeResult) -> RunnerResult {
        r
    }

    fn scoped_coding() -> PermissionMode {
        PermissionMode::Scoped {
            allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
        }
    }

    /// Mock binary: prints a deterministic marker string + the prompt read
    /// from stdin. The marker lets the test discriminate a real subprocess
    /// invocation from a stub `Ok(default)` — see "Known-bad discriminator"
    /// in the AC.
    fn make_marker_script(name: &str, marker: &str) -> std::path::PathBuf {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let path = std::env::temp_dir().join(format!("task_mgr_test_{name}_marker.sh"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
            writeln!(f, r#"echo "{marker} $PROMPT""#).unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// AC: dispatch(RunnerKind::Claude, ...) routes through to the Claude
    /// binary, and the returned RunnerResult contains the echoed marker
    /// string. Mirrors `claude::tests::spawn_claude_echo` shape.
    ///
    /// Known-bad discriminator: a stub `dispatch` that returns
    /// `Ok(RunnerResult::default())` would NOT contain the marker — so the
    /// `contains(marker)` assertion fails loudly if dispatch ever stops
    /// running the subprocess.
    #[test]
    fn dispatch_claude_runs_subprocess_and_returns_echoed_output() {
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let marker = "DISPATCH_CLAUDE_MARKER_5BA153A7";
        let script = make_marker_script("dispatch_claude", marker);
        unsafe { std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap()) };

        let perm = scoped_coding();
        let result = dispatch(
            RunnerKind::Claude,
            "hello-from-dispatch",
            &perm,
            RunnerOpts::default(),
        );

        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        let _ = std::fs::remove_file(&script);

        let r = result.expect("dispatch returned Err");
        assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
        assert!(
            r.output.contains(marker),
            "expected output to contain {marker:?}, got {:?}",
            r.output,
        );
        assert!(
            r.output.contains("hello-from-dispatch"),
            "expected output to contain the piped prompt, got {:?}",
            r.output,
        );
    }

    /// v0 behavior pin: dispatch(RunnerKind::Grok, ...) panics with
    /// `unimplemented!()` until FEAT-003 lands GrokRunner. This test exists
    /// so a future "always Ok(default())" stub can't silently mask a missing
    /// Grok impl — and so FEAT-003 must update this test (forcing the author
    /// to confirm they replaced the unimplemented! arm).
    #[test]
    #[should_panic(expected = "FEAT-003")]
    fn dispatch_grok_is_unimplemented_until_feat_003() {
        let perm = scoped_coding();
        let _ = dispatch(RunnerKind::Grok, "ignored", &perm, RunnerOpts::default());
    }

    /// AC: existing spawn_claude_echo (claude.rs:1221) compiles and behaves
    /// unchanged after the runner module is introduced. We can't call the
    /// `#[cfg(test)] fn spawn_claude_echo` helper from another module, but
    /// we can verify the same shape works end-to-end via `claude::spawn_claude`
    /// directly — proving the SpawnOpts API surface still exists for the
    /// helper's callers.
    #[test]
    fn spawn_claude_path_unchanged_through_alias() {
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let marker = "SPAWN_CLAUDE_ALIAS_MARKER_5BA153A7";
        let script = make_marker_script("spawn_claude_alias", marker);
        unsafe { std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap()) };

        let perm = scoped_coding();
        // Construct via the legacy `SpawnOpts` name, pass to dispatch via the
        // alias `RunnerOpts` — proves the alias is bidirectional.
        let opts: SpawnOpts<'_> = SpawnOpts::default();
        let result = dispatch(RunnerKind::Claude, "legacy-shape", &perm, opts);

        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        let _ = std::fs::remove_file(&script);

        let r = result.expect("dispatch returned Err");
        assert_eq!(r.exit_code, 0);
        assert!(r.output.contains(marker));
        assert!(r.output.contains("legacy-shape"));
    }
}
