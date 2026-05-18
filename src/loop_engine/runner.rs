//! LLM runner abstraction.
//!
//! Provides a trait-object-free abstraction over LLM CLI subprocesses
//! (Claude, Grok, …). Static `enum RunnerKind` dispatch keeps allocation-free
//! behavior and forces exhaustive-match on every variant.
//!
//! v1: `RunnerKind::Claude` routes through `ClaudeRunner::spawn`, which holds
//! the full Claude-subprocess body (formerly `claude::spawn_claude`).
//! `RunnerKind::Grok` panics with `unimplemented!` until FEAT-003 lands the
//! `GrokRunner` impl. Legacy `SpawnOpts` / `ClaudeResult` names remain valid as
//! `pub type` aliases in `claude.rs`, so existing call sites compile unchanged.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use uuid::Uuid;

use crate::error::{TaskMgrError, TaskMgrResult};
#[cfg(unix)]
use crate::loop_engine::claude::open_pty_for_child_output;
use crate::loop_engine::claude::{
    ACTIVE_PREFIX_ENV, cleanup_title_artifact_sync, emit_prefixed_lines, is_pty_read_eof,
    tee_stream_json,
};
use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::signals::SignalFlag;
use crate::loop_engine::watchdog::{TimeoutConfig, exit_code_from_status, watchdog_loop};

/// Result of a runner invocation.
///
/// Provider-neutral: every backend populates the same fields (Claude today,
/// Grok in FEAT-003, others later). Kept `pub` because integration tests
/// import the legacy `claude::ClaudeResult` alias which resolves here.
#[derive(Debug)]
pub struct RunnerResult {
    /// Process exit code (0 = success, non-zero = error/crash)
    pub exit_code: i32,
    /// Complete stdout output collected from the process.
    /// In stream-json mode, this is the `result.result` field from the final line.
    /// In plain mode, this is the raw stdout.
    pub output: String,
    /// Formatted conversation transcript (only set in stream-json mode).
    /// Contains assistant text, tool calls, and tool results.
    pub conversation: Option<String>,
    /// Whether the process was killed due to iteration timeout.
    pub timed_out: bool,
    /// Whether the process was killed by the post-completion grace window
    /// elapsing (watchdog saw `<completed>TARGET</completed>` and the
    /// POST_COMPLETION_GRACE_SECS window ran out). This produces a SIGTERM
    /// (exit code 143) that is NOT an external Ctrl+C and must not
    /// propagate to the parent's signal flag.
    pub completion_killed: bool,
    /// Tool calls denied by the permission system during this invocation.
    /// Each entry is a raw JSON value from the stream-json `permission_denials` array.
    pub permission_denials: Vec<serde_json::Value>,
}

