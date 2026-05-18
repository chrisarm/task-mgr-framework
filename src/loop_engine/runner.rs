//! LLM runner abstraction.
//!
//! Provides a trait-object-free abstraction over LLM CLI subprocesses
//! (Claude, Grok, …). Static `enum RunnerKind` dispatch keeps allocation-free
//! behavior and forces exhaustive-match on every variant.
//!
//! `RunnerKind::Claude` routes through `ClaudeRunner::spawn`, which holds the
//! full Claude-subprocess body (formerly `claude::spawn_claude`).
//! `RunnerKind::Grok` routes through `GrokRunner::spawn`, which mirrors the
//! Claude body but maps flags per the PRD §6 table (e.g. `--allowedTools` →
//! `--tools`, `--output-format stream-json` → `--output-format
//! streaming-json`) and adds the FR-007 auth-failure sniff. Legacy
//! `SpawnOpts` / `ClaudeResult` names remain valid as `pub type` aliases in
//! `claude.rs`, so existing call sites compile unchanged.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Fast-fail window for the Grok auth-failure sniff. A non-zero exit within
/// this window combined with one of [`GROK_AUTH_FAILURE_SUBSTRINGS`] on
/// stderr is classified as [`TaskMgrError::GrokAuthFailure`]. Past the
/// window, a substring match is more likely a tool-use runtime error than an
/// auth lapse, so we fall through to a generic IoError. PRD §6 FR-007.
const GROK_AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(3);

/// Case-insensitive substrings that, combined with a non-zero exit within
/// [`GROK_AUTH_FAILURE_WINDOW`] of spawn, indicate an unauthenticated Grok
/// install. Comparison is done against a lowercased copy of stderr.
const GROK_AUTH_FAILURE_SUBSTRINGS: &[&str] = &[
    "not authenticated",
    "please run grok login",
    "grok login required",
];

/// Operator hint surfaced via [`TaskMgrError::GrokAuthFailure`]. Single source
/// of truth so the loop's auth short-circuit hint stays consistent.
const GROK_AUTH_FAILURE_HINT: &str = "Run `grok login` to authenticate, then retry the task.";

/// Cap on stderr bytes buffered for the auth-failure sniff. Stderr beyond this
/// cap is still tee'd live but not retained for substring scanning — auth
/// failures fire in the first handful of lines, so a small cap keeps memory
/// bounded even when grok later produces verbose output.
const GROK_STDERR_SNIFF_CAP_BYTES: usize = 64 * 1024;

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
    /// Fallback runner CLI binary path resolved from `FallbackRunnerConfig.cli_binary`.
    /// Only consumed by [`GrokRunner`]: used as the second link in the binary
    /// resolution chain (`$GROK_BINARY` → `fallback_cli_binary` → `"grok"` on
    /// PATH). [`ClaudeRunner`] ignores it. `None` falls through to the PATH
    /// default; `Some(p)` is invoked verbatim (no PATH re-resolution).
    pub fallback_cli_binary: Option<&'a str>,
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
            // Grok-only knob; Claude resolves its binary purely via $CLAUDE_BINARY.
            fallback_cli_binary: _,
        } = opts;
        let binary = std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string());
        let base: &[&str] = if stream_json {
            &[
                "--print",
                "--verbose",
                "--output-format",
                "stream-json",
                "--no-session-persistence",
            ]
        } else {
            &["--print", "--no-session-persistence"]
        };
        let mut args: Vec<String> = base.iter().map(|s| s.to_string()).collect();
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
        push_optional_flag(&mut args, "--disallowedTools", disallowed_tools);
        push_optional_flag(&mut args, "--model", model);
        push_optional_flag(&mut args, "--effort", effort);
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

        let mut cmd = Command::new(&binary);
        cmd.args(&args).stdin(Stdio::piped());

        // PTY: Node.js line-buffers only when isatty(1). `pty_master` must stay
        // in scope through the read loop — dropping it early causes EIO mid-run.
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

        apply_common_env(&mut cmd, db_dir, active_prefix, working_dir);
        let mut child = spawn_with_context(&mut cmd, &binary, "Claude")?;
        write_prompt_to_stdin(&mut child, prompt, &binary, "Claude")?;
        let watchdog = spawn_watchdog(child.id(), signal_flag, timeout, target_task_id);

        // Box to a single `Read` so the tee logic is generic over PTY vs pipe.
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
            tee_stream_json(
                reader,
                target_task_id,
                &watchdog.completion_epoch,
                slot_label,
            )
        } else {
            (
                read_plain_stdout(reader, slot_label, "Claude"),
                None,
                Vec::new(),
            )
        };

        let status = child.wait().map_err(|e| TaskMgrError::IoErrorWithContext {
            file_path: binary,
            operation: "waiting for Claude subprocess to exit".to_string(),
            source: e,
        })?;

        let (timed_out, completion_killed) = watchdog.teardown();

        // Child has exited: ai-title jsonl is guaranteed written (or never will be).
        if let Some(uuid) = cleanup_session_id {
            cleanup_title_artifact_sync(uuid, working_dir);
        }

        Ok(RunnerResult {
            exit_code: exit_code_from_status(status),
            output,
            conversation,
            timed_out,
            completion_killed,
            permission_denials,
        })
    }
}