/// Optional settings for a runner invocation.
///
/// Every field has a safe default (`None` / `false`), so callers only need to
/// set what's relevant to their use case. `prompt` and `permission_mode`
/// remain required positional args to `dispatch` because they have no
/// meaningful default.
///
/// Example:
/// ```ignore
/// dispatch(RunnerKind::Claude, &prompt, &permission_mode, RunnerOpts {
///     model: Some(HAIKU_MODEL),
///     db_dir: Some(db_dir),
///     cleanup_title_artifact: true,
///     ..RunnerOpts::default()
/// })
/// ```
#[derive(Default)]
pub struct RunnerOpts<'a> {
    /// Watchdog signal flag. When set, a watchdog thread polls this every
    /// 200ms and escalates SIGTERM → 3s grace → SIGKILL on trip.
    pub signal_flag: Option<&'a SignalFlag>,
    /// Working directory for the spawned subprocess. Required when running
    /// inside a git worktree so Claude's sandbox scopes writes correctly.
    pub working_dir: Option<&'a Path>,
    /// `--model` flag value; empty/None omits the flag.
    pub model: Option<&'a str>,
    /// Iteration timeout configuration; `None` disables the timeout thread.
    pub timeout: Option<TimeoutConfig>,
    /// When `true`, use `--verbose --output-format stream-json` instead of
    /// plain mode.
    pub stream_json: bool,
    /// `--effort` flag value; empty/None omits the flag.
    pub effort: Option<&'a str>,
    /// `--disallowedTools` value; empty/None omits the flag.
    pub disallowed_tools: Option<&'a str>,
    /// Canonical task-mgr DB dir to pin via `TASK_MGR_DIR` env.
    pub db_dir: Option<&'a Path>,
    /// When `true`, inject `--session-id <uuid>` before `-p` and synchronously
    /// delete `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` after the
    /// child process exits. Workaround for Claude Code 2.1.110 leaking
    /// ai-title metadata despite `--no-session-persistence`.
    pub cleanup_title_artifact: bool,
    /// When `true` (Unix only), wire the child's stdout+stderr to a
    /// pseudo-TTY slave instead of a regular pipe. Node.js (and therefore
    /// Claude Code) line-buffers stdout only when `isatty(1)` is true, so
    /// a pipe causes block-buffered bursts while a PTY streams per line.
    /// stdin remains a pipe (unchanged prompt delivery + no echo).
    /// Ignored on non-Unix — falls back to piped stdout / inherited stderr.
    pub use_pty: bool,
    /// The task ID this spawn is working on. When `Some`, the stream-json
    /// tee scans assistant text for `<completed>TARGET</completed>` and,
    /// on first match, starts a bounded post-completion grace window
    /// (see `watchdog::POST_COMPLETION_GRACE_SECS`). After the grace
    /// elapses, the watchdog terminates the process even if the agent is
    /// still waiting on background tasks. Only the *current* target's
    /// completion triggers the grace — other `<completed>` tags (e.g.
    /// tasks finished en route) are collected normally but do not arm it.
    pub target_task_id: Option<&'a str>,
    /// Optional prefix applied to every live-output line this spawn emits
    /// (assistant text, plain-mode stdout passthrough, malformed-JSON
    /// warnings). Used by parallel-wave callers to disambiguate which
    /// slot's Claude is talking when multiple subprocesses tee to stderr
    /// concurrently. Sequential callers pass `None` and output is unprefixed.
    /// Note: child stderr is inherited (not piped) and therefore cannot be
    /// prefixed — only output that flows through our tee paths is tagged.
    pub slot_label: Option<&'a str>,
    /// Active PRD prefix to forward to the child via `TASK_MGR_ACTIVE_PREFIX`.
    /// The loop engine sets this to the iteration's `task_prefix` so that
    /// `task-mgr add --stdin` calls from inside the subprocess auto-prefix IDs
    /// to the correct PRD. All non-loop callers (curate, learnings, merge
    /// resolver, etc.) pass `None`, leaving the variable unset in the child.
    pub active_prefix: Option<&'a str>,
}

/// Which LLM CLI to invoke.
///
/// Static-dispatch enum (no `Box<dyn LlmRunner>`); every dispatch site is
/// forced to handle every variant by exhaustive match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunnerKind {
    Claude,
    Grok,
}

/// Common interface implemented by every concrete LLM runner.
///
/// `Send + Sync` so a runner can be stored on a struct shared across
/// threads (e.g. wave-iteration slot state). Implementations are
/// zero-sized today (`ClaudeRunner`, future `GrokRunner`) — the trait
/// exists for testability + clean separation; production dispatch goes
/// through the `dispatch` free function on a `RunnerKind` discriminant.
pub(crate) trait LlmRunner: Send + Sync {
    /// Spawn the runner's CLI with the given prompt and collect its output.
    fn spawn(
        &self,
        prompt: &str,
        permission_mode: &PermissionMode,
        opts: RunnerOpts<'_>,
    ) -> TaskMgrResult<RunnerResult>;
}

/// Claude CLI runner.
///
/// Wraps `<binary> <base-flags> <permission-flags> [-model m] -p <prompt>`
/// where `<binary>` defaults to `claude` (overridable via `CLAUDE_BINARY`).
/// Base flags are `--print --no-session-persistence` (plain mode) or
/// `--verbose --output-format stream-json --no-session-persistence`
/// (stream-json mode). See `LlmRunner::spawn` impl below for the full
/// permission-mode flag mapping.
pub(crate) struct ClaudeRunner;

impl LlmRunner for ClaudeRunner {
    /// Spawn Claude with the given prompt and collect its output.
    ///
    /// The subprocess runs `<binary> <base-flags> <permission-flags> [-model m] -p <prompt>`.
    /// Permission flags are determined by `permission_mode`:
    /// - `Dangerous` → `--dangerously-skip-permissions`
    /// - `Scoped { allowed_tools: Some(t) }` → `--permission-mode dontAsk --allowedTools <t>`
    /// - `Scoped { allowed_tools: None }` → `--permission-mode dontAsk`
    /// - `Auto` → `--permission-mode auto`
    ///
    /// When `opts.model` is `Some(m)` and non-empty, `--model m` is inserted before `-p`.
    /// The binary defaults to `claude` but can be overridden via the `CLAUDE_BINARY`
    /// environment variable (useful for testing with mock scripts).
    ///
    /// - stdout is piped, read line-by-line, echoed to stderr (tee), and buffered
    /// - stderr is inherited (passes through directly to the terminal)
    /// - The full environment is inherited by the subprocess
    ///
    /// # Errors
    ///
    /// Returns `TaskMgrError::IoErrorWithContext` if the binary is not found
    /// or the process fails to spawn.
    fn spawn(
        &self,
        prompt: &str,
        permission_mode: &PermissionMode,
        opts: RunnerOpts<'_>,
    ) -> TaskMgrResult<RunnerResult> {
        let RunnerOpts {
            signal_flag,
            working_dir,
            model,
            timeout,
            stream_json,
            effort,
            disallowed_tools,
            db_dir,
            cleanup_title_artifact,
            use_pty,
            target_task_id,
            slot_label,
            active_prefix,
        } = opts;
        let binary = std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string());
        let mut args: Vec<String> = if stream_json {
            vec![
                "--print".to_string(),
                "--verbose".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--no-session-persistence".to_string(),
            ]
        } else {
            vec![
                "--print".to_string(),
                "--no-session-persistence".to_string(),
            ]
        };
        match permission_mode {
            PermissionMode::Dangerous => {
                args.push("--dangerously-skip-permissions".to_string());
            }
            PermissionMode::Scoped { allowed_tools } => {
                args.push("--permission-mode".to_string());
                args.push("dontAsk".to_string());
                if let Some(tools) = allowed_tools {
                    args.push("--allowedTools".to_string());
                    args.push(tools.clone());
                }
            }
            PermissionMode::Auto { allowed_tools } => {
                args.push("--permission-mode".to_string());
                args.push("auto".to_string());
                if let Some(tools) = allowed_tools {
                    args.push("--allowedTools".to_string());
                    args.push(tools.clone());
                }
            }
        }
        if let Some(tools) = disallowed_tools
            && !tools.trim().is_empty()
        {
            args.push("--disallowedTools".to_string());
            args.push(tools.to_string());
        }
        if let Some(m) = model
            && !m.trim().is_empty()
        {
            args.push("--model".to_string());
            args.push(m.to_string());
        }
        if let Some(e) = effort
            && !e.trim().is_empty()
        {
            args.push("--effort".to_string());
            args.push(e.to_string());
        }
        // Claude Code 2.1.110 writes an ai-title jsonl despite --no-session-persistence;
        // forcing a known UUID lets the post-wait cleanup delete that exact file.
        // Must stay before -p — Claude only parses flags left of the prompt.
        let cleanup_session_id: Option<Uuid> = cleanup_title_artifact.then(|| {
            let id = Uuid::new_v4();
            args.push("--session-id".to_string());
            args.push(id.to_string());
            id
        });
        args.push("-p".to_string());
        // Prompt is piped via stdin (not as a CLI argument) to avoid OS ARG_MAX
        // limits when prompts are large (e.g. curate dedup with many learnings).

        let mut cmd = Command::new(&binary);
        cmd.args(&args).stdin(Stdio::piped());

        // PTY mode (Unix): wire stdout+stderr to a pseudo-TTY slave so Node.js
        // line-buffers its writes. Pipe mode (default): piped stdout for our
        // reader, inherited stderr for direct terminal passthrough.
        //
        // `pty_master` stays in scope through the end of the read loop — dropping
        // it early would close our end of the PTY and cause reads to EIO mid-run.
        #[cfg(unix)]
        let pty_master: Option<std::os::fd::OwnedFd> = if use_pty {
            match open_pty_for_child_output() {
                Ok((master, slave_out, slave_err)) => {
                    cmd.stdout(Stdio::from(slave_out));
                    cmd.stderr(Stdio::from(slave_err));
                    Some(master)
                }
                Err(e) => {
                    eprintln!(
                        "Warning: failed to allocate PTY for streaming (falling back to pipe): {}",
                        e
                    );
                    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
                    None
                }
            }
        } else {
            cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
            None
        };
        #[cfg(not(unix))]
        {
            let _ = use_pty;
            cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
        }