/// Grok CLI runner.
///
/// Wraps `<binary> <base-flags> <permission-flags> [-model m] [-effort e] -p`
/// (prompt is piped via stdin, never as an argv entry — same convention as
/// [`ClaudeRunner`] to dodge OS ARG_MAX limits on large prompts).
///
/// Binary resolution chain (PRD §2.5: "GrokRunner binary resolution is
/// config-independent"):
/// 1. `GROK_BINARY` env var if set and non-empty
/// 2. `opts.fallback_cli_binary` (from `FallbackRunnerConfig.cli_binary`)
/// 3. `"grok"` resolved on `PATH`
///
/// Flag mapping (PRD §6 Public Contracts):
/// - `--no-session-persistence` is **omitted** (grok defaults to no
///   persistence; no equivalent flag needed)
/// - `--allowedTools` → `--tools`
/// - `--disallowedTools` → `--disallowed-tools`
/// - `--dangerously-skip-permissions` → `--permission-mode bypassPermissions`
/// - `--output-format stream-json` → `--output-format streaming-json`
///   (different spelling)
/// - `cleanup_title_artifact: true` is silently ignored — grok has no
///   ai-title-jsonl leak so no `--session-id` flag is emitted and no
///   post-run cleanup runs
///
/// Auth-failure detection (FR-007): stderr is captured into a bounded buffer
/// while still being tee'd to the parent process. After the child exits, if
/// it terminated non-zero AND elapsed wall-clock is within
/// [`GROK_AUTH_FAILURE_WINDOW`] AND lowercased stderr matches one of
/// [`GROK_AUTH_FAILURE_SUBSTRINGS`], the runner returns
/// [`TaskMgrError::GrokAuthFailure`] instead of `Ok(RunnerResult)`. The
/// timing guard distinguishes a real auth lapse (fast-fail at startup) from
/// a long-running tool-use error that happens to mention auth strings.
pub(crate) struct GrokRunner;

impl LlmRunner for GrokRunner {
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
            // Claude-only ai-title workaround; grok has no equivalent artifact.
            // PRD §6: silently ignored — no flag emitted, no post-run cleanup.
            cleanup_title_artifact: _,
            // PTY workaround is Claude-specific (Node.js line-buffering).
            // Out of scope for v1; grok uses plain pipes.
            use_pty: _,
            target_task_id,
            slot_label,
            active_prefix,
            fallback_cli_binary,
        } = opts;

        let binary = resolve_grok_binary(fallback_cli_binary);

        let mut args: Vec<String> = if stream_json {
            vec![
                "--verbose".to_string(),
                "--output-format".to_string(),
                "streaming-json".to_string(),
            ]
        } else {
            Vec::new()
        };
        match permission_mode {
            PermissionMode::Dangerous => {
                args.push("--permission-mode".to_string());
                args.push("bypassPermissions".to_string());
            }
            PermissionMode::Scoped { allowed_tools } => {
                args.push("--permission-mode".to_string());
                args.push("dontAsk".to_string());
                if let Some(tools) = allowed_tools {
                    args.push("--tools".to_string());
                    args.push(tools.clone());
                }
            }
            PermissionMode::Auto { allowed_tools } => {
                args.push("--permission-mode".to_string());
                args.push("auto".to_string());
                if let Some(tools) = allowed_tools {
                    args.push("--tools".to_string());
                    args.push(tools.clone());
                }
            }
        }
        push_optional_flag(&mut args, "--disallowed-tools", disallowed_tools);
        push_optional_flag(&mut args, "--model", model);
        push_optional_flag(&mut args, "--effort", effort);
        args.push("-p".to_string());

        let mut cmd = Command::new(&binary);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Piped (not inherited) so we can sniff for auth-failure substrings
            // while still teeing each line to the parent stderr in real time.
            .stderr(Stdio::piped());

        apply_common_env(&mut cmd, db_dir, active_prefix, working_dir);
        let spawn_instant = Instant::now();
        let mut child = spawn_with_context(&mut cmd, &binary, "Grok")?;
        write_prompt_to_stdin(&mut child, prompt, &binary, "Grok")?;
        let watchdog = spawn_watchdog(child.id(), signal_flag, timeout, target_task_id);

        // Tee stderr to parent stderr while buffering the first
        // GROK_STDERR_SNIFF_CAP_BYTES for the post-exit auth-failure sniff.
        let stderr_pipe = child
            .stderr
            .take()
            .expect("stderr should be piped (Stdio::piped() was set on spawn)");
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_handle = {
            let buf = Arc::clone(&stderr_buf);
            let label = slot_label.map(str::to_owned);
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr_pipe);
                for line_result in reader.lines() {
                    match line_result {
                        Ok(line) => {
                            emit_prefixed_lines(label.as_deref(), &line);
                            if let Ok(mut b) = buf.lock()
                                && b.len() < GROK_STDERR_SNIFF_CAP_BYTES
                            {
                                b.push_str(&line);
                                b.push('\n');
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
        };

        // Plain pipe — grok PTY support is out of v1 scope.
        let reader = BufReader::new(
            child
                .stdout
                .take()
                .expect("stdout should be piped (Stdio::piped() was set on spawn)"),
        );

        let (output, conversation, permission_denials) = if stream_json {
            tee_stream_json(
                reader,
                target_task_id,
                &watchdog.completion_epoch,
                slot_label,
            )
        } else {
            (
                read_plain_stdout(reader, slot_label, "Grok"),
                None,
                Vec::new(),
            )
        };

        let status = child.wait().map_err(|e| TaskMgrError::IoErrorWithContext {
            file_path: binary.clone(),
            operation: "waiting for Grok subprocess to exit".to_string(),
            source: e,
        })?;

        let (timed_out, completion_killed) = watchdog.teardown();
        let _ = stderr_handle.join();
        let elapsed = spawn_instant.elapsed();
        let exit_code = exit_code_from_status(status);

        // Auth-failure sniff: only credible when the child died fast AND with
        // a known auth-phrase on stderr. Either condition alone falls through
        // to a normal RunnerResult.
        if exit_code != 0 && elapsed < GROK_AUTH_FAILURE_WINDOW {
            let stderr_str = stderr_buf.lock().map(|b| b.clone()).unwrap_or_default();
            if stderr_contains_auth_failure(&stderr_str) {
                return Err(TaskMgrError::GrokAuthFailure {
                    hint: GROK_AUTH_FAILURE_HINT.to_string(),
                });
            }
        }

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

/// Resolve the grok binary path. PRD §2.5: config-independent chain
/// `$GROK_BINARY` → `opts.fallback_cli_binary` → bare `"grok"` on PATH.
/// Empty / whitespace-only `$GROK_BINARY` falls through to the next link
/// (treats `""` as "unset" — common shell footgun).
fn resolve_grok_binary(fallback_cli_binary: Option<&str>) -> String {
    if let Ok(env_path) = std::env::var("GROK_BINARY")
        && !env_path.trim().is_empty()
    {
        return env_path;
    }
    if let Some(path) = fallback_cli_binary
        && !path.trim().is_empty()
    {
        return path.to_string();
    }
    "grok".to_string()
}

/// Case-insensitive scan for any of [`GROK_AUTH_FAILURE_SUBSTRINGS`] in the
/// captured stderr. Splitting this out keeps the auth-sniff intent testable
/// without spawning a subprocess.
fn stderr_contains_auth_failure(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    GROK_AUTH_FAILURE_SUBSTRINGS
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Process-lifecycle handles for the subprocess watchdog thread.
///
/// Created by [`spawn_watchdog`]; torn down by [`WatchdogHandles::teardown`].
struct WatchdogHandles {
    /// Written by the reader when `<completed>` is first seen; 0 = unseen.
    /// Must be passed to `tee_stream_json` before calling `teardown`.
    completion_epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    timed_out: Arc<AtomicBool>,
    completion_killed: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl WatchdogHandles {
    /// Signal the watchdog to stop, join it, and return `(timed_out, completion_killed)`.
    fn teardown(self) -> (bool, bool) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle {
            let _ = h.join();
        }
        (
            self.timed_out.load(Ordering::Acquire),
            self.completion_killed.load(Ordering::Acquire),
        )
    }
}

/// Wire subprocess environment variables common to every LLM runner.
///
/// Sets `LOOP_ALLOW_DESTRUCTIVE`, `TASK_MGR_DIR` (canonicalized), and
/// `TASK_MGR_ACTIVE_PREFIX`; applies `current_dir`; puts the child in its
/// own process group on Unix.
fn apply_common_env(
    cmd: &mut Command,
    db_dir: Option<&Path>,
    active_prefix: Option<&str>,
    working_dir: Option<&Path>,
) {
    cmd.env("LOOP_ALLOW_DESTRUCTIVE", "1");
    if let Some(dir) = db_dir {
        let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        cmd.env("TASK_MGR_DIR", canonical);
    }
    if let Some(p) = active_prefix.filter(|p| !p.is_empty()) {
        cmd.env(ACTIVE_PREFIX_ENV, p);
    }
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
}

/// Spawn `cmd`, mapping `NotFound` to a helpful `IoErrorWithContext`.
///
/// `provider_label` ("Claude" or "Grok") is interpolated into the operation
/// string so the error identifies which runner failed.
fn spawn_with_context(
    cmd: &mut Command,
    binary: &str,
    provider_label: &str,
) -> TaskMgrResult<std::process::Child> {
    cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            TaskMgrError::IoErrorWithContext {
                file_path: binary.to_string(),
                operation: format!(
                    "spawning {provider_label} subprocess (is '{binary}' in your PATH?)"
                ),
                source: e,
            }
        } else {
            TaskMgrError::IoErrorWithContext {
                file_path: binary.to_string(),
                operation: format!("spawning {provider_label} subprocess"),
                source: e,
            }
        }
    })
}

/// Write `prompt` to the child's stdin and close the pipe.
///
/// `BrokenPipe` is swallowed — the child may close stdin early on a startup
/// crash; the exit code captured after `child.wait()` is the authoritative
/// signal. Any other write error returns `IoErrorWithContext`.
fn write_prompt_to_stdin(
    child: &mut std::process::Child,
    prompt: &str,
    binary: &str,
    provider_label: &str,
) -> TaskMgrResult<()> {
    use std::io::Write;
    let mut stdin = child
        .stdin
        .take()
        .expect("stdin should be piped (Stdio::piped() was set on spawn)");
    match stdin.write_all(prompt.as_bytes()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(TaskMgrError::IoErrorWithContext {
            file_path: binary.to_string(),
            operation: format!("writing prompt to {provider_label} subprocess stdin"),
            source: e,
        }),
    }
}

/// Create the watchdog Arcs and optionally spawn a watchdog thread.
///
/// Returns [`WatchdogHandles`] whose `completion_epoch` must be passed to
/// `tee_stream_json` before calling `teardown`.
fn spawn_watchdog(
    child_pid: u32,
    signal_flag: Option<&SignalFlag>,
    timeout: Option<TimeoutConfig>,
    target_task_id: Option<&str>,
) -> WatchdogHandles {
    let completion_epoch = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let completion_killed = Arc::new(AtomicBool::new(false));
    let handle = if signal_flag.is_some() || timeout.is_some() || target_task_id.is_some() {
        let stop_w = Arc::clone(&stop);
        let flag = signal_flag.cloned();
        let timed_out_w = Arc::clone(&timed_out);
        let epoch = Arc::clone(&completion_epoch);
        let target = target_task_id.map(str::to_owned);
        let completion_killed_w = Arc::clone(&completion_killed);
        Some(std::thread::spawn(move || {
            watchdog_loop(
                child_pid,
                flag.as_ref(),
                &stop_w,
                timeout.as_ref(),
                &timed_out_w,
                Some(&epoch),
                target.as_deref(),
                Some(&completion_killed_w),
            );
        }))
    } else {
        None
    };
    WatchdogHandles {
        completion_epoch,
        stop,
        timed_out,
        completion_killed,
        handle,
    }
}

/// Push `flag value` onto `args` when `value` is non-empty; no-op otherwise.
fn push_optional_flag(args: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
        args.push(flag.to_string());
        args.push(v.to_string());
    }
}