        // Tell the guard-destructive hook to allow all commands.  The loop
        // engine already scopes permissions via --allowedTools, so the hook's
        // interactive-approval model is not applicable here.
        cmd.env("LOOP_ALLOW_DESTRUCTIVE", "1");

        // Pin every `task-mgr` invocation inside the spawned Claude (and any
        // nested subprocesses that inherit env) to the canonical DB. Without
        // this, when `working_dir` is a worktree, a `task-mgr add` from inside
        // the subprocess would resolve `--dir=".task-mgr"` against the worktree
        // cwd and silently create a stray `<worktree>/.task-mgr/tasks.db` —
        // the original bug this whole feature exists to fix.
        //
        // Canonicalize defensively: the loop's `db_dir` may differ from the
        // git-resolved path the subprocess would compute when reached via a
        // symlinked worktree.
        if let Some(dir) = db_dir {
            let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
            cmd.env("TASK_MGR_DIR", canonical);
        }

        // Pin the active PRD prefix so subprocess calls to `task-mgr add --stdin`
        // auto-prefix IDs to the correct PRD. Set only when Some and non-empty —
        // when None (or empty), leave the variable unset so manual `task-mgr`
        // invocations from inside a worktree inherit the parent env unchanged.
        if let Some(p) = active_prefix.filter(|p| !p.is_empty()) {
            cmd.env(ACTIVE_PREFIX_ENV, p);
        }

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Put child in its own process group so we can kill the entire tree on signal.
        // kill(-child_pid, sig) targets the group. The watchdog thread monitors the
        // signal flag and escalates: SIGTERM → 3s grace → SIGKILL.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TaskMgrError::IoErrorWithContext {
                    file_path: binary.clone(),
                    operation: format!(
                        "spawning Claude subprocess (is '{}' in your PATH?)",
                        binary
                    ),
                    source: e,
                }
            } else {
                TaskMgrError::IoErrorWithContext {
                    file_path: binary.clone(),
                    operation: "spawning Claude subprocess".to_string(),
                    source: e,
                }
            }
        })?;

        // Write the prompt to stdin and close it so the child can start processing.
        // This must happen before reading stdout to avoid deadlock.
        {
            use std::io::Write;
            let mut stdin = child
                .stdin
                .take()
                .expect("stdin should be piped (Stdio::piped() was set on spawn)");
            match stdin.write_all(prompt.as_bytes()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    // Child closed stdin early (e.g., crashed during startup).
                    // We'll capture its exit code below.
                }
                Err(e) => {
                    return Err(TaskMgrError::IoErrorWithContext {
                        file_path: binary.clone(),
                        operation: "writing prompt to Claude subprocess stdin".to_string(),
                        source: e,
                    });
                }
            }
            // stdin is dropped here, closing the pipe
        }

        // Extract PID before starting watchdog — no race condition
        let child_pid = child.id();

        // Shared epoch seconds of when the current task's `<completed>` tag was
        // first observed in the stream. 0 = not yet seen. Written by the reader
        // thread (tee_stream_json), read by the watchdog thread; the watchdog
        // force-exits POST_COMPLETION_GRACE_SECS after the value goes non-zero.
        let completion_epoch = Arc::new(AtomicU64::new(0));

        // Start watchdog thread if signal handling, timeout, or completion-grace
        // is requested. target_task_id alone justifies a watchdog because it's
        // what triggers the post-completion kill.
        let stop_watchdog = Arc::new(AtomicBool::new(false));
        let timed_out_flag = Arc::new(AtomicBool::new(false));
        let completion_killed_flag = Arc::new(AtomicBool::new(false));
        let watchdog_handle =
            if signal_flag.is_some() || timeout.is_some() || target_task_id.is_some() {
                let stop = Arc::clone(&stop_watchdog);
                let flag = signal_flag.cloned();
                let timeout_cfg = timeout;
                let timed_out = Arc::clone(&timed_out_flag);
                let epoch = Arc::clone(&completion_epoch);
                let target = target_task_id.map(str::to_owned);
                let completion_killed = Arc::clone(&completion_killed_flag);
                Some(std::thread::spawn(move || {
                    watchdog_loop(
                        child_pid,
                        flag.as_ref(),
                        &stop,
                        timeout_cfg.as_ref(),
                        &timed_out,
                        Some(&epoch),
                        target.as_deref(),
                        Some(&completion_killed),
                    );
                }))
            } else {
                None
            };

        // In PTY mode, reads come from our master fd (child wrote via the slave end);
        // otherwise they come from the piped stdout. Both implement `Read`; we box to
        // a single type so the downstream tee logic stays generic over the source.
        let reader_source: Box<dyn Read + Send> = {
            #[cfg(unix)]
            {
                if let Some(master) = pty_master {
                    Box::new(std::fs::File::from(master))
                } else {
                    let stdout = child
                        .stdout
                        .take()
                        .expect("stdout should be piped (Stdio::piped() was set on spawn)");
                    Box::new(stdout)
                }
            }
            #[cfg(not(unix))]
            {
                let stdout = child
                    .stdout
                    .take()
                    .expect("stdout should be piped (Stdio::piped() was set on spawn)");
                Box::new(stdout)
            }
        };
        let reader = BufReader::new(reader_source);

        let (output, conversation, permission_denials) = if stream_json {
            tee_stream_json(reader, target_task_id, &completion_epoch, slot_label)
        } else {
            let mut buf = String::new();
            for line_result in reader.lines() {
                match line_result {
                    Ok(line) => {
                        // Tee: echo to stderr (live display) and collect in buffer
                        emit_prefixed_lines(slot_label, &line);
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                    Err(e) if is_pty_read_eof(&e) => break,
                    Err(e) => {
                        emit_prefixed_lines(
                            slot_label,
                            &format!("Warning: error reading Claude stdout: {}", e),
                        );
                        break;
                    }
                }
            }
            (buf, None, Vec::new())
        };

        let status = child.wait().map_err(|e| TaskMgrError::IoErrorWithContext {
            file_path: binary,
            operation: "waiting for Claude subprocess to exit".to_string(),
            source: e,
        })?;

        // Stop the watchdog thread
        stop_watchdog.store(true, Ordering::Release);
        if let Some(handle) = watchdog_handle {
            let _ = handle.join();
        }

        // Child has exited: the ai-title jsonl is guaranteed written (or never will
        // be). Delete it synchronously so it's gone before this function returns,
        // even if the caller (curate_dedup worker) immediately exits the process.
        if let Some(uuid) = cleanup_session_id {
            cleanup_title_artifact_sync(uuid, working_dir);
        }

        let exit_code = exit_code_from_status(status);
        let timed_out = timed_out_flag.load(Ordering::Acquire);
        let completion_killed = completion_killed_flag.load(Ordering::Acquire);

        Ok(RunnerResult {
            exit_code,
            output,
            conversation,
            timed_out,
            completion_killed,
            permission_denials,
        })
    }
}

/// Route a runner invocation to the correct backend.
///
/// `RunnerKind::Claude` → `ClaudeRunner::spawn` (existing Claude subprocess
/// body, byte-identical behavior). `RunnerKind::Grok` → `unimplemented!()`
/// until FEAT-003 lands the `GrokRunner` impl; v1 callers must avoid that
/// variant.
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
        RunnerKind::Claude => ClaudeRunner.spawn(prompt, permission_mode, opts),
        RunnerKind::Grok => unimplemented!(
            "RunnerKind::Grok dispatch not implemented (FEAT-003 will land GrokRunner)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::claude::{ClaudeResult, SpawnOpts};
    use crate::loop_engine::config::CODING_ALLOWED_TOOLS;
    use crate::loop_engine::test_utils::CLAUDE_BINARY_MUTEX;

    /// Compile-only assertion: `SpawnOpts` and `RunnerOpts` are the same
    /// type. If a future refactor ever swaps `SpawnOpts` for a non-aliased
    /// newtype, this fails to compile and the parity contract is broken loudly.
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

    /// v1 behavior pin: dispatch(RunnerKind::Grok, ...) panics with
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