/// Drain a plain-text stdout reader line-by-line, tee-ing each line to stderr
/// and collecting into a `String`. Handles PTY EOF gracefully.
fn read_plain_stdout(
    reader: impl BufRead,
    slot_label: Option<&str>,
    provider_label: &str,
) -> String {
    let mut buf = String::new();
    for line_result in reader.lines() {
        match line_result {
            Ok(line) => {
                emit_prefixed_lines(slot_label, &line);
                buf.push_str(&line);
                buf.push('\n');
            }
            Err(e) if is_pty_read_eof(&e) => break,
            Err(e) => {
                emit_prefixed_lines(
                    slot_label,
                    &format!("Warning: error reading {provider_label} stdout: {e}"),
                );
                break;
            }
        }
    }
    buf
}

/// Route a runner invocation to the correct backend.
///
/// `RunnerKind::Claude` → [`ClaudeRunner::spawn`] (the existing Claude
/// subprocess body, byte-identical behavior). `RunnerKind::Grok` →
/// [`GrokRunner::spawn`] (FEAT-003).
///
/// # Errors
///
/// Returns whatever the underlying backend returns. Grok adds one provider-
/// specific error variant ([`TaskMgrError::GrokAuthFailure`]); Claude has no
/// equivalent.
pub fn dispatch(
    kind: RunnerKind,
    prompt: &str,
    permission_mode: &PermissionMode,
    opts: RunnerOpts<'_>,
) -> TaskMgrResult<RunnerResult> {
    match kind {
        RunnerKind::Claude => ClaudeRunner.spawn(prompt, permission_mode, opts),
        RunnerKind::Grok => GrokRunner.spawn(prompt, permission_mode, opts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::claude::{ClaudeResult, SpawnOpts};
    use crate::loop_engine::config::CODING_ALLOWED_TOOLS;
    use crate::loop_engine::test_utils::{CLAUDE_BINARY_MUTEX, GROK_BINARY_MUTEX};

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

    /// Build a mock grok binary that emits a marker line plus the prompt.
    /// Mirrors [`make_marker_script`] but is named to make the call sites
    /// in Grok-specific tests self-documenting.
    fn make_grok_marker_script(name: &str, marker: &str) -> std::path::PathBuf {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let path = std::env::temp_dir().join(format!("task_mgr_grok_test_{name}_marker.sh"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
            writeln!(f, r#"echo "{marker} $PROMPT""#).unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Test helper: run dispatch(RunnerKind::Grok) with a mock binary that
    /// prints `marker $PROMPT` on stdout. Mirrors `claude::tests::spawn_claude_echo`
    /// (claude.rs:1221) so future tests adding GrokRunner coverage have a
    /// drop-in helper symmetric with the Claude side.
    ///
    /// Holds [`GROK_BINARY_MUTEX`] for the duration (env-var mutation is
    /// process-global). Cleans up the env var and the temp script on every
    /// exit path.
    fn spawn_grok_echo(
        prompt: &str,
        permission_mode: &PermissionMode,
        stream_json: bool,
    ) -> TaskMgrResult<RunnerResult> {
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let marker = "GROK_ECHO_HELPER_MARKER";
        let script = make_grok_marker_script("spawn_grok_echo", marker);
        unsafe { std::env::set_var("GROK_BINARY", script.to_str().unwrap()) };
        let result = dispatch(
            RunnerKind::Grok,
            prompt,
            permission_mode,
            RunnerOpts {
                stream_json,
                ..RunnerOpts::default()
            },
        );
        unsafe { std::env::remove_var("GROK_BINARY") };
        let _ = std::fs::remove_file(&script);
        result
    }

    /// TEST-INIT-002: dispatch(RunnerKind::Grok, ...) runs the binary at
    /// `GROK_BINARY` and surfaces its stdout in `RunnerResult::output`.
    /// Known-bad discriminator: an `Ok(default())` stub would carry no
    /// marker text and fail the substring assertion.
    #[test]
    fn dispatch_grok_runs_subprocess_and_returns_echoed_output() {
        let perm = scoped_coding();
        let result =
            spawn_grok_echo("hello-from-grok", &perm, false).expect("dispatch(Grok) returned Err");
        assert_eq!(result.exit_code, 0, "expected clean exit, got {result:?}");
        assert!(
            result.output.contains("GROK_ECHO_HELPER_MARKER"),
            "expected marker in stdout, got {:?}",
            result.output,
        );
        assert!(
            result.output.contains("hello-from-grok"),
            "expected piped prompt to round-trip into mock stdout, got {:?}",
            result.output,
        );
    }

    /// TEST-INIT-002 — cleanup_title_artifact is silently ignored. Grok
    /// has no ai-title-jsonl leak, so the runner must not emit `--session-id`
    /// and must not fail on `cleanup_title_artifact: true`. Verified by
    /// dispatching with the flag set and asserting the run succeeds. (The
    /// stronger argv assertion lives in the integration test
    /// `tests/grok_runner_unit.rs::grok_runner_silently_ignores_cleanup_title_artifact`.)
    #[test]
    fn dispatch_grok_silently_ignores_cleanup_title_artifact() {
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let marker = "GROK_CLEANUP_IGNORED_MARKER";
        let script = make_grok_marker_script("cleanup_ignored", marker);
        unsafe { std::env::set_var("GROK_BINARY", script.to_str().unwrap()) };
        let perm = scoped_coding();
        let result = dispatch(
            RunnerKind::Grok,
            "cleanup-probe",
            &perm,
            RunnerOpts {
                cleanup_title_artifact: true,
                ..RunnerOpts::default()
            },
        );
        unsafe { std::env::remove_var("GROK_BINARY") };
        let _ = std::fs::remove_file(&script);

        let r = result.expect("dispatch returned Err");
        assert_eq!(r.exit_code, 0);
        assert!(
            r.output.contains(marker),
            "prompt round-trip lost when cleanup_title_artifact was set"
        );
    }

    /// TEST-INIT-007: stderr substring + non-zero exit within the fast-fail
    /// window classifies as `TaskMgrError::GrokAuthFailure`. Mock script
    /// exits 1 immediately after writing the auth phrase to stderr.
    #[test]
    fn dispatch_grok_classifies_fast_auth_failure() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let path = std::env::temp_dir().join("task_mgr_grok_test_auth_failure.sh");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"printf '%s\n' 'Error: not authenticated' 1>&2"#).unwrap();
            writeln!(f, "exit 1").unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        unsafe { std::env::set_var("GROK_BINARY", path.to_str().unwrap()) };
        let perm = scoped_coding();
        let result = dispatch(RunnerKind::Grok, "probe", &perm, RunnerOpts::default());
        unsafe { std::env::remove_var("GROK_BINARY") };
        let _ = std::fs::remove_file(&path);

        match result {
            Err(TaskMgrError::GrokAuthFailure { hint }) => {
                assert!(
                    !hint.is_empty(),
                    "auth-failure hint must be operator-actionable"
                );
                assert!(
                    hint.to_lowercase().contains("grok login"),
                    "hint should point operators at `grok login`, got {hint:?}"
                );
            }
            Err(other) => panic!("expected GrokAuthFailure, got Err({other:?})"),
            Ok(r) => panic!("expected GrokAuthFailure, got Ok({r:?})"),
        }
    }

    /// TEST-INIT-007: an auth substring on stderr with a CLEAN exit is a
    /// warning, not an auth failure. The grok CLI is allowed to print
    /// stderr during normal operation; only substring + non-zero is the
    /// credible signal.
    #[test]
    fn dispatch_grok_clean_exit_is_not_auth_failure() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let path = std::env::temp_dir().join("task_mgr_grok_test_clean_exit.sh");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(
                f,
                r#"printf '%s\n' 'deprecation: not authenticated is now auth_required' 1>&2"#
            )
            .unwrap();
            writeln!(f, "exit 0").unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        unsafe { std::env::set_var("GROK_BINARY", path.to_str().unwrap()) };
        let perm = scoped_coding();
        let result = dispatch(RunnerKind::Grok, "probe", &perm, RunnerOpts::default());
        unsafe { std::env::remove_var("GROK_BINARY") };
        let _ = std::fs::remove_file(&path);

        match result {
            Err(TaskMgrError::GrokAuthFailure { .. }) => {
                panic!(
                    "clean exit with auth substring on stderr must NOT classify as auth failure"
                );
            }
            Ok(r) => assert_eq!(r.exit_code, 0),
            Err(other) => panic!("expected Ok(success), got Err({other:?})"),
        }
    }

    /// Unit: case-insensitive sniff covers every documented auth phrase.
    /// Pure-function test of the substring scanner — no subprocess overhead.
    #[test]
    fn stderr_contains_auth_failure_is_case_insensitive() {
        assert!(stderr_contains_auth_failure("Error: not authenticated"));
        assert!(stderr_contains_auth_failure("FATAL: NOT AUTHENTICATED"));
        assert!(stderr_contains_auth_failure(
            "auth check failed; please run grok login to continue"
        ));
        assert!(stderr_contains_auth_failure(
            "401 Unauthorized: GROK LOGIN REQUIRED"
        ));
        assert!(!stderr_contains_auth_failure("some unrelated error"));
        assert!(!stderr_contains_auth_failure(""));
    }

    /// Unit: binary resolution chain prefers `$GROK_BINARY`, then
    /// `fallback_cli_binary`, then defaults to `"grok"`. Whitespace-only env
    /// var falls through (treats `""` as unset — common shell footgun).
    #[test]
    fn resolve_grok_binary_precedence_chain() {
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("GROK_BINARY") };
        assert_eq!(resolve_grok_binary(None), "grok");
        assert_eq!(
            resolve_grok_binary(Some("/opt/grok/bin/grok")),
            "/opt/grok/bin/grok"
        );

        unsafe { std::env::set_var("GROK_BINARY", "/env/wins") };
        assert_eq!(
            resolve_grok_binary(Some("/opt/grok/bin/grok")),
            "/env/wins",
            "GROK_BINARY env must win over fallback_cli_binary"
        );

        unsafe { std::env::set_var("GROK_BINARY", "   ") };
        assert_eq!(
            resolve_grok_binary(Some("/opt/grok/bin/grok")),
            "/opt/grok/bin/grok",
            "whitespace-only GROK_BINARY must fall through to fallback_cli_binary"
        );

        unsafe { std::env::remove_var("GROK_BINARY") };
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
