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

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::error::{TaskMgrError, TaskMgrResult};
#[cfg(unix)]
use crate::loop_engine::claude::open_pty_for_child_output;
use crate::loop_engine::claude::{ACTIVE_PREFIX_ENV, ClaudeStreamFormat, is_pty_read_eof};
use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::signals::SignalFlag;
use crate::loop_engine::stream::{CodexStreamFormat, GrokStreamFormat, drive_stream};
use crate::loop_engine::watchdog::{TimeoutConfig, exit_code_from_status, watchdog_loop};
use crate::output::ui;

/// Default fast-fail window for the Grok auth-failure sniff (3 seconds).
///
/// A non-zero exit within this window combined with one of
/// [`GROK_AUTH_FAILURE_SUBSTRINGS`] on stderr is classified as
/// [`TaskMgrError::GrokAuthFailure`]. Past the window a substring match is
/// more likely a tool-use runtime error than an auth lapse. PRD §6 FR-007.
///
/// **Test-only seam**: set `TASK_MGR_GROK_AUTH_WINDOW_SECS` to override
/// the window duration without waiting 3 real seconds. Missing or
/// non-numeric values are silently ignored and fall back to this default.
/// Do NOT set this env var in production.
const GROK_AUTH_FAILURE_WINDOW_DEFAULT_SECS: u64 = 3;

/// Read the auth-failure sniff window, honouring the `TASK_MGR_GROK_AUTH_WINDOW_SECS`
/// test-only env override. Falls back to [`GROK_AUTH_FAILURE_WINDOW_DEFAULT_SECS`]
/// on missing or non-numeric values.
fn grok_auth_failure_window() -> Duration {
    std::env::var("TASK_MGR_GROK_AUTH_WINDOW_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(GROK_AUTH_FAILURE_WINDOW_DEFAULT_SECS))
}

/// Case-insensitive substrings that, combined with a non-zero exit within
/// [`GROK_AUTH_FAILURE_WINDOW_DEFAULT_SECS`] of spawn, indicate an
/// unauthenticated Grok install. Comparison is done against a lowercased
/// copy of stderr.
///
/// **Runbook**: this list is the auth-failure short-circuit's only signal.
/// A missed match silently fails open — the task gets counted toward
/// `consecutive_failures` and may be auto-blocked with a misleading
/// "max retries exceeded" reason instead of "grok auth failed". On each
/// grok CLI version bump, re-capture the unauthenticated stderr output and
/// extend this list if new phrasing appears (see
/// `src/loop_engine/CLAUDE.md` "Grok auth-failure detection").
const GROK_AUTH_FAILURE_SUBSTRINGS: &[&str] = &[
    "not authenticated",
    "please run grok login",
    "grok login required",
    "login required",
    "authentication required",
    "authentication failed",
    "unauthorized",
    "401",
    "invalid api key",
    "api key not found",
    "missing api key",
];

/// Operator hint surfaced via [`TaskMgrError::GrokAuthFailure`]. Single source
/// of truth so the loop's auth short-circuit hint stays consistent.
const GROK_AUTH_FAILURE_HINT: &str = "Run `grok login` to authenticate, then retry the task.";
const CODEX_AUTH_FAILURE_HINT: &str = "Run `codex login` to authenticate, then retry the task.";
/// Auth-marker substrings (case-insensitive) that, combined with an
/// `[Error: ...]` line in the Codex conversation transcript, indicate an
/// authentication failure. Comparison is against a lowercased copy of the
/// transcript line. Matched only against STRUCTURED `[Error: ` lines so a
/// substring hidden in agent text does not trip a false positive.
const CODEX_AUTH_FAILURE_MARKERS: &[&str] = &[
    "401",
    "unauthorized",
    "missing bearer",
    "missing-bearer",
    "invalid api key",
    "invalid bearer",
    "authentication failed",
    "authentication required",
    "not authenticated",
    "login required",
];

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
    /// Session UUID injected via `--session-id` into the LLM CLI invocation.
    /// `Some(uuid)` for ClaudeRunner (unconditional as of FEAT-003).
    /// `None` for GrokRunner (Grok capture lands in FEAT-004).
    pub session_id: Option<Uuid>,
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
    /// When `true`, signal that the caller wants per-session ai-title artifact
    /// cleanup for this invocation. Used as the opt-in field that drives the
    /// [`RunnerCapability::TitleArtifactCleanup`] enforcement gate in `dispatch`:
    /// if set to `true` on a runner that returns `false` for
    /// `supports(TitleArtifactCleanup)` (e.g. [`GrokRunner`]), dispatch returns
    /// [`TaskMgrError::UnsupportedRunnerCapability`] before spawning.
    ///
    /// When `true` on a supported runner ([`ClaudeRunner`]), dispatch drives
    /// `cleanup_session` post-spawn (FEAT-006). Callers that do not need
    /// the cleanup workaround (e.g. non-loop callers) leave this `false`
    /// (the default), and cleanup is skipped.
    pub cleanup_title_artifact: bool,
    /// Fallback runner CLI binary path resolved from `FallbackRunnerConfig.cli_binary`.
    /// Only consumed by `GrokRunner`: used as the second link in the binary
    /// resolution chain (`$GROK_BINARY` → `fallback_cli_binary` → `"grok"` on
    /// PATH). `ClaudeRunner` ignores it. `None` falls through to the PATH
    /// default; `Some(p)` is invoked verbatim (no PATH re-resolution).
    pub fallback_cli_binary: Option<&'a str>,
    /// Loop run ID forwarded to the per-slot grok stderr capture file name.
    /// Non-loop callers pass `None`; the sniffer uses `"no-run"` as a fallback.
    pub run_id: Option<&'a str>,
    /// Iteration index forwarded to the per-slot grok stderr capture file name.
    /// `None` causes the sniffer to use `0` as a fallback.
    pub iteration: Option<u32>,
}

/// Which LLM CLI to invoke.
///
/// Static-dispatch enum (no `Box<dyn LlmRunner>`); every dispatch site is
/// forced to handle every variant by exhaustive match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunnerKind {
    Claude,
    Grok,
    Codex,
}

impl RunnerKind {
    /// Whether this backend supports `cap`, without constructing a trait
    /// object. Delegates to the zero-sized runner `supports` impls so the
    /// capability matrix stays single-sourced. Lets callers shape
    /// [`RunnerOpts`] to the *selected* runner before [`dispatch`] (which
    /// fail-closes on a capability the runner lacks) — e.g. only request
    /// `cleanup_title_artifact` when the runner actually emits the artifact.
    pub(crate) fn supports(self, cap: RunnerCapability) -> bool {
        match self {
            RunnerKind::Claude => ClaudeRunner.supports(cap),
            RunnerKind::Grok => GrokRunner.supports(cap),
            RunnerKind::Codex => CodexRunner.supports(cap),
        }
    }
}

/// Typed capability surface used by `dispatch` to refuse a spawn whose
/// [`RunnerOpts`] sets a field encoding a capability the chosen runner
/// does not support — before any subprocess is launched.
///
/// (a) **Surface contract**: each variant maps 1:1 to a `RunnerOpts` field
/// whose presence (non-default value) implies the caller wants behavior
/// the runner may or may not deliver. The mapping from variant to
/// `RunnerOpts` field lives in a single `checks` registry table on the
/// `dispatch` side (FEAT-003), never duplicated across runner impls.
///
/// (b) **`#[non_exhaustive]` rationale**: capabilities are an open set —
/// future runners (Gemini, local llama.cpp, …) will surface new flags
/// (`ThinkingTokens`, `PermissionMode`, `SessionId`, …) as `RunnerOpts`
/// grows. Marking the enum non-exhaustive means external crates cannot
/// pattern-match without a wildcard, leaving us free to add variants
/// without a major-version bump. Internal `LlmRunner` impls intentionally
/// match exhaustively (see (c)) — `#[non_exhaustive]` does NOT change
/// in-crate match semantics.
///
/// (c) **Exhaustive-match-in-production-impls convention**: every
/// production [`LlmRunner::supports`] impl MUST use an exhaustive match
/// (NO `_` wildcard arm). Adding a new variant then forces a per-runner
/// compile error so the capability decision is made deliberately — never
/// silently defaulted. This is the forcing function that makes the
/// capability surface a contract instead of a hint; a wildcard arm here
/// defeats the entire mechanism.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RunnerCapability {
    /// Runner accepts an `--effort` (or equivalent) flag for reasoning depth.
    /// Maps to [`RunnerOpts::effort`].
    Effort,
    /// Runner emits stream-json (or `streaming-json`) output when requested.
    /// Maps to [`RunnerOpts::stream_json`].
    StreamJson,
    /// Runner honors PTY allocation for stdout line-buffering workarounds.
    /// Maps to [`RunnerOpts::use_pty`]. Claude-specific today (Node.js
    /// line-buffering); Grok uses plain pipes.
    Pty,
    /// Runner accepts a disallowed-tools list. Maps to
    /// [`RunnerOpts::disallowed_tools`].
    DisallowedTools,
    /// Runner needs to inject `--session-id` and clean up the ai-title jsonl
    /// artifact after the subprocess exits. Claude-specific workaround for the
    /// 2.1.110 session-leak; Grok has no equivalent artifact. Maps to
    /// [`RunnerOpts::cleanup_title_artifact`].
    TitleArtifactCleanup,
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

    /// Declare whether this runner supports a given capability.
    ///
    /// Used by [`dispatch`] (FEAT-003) to refuse calls that set a
    /// [`RunnerOpts`] field encoding an unsupported capability **before**
    /// any subprocess is spawned — fail-closed at the boundary instead of
    /// silently dropping the flag in the runner body.
    ///
    /// The default returns `false` for every capability: an
    /// implementation that forgets to override `supports` is treated as
    /// "supports nothing", so every capability-driven call against it
    /// will be rejected by dispatch. Choosing the safe direction here
    /// means a future runner added without thinking about capabilities
    /// fails loudly at the spawn boundary instead of silently no-op'ing
    /// on flags the operator depended on.
    ///
    /// Production impls MUST override this with an **exhaustive** match
    /// (no `_` wildcard arm) — see the
    /// [`RunnerCapability`] rustdoc, paragraph (c).
    fn supports(&self, _cap: RunnerCapability) -> bool {
        false
    }

    /// Delete the per-session artifact (file or directory) that this runner's
    /// CLI wrote to disk for the given `(session_id, cwd)` tuple.
    ///
    /// **Target identification**: implementations MUST derive the artifact
    /// path deterministically from `session_id` + `cwd` and remove ONLY that
    /// path. Never enumerate-and-sweep; shared session directories that
    /// accumulate cross-session state (e.g. `prompt_history.jsonl`) must be
    /// preserved.
    ///
    /// **Idempotency**: `NotFound` is silent success — the artifact may have
    /// never been written (e.g. a future CLI release stops leaking) or may
    /// already have been removed by a prior call.
    ///
    /// **Default impl returns `Ok(())`**: providers with no headless artifact
    /// (future cloud-API runners, in-process backends) need zero cleanup
    /// code. The trait method is statically dispatched per `RunnerKind`
    /// variant — no dynamic dispatch overhead.
    ///
    /// **Dispatch-side contract** (FEAT-006): `dispatch` calls this
    /// unconditionally after every spawn. Errors are surfaced via the
    /// warn-once banner so a misconfigured `~/.claude/` or `~/.grok/` mount
    /// produces one diagnostic line instead of per-iteration spam.
    fn cleanup_session(&self, _session_id: Uuid, _cwd: &Path) -> TaskMgrResult<()> {
        Ok(())
    }
}

/// Warn-once guard for cleanup_session errors in [`dispatch`].
/// Prevents a misconfigured `~/.claude/` or `~/.grok/` mount from printing
/// one stderr line per spawn across a 50-batch run. The first error prints;
/// subsequent errors in the same process are silent.
static CLEANUP_WARN_ONCE: AtomicBool = AtomicBool::new(false);

/// Reset the [`CLEANUP_WARN_ONCE`] gate to `false`.
///
/// Intended exclusively for integration tests that need to observe the
/// first-banner transition in isolation. Production code must never call
/// this — the gate is intentionally sticky across a process lifetime.
pub fn reset_cleanup_warn_once_for_test() {
    CLEANUP_WARN_ONCE.store(false, Ordering::SeqCst);
}

/// Read the current [`CLEANUP_WARN_ONCE`] state.
///
/// Returns `true` if at least one cleanup error has been logged in this
/// process. Intended for integration tests that need to verify the
/// warn-once rate-limit transitions without capturing stderr at the fd level.
pub fn cleanup_warn_once_was_triggered() -> bool {
    CLEANUP_WARN_ONCE.load(Ordering::SeqCst)
}

// WORKAROUND(claude-code-2.1.110-session-stub): Claude Code 2.1.110 writes a
// per-session ai-title jsonl under `<HOME>/.claude/projects/<encoded-cwd>/`
// despite `--no-session-persistence`. Until upstream stops leaking, the
// runner injects a known UUID via `--session-id` and synchronously deletes
// the matching file after the child exits. Dispatch (FEAT-006) calls this
// unconditionally post-spawn and routes Err via the CLEANUP_WARN_ONCE banner.
pub(crate) fn cleanup_claude_session_artifact(
    session_id: Uuid,
    cwd: Option<&Path>,
) -> TaskMgrResult<()> {
    use crate::loop_engine::claude::encoded_cwd_dir;
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => return Ok(()),
    };
    let cwd_buf = match cwd {
        Some(p) => p.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        },
    };
    let target = encoded_cwd_dir(&cwd_buf, &home).join(format!("{}.jsonl", session_id));
    match std::fs::remove_file(&target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(TaskMgrError::IoErrorWithContext {
            file_path: target.display().to_string(),
            operation: "deleting ai-title session artifact".to_string(),
            source: e,
        }),
    }
}

/// Compute the directory the Grok CLI uses for a given working directory.
///
/// Grok writes per-session artifacts under
/// `<HOME>/.grok/sessions/<percent-encoded-cwd>/<session-uuid>/`. The cwd
/// is percent-encoded so embedded slashes don't collide with the directory
/// hierarchy (e.g. `/home/user/repo` → `%2Fhome%2Fuser%2Frepo`).
///
/// Pure: takes `&Path` inputs, no filesystem access. Trailing slashes are
/// trimmed before encoding so `/foo/` and `/foo` map to the same directory.
pub(crate) fn grok_encoded_session_dir(cwd: &Path, home: &Path) -> PathBuf {
    let cwd_str = cwd.to_string_lossy();
    let trimmed = cwd_str.trim_end_matches('/');
    let encoded = urlencoding::encode(trimmed).into_owned();
    home.join(".grok").join("sessions").join(encoded)
}

/// Tiebreaker for the FEAT-004 pre/post dir diff when multiple new UUID
/// subdirectories appear under `grok_dir` after the child exits (rare
/// parallel-slot race). Returns the UUID whose directory has the latest
/// modification time (last-write-wins). Returns `None` if mtime is
/// unreadable for every candidate — safest outcome under unexpected FS state.
fn pick_newest_by_mtime(grok_dir: &Path, ids: &[Uuid]) -> Option<Uuid> {
    let mut best: Option<(std::time::SystemTime, Uuid)> = None;
    for &id in ids {
        let dir_path = grok_dir.join(id.to_string());
        if let Ok(meta) = std::fs::metadata(&dir_path)
            && let Ok(mtime) = meta.modified()
        {
            match &best {
                None => best = Some((mtime, id)),
                Some((prev, _)) if mtime > *prev => best = Some((mtime, id)),
                _ => {}
            }
        }
    }
    best.map(|(_, id)| id)
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
    // EXHAUSTIVE match (no `_` arm) is the forcing function — a new
    // `RunnerCapability` variant must cause a per-runner compile error so
    // the support decision is made deliberately. See `RunnerCapability`
    // rustdoc paragraph (c).
    fn supports(&self, cap: RunnerCapability) -> bool {
        match cap {
            RunnerCapability::Effort => true,
            RunnerCapability::StreamJson => true,
            RunnerCapability::Pty => true,
            RunnerCapability::DisallowedTools => true,
            RunnerCapability::TitleArtifactCleanup => true,
        }
    }

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
            use_pty,
            target_task_id,
            slot_label,
            active_prefix,
            // Grok-only knob; Claude resolves its binary purely via $CLAUDE_BINARY.
            // cleanup_title_artifact is a capability-gate signal enforced at dispatch;
            // spawn body does not read it. Use `..` to elide all remaining fields.
            fallback_cli_binary: _,
            ..
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
        // WORKAROUND(claude-code-2.1.110): Claude writes an ai-title jsonl despite
        // --no-session-persistence. Forcing a known UUID lets the post-wait cleanup
        // delete that exact file. Must stay before -p — Claude only parses flags left
        // of the prompt. Unconditional as of FEAT-003; inline cleanup moves to dispatch
        // in FEAT-006.
        let uuid = Uuid::new_v4();
        args.push("--session-id".to_string());
        args.push(uuid.to_string());
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
                    tracing::warn!(
                        error = %e,
                        "failed to allocate PTY for streaming (falling back to pipe)",
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

        apply_common_env(
            &mut cmd,
            db_dir,
            active_prefix,
            working_dir,
            RunnerKind::Claude,
        );
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
            drive_stream(
                reader,
                &ClaudeStreamFormat,
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

        Ok(RunnerResult {
            exit_code: exit_code_from_status(status),
            output,
            conversation,
            timed_out,
            completion_killed,
            permission_denials,
            session_id: Some(uuid),
        })
    }

    fn cleanup_session(&self, session_id: Uuid, cwd: &Path) -> TaskMgrResult<()> {
        cleanup_claude_session_artifact(session_id, Some(cwd))
    }
}

/// Grok CLI runner.
///
/// Wraps `<binary> <base-flags> <permission-flags> [-model m] [-effort e]
/// --prompt-file <tempfile>` with stdin set to `Stdio::null()`. Unlike
/// [`ClaudeRunner`] (whose `--print`/`-p` reads stdin), grok's
/// `-p/--single <PROMPT>` requires an inline value and ignores stdin, so the
/// prompt is delivered through a temp file — which also dodges OS
/// `MAX_ARG_STRLEN` (128 KiB) on large prompts. See `write_prompt_to_tempfile`.
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
///   (different spelling; Claude's required `--verbose` companion is **not**
///   passed — grok has no such flag)
/// - prompt via stdin (`-p`/`--print`) → `--prompt-file <tempfile>`
/// - `cleanup_title_artifact: true` is rejected at dispatch with
///   [`TaskMgrError::UnsupportedRunnerCapability`] — grok has no
///   ai-title-jsonl leak and the `TitleArtifactCleanup` capability is
///   `false` on [`GrokRunner`].
///
/// Session capture: `spawn` snapshots `~/.grok/sessions/<encoded-cwd>/`
/// immediately before and after the child exits (pre/post diff). Entries
/// that are valid UUIDs and appear only in the post-snapshot are the
/// candidate session ids. Zero new ids → `session_id: None`. Exactly one
/// → `session_id: Some(uuid)`. Multiple (rare parallel-slot race) →
/// the UUID directory with the most recent mtime is chosen
/// (last-write-wins heuristic; see `pick_newest_by_mtime`). `HOME` absent
/// → snapshot is skipped and `session_id` stays `None` (best-effort).
///
/// Auth-failure detection (FR-007): stderr is captured into a bounded buffer
/// while still being tee'd to the parent process. After the child exits, if
/// it terminated non-zero AND elapsed wall-clock is within the window returned
/// by [`grok_auth_failure_window`] (default 3 s; overridable via
/// `TASK_MGR_GROK_AUTH_WINDOW_SECS` for tests) AND lowercased stderr matches
/// one of [`GROK_AUTH_FAILURE_SUBSTRINGS`], the runner returns
/// [`TaskMgrError::GrokAuthFailure`] instead of `Ok(RunnerResult)`. The
/// timing guard distinguishes a real auth lapse (fast-fail at startup) from
/// a long-running tool-use error that happens to mention auth strings.
pub(crate) struct GrokRunner;

impl LlmRunner for GrokRunner {
    // EXHAUSTIVE match (no `_` arm) per the `RunnerCapability` convention.
    // `Pty=false`: grok uses plain pipes; the Node.js line-buffering
    // workaround does not apply.
    // `TitleArtifactCleanup=false`: grok has no ai-title-jsonl leak.
    fn supports(&self, cap: RunnerCapability) -> bool {
        match cap {
            RunnerCapability::Effort => true,
            RunnerCapability::StreamJson => true,
            RunnerCapability::Pty => false,
            RunnerCapability::DisallowedTools => true,
            RunnerCapability::TitleArtifactCleanup => false,
        }
    }

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
            target_task_id,
            slot_label,
            active_prefix,
            fallback_cli_binary,
            run_id,
            iteration,
            // Capabilities the runner does not support are enforced at dispatch;
            // this destructure consumes only the fields Grok actively uses.
            ..
        } = opts;

        // Resolve cwd and HOME for the session-dir snapshot. HOME absence
        // suppresses the snapshot (best-effort; mirrors Claude helper behavior).
        let cwd = working_dir
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let home_opt = std::env::var("HOME").ok().map(PathBuf::from);
        let grok_dir_opt = home_opt.as_ref().map(|h| grok_encoded_session_dir(&cwd, h));
        // Pre-spawn snapshot of entry names; missing dir → empty set (first run).
        let before: HashSet<String> = grok_dir_opt
            .as_ref()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();

        let binary = resolve_grok_binary(fallback_cli_binary);

        // NOTE: unlike Claude (which requires `--verbose` alongside
        // `--output-format stream-json`), grok takes `--output-format
        // streaming-json` standalone and rejects `--verbose` (its only
        // near-match is `-v/--version`).
        let mut args: Vec<String> = if stream_json {
            vec!["--output-format".to_string(), "streaming-json".to_string()]
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
        // WORKAROUND(grok-cli-headless-subagent-coordinator): in headless
        // one-shot mode (`--output-format streaming-json --prompt-file`) grok's
        // background-subagent coordinator is unreliable. A review prompt that
        // says "use the <X> agent for the review pass" makes grok try to
        // background-spawn that subagent; the coordinator then cancels it
        // mid-turn (`turn_ended outcome="cancelled" cancellation_category=
        // "mid_turn_abort"`) and the session_state upload drops
        // (`channel_dropped`), aborting the PARENT review at turn 0. task-mgr
        // then finds the task stuck `in_progress`, auto-recovers it as stale,
        // and retries forever without progress. task-mgr runs exactly one task
        // per grok process and never relies on grok fanning out to subagents,
        // so disable spawning outright — grok performs the work inline with its
        // own prompt + context. Remove if upstream grok makes headless subagent
        // spawning reliable.
        args.push("--no-subagents".to_string());
        push_optional_flag(&mut args, "--disallowed-tools", disallowed_tools);
        push_optional_flag(&mut args, "--model", model);
        push_optional_flag(&mut args, "--effort", effort);

        // Grok's `-p/--single <PROMPT>` requires the prompt as an inline arg
        // value and does NOT read it from stdin (unlike Claude's `--print`,
        // which does). Inline args are bounded by Linux MAX_ARG_STRLEN
        // (128 KiB per arg), which a CODE-REVIEW prompt carrying a full diff
        // can exceed (→ E2BIG), so route the prompt through `--prompt-file`
        // instead. `prompt_file` is held until the child exits; its `Drop`
        // unlinks the temp file (owned values drop at end of this scope,
        // which is past `child.wait()` below).
        let prompt_file = write_prompt_to_tempfile(prompt, &binary, "Grok")?;
        args.push("--prompt-file".to_string());
        args.push(prompt_file.path().to_string_lossy().into_owned());

        let mut cmd = Command::new(&binary);
        cmd.args(&args)
            // No stdin: the prompt is delivered via `--prompt-file`. Null
            // (not piped) so grok can't block reading a stdin we never write.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // Piped (not inherited) so we can sniff for auth-failure substrings
            // while still teeing each line to the parent stderr in real time.
            .stderr(Stdio::piped());

        apply_common_env(
            &mut cmd,
            db_dir,
            active_prefix,
            working_dir,
            RunnerKind::Grok,
        );
        let capture_path =
            grok_stderr_capture_path(db_dir, active_prefix, run_id, slot_label, iteration);
        let spawn_instant = Instant::now();
        let mut child = spawn_with_context(&mut cmd, &binary, "Grok")?;
        let watchdog = spawn_watchdog(child.id(), signal_flag, timeout, target_task_id);

        if let Some(ref path) = capture_path {
            ui::emit(&format!("grok stderr → {}", path.display()));
        }
        let (stderr_buf, stderr_handle) = spawn_grok_stderr_sniffer(&mut child, capture_path);

        // Plain pipe — grok PTY support is out of v1 scope.
        let reader = BufReader::new(
            child
                .stdout
                .take()
                .expect("stdout should be piped (Stdio::piped() was set on spawn)"),
        );

        let (output, conversation, permission_denials) = if stream_json {
            drive_stream(
                reader,
                &GrokStreamFormat,
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

        // Post-spawn snapshot: diff against before to find the new session dir.
        let session_id: Option<Uuid> = grok_dir_opt.as_ref().and_then(|grok_dir| {
            let after: HashSet<String> = std::fs::read_dir(grok_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            let new_ids: Vec<Uuid> = after
                .difference(&before)
                .filter_map(|s| Uuid::parse_str(s).ok())
                .collect();
            match new_ids.len() {
                0 => None,
                1 => Some(new_ids[0]),
                _ => pick_newest_by_mtime(grok_dir, &new_ids),
            }
        });

        // Post-exit stderr classification. Read the buffered stderr ONCE for
        // both sniffs (auth-failure + transient-backend).
        if exit_code != 0 {
            let stderr_str = stderr_buf.lock().map(|b| b.clone()).unwrap_or_default();

            // Auth-failure sniff: only credible when the child died fast AND
            // with a known auth-phrase on stderr. The fast-fail timing window
            // distinguishes a real auth lapse (errors at startup) from a
            // long-running tool-use error that happens to mention auth strings.
            // Window is overridable via env var for tests.
            if elapsed < grok_auth_failure_window() && stderr_contains_auth_failure(&stderr_str) {
                return Err(TaskMgrError::GrokAuthFailure {
                    hint: GROK_AUTH_FAILURE_HINT.to_string(),
                });
            }

            // Transient-backend sniff (FEAT-014): a 5xx / Bad Gateway /
            // overloaded response is a "retry later" signal, NOT a task crash.
            // UNLIKE the auth sniff this is NOT time-windowed — the motivating
            // incident was a `cli-chat-proxy.grok.com` 502 that recurred after
            // grok restarted its turn repeatedly, so the elapsed wall-clock was
            // well past the 3s auth window. Surfacing TransientBackend (instead
            // of letting the non-zero exit fall through to Crash(RuntimeError))
            // keeps the loop from burning crash budget / resetting in-flight
            // work. Uses the shared `detection` classifiers so the Grok-stderr
            // and Claude-stdout paths cannot drift.
            if crate::loop_engine::detection::is_transient_backend(&stderr_str) {
                return Err(TaskMgrError::TransientBackend {
                    retry_after_secs: crate::loop_engine::detection::parse_retry_after_secs(
                        &stderr_str,
                    ),
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
            session_id,
        })
    }

    // WORKAROUND(grok-cli-no-persistence-off): grok has no --no-session-persistence
    // equivalent and writes a directory of artifacts per session at
    // ~/.grok/sessions/<percent-encoded-cwd>/<uuid>/. Remove only the uuid subdir;
    // prompt_history.jsonl in the parent dir accumulates across sessions by design
    // and must be preserved.
    fn cleanup_session(&self, session_id: Uuid, cwd: &Path) -> TaskMgrResult<()> {
        let home = match std::env::var("HOME") {
            Ok(h) if !h.is_empty() => PathBuf::from(h),
            _ => return Ok(()),
        };
        let target = grok_encoded_session_dir(cwd, &home).join(session_id.to_string());
        match std::fs::remove_dir_all(&target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(TaskMgrError::IoErrorWithContext {
                file_path: target.display().to_string(),
                operation: "deleting grok session directory".to_string(),
                source: e,
            }),
        }
    }
}

pub(crate) struct CodexRunner;

impl LlmRunner for CodexRunner {
    fn supports(&self, cap: RunnerCapability) -> bool {
        match cap {
            RunnerCapability::Effort => false,
            RunnerCapability::StreamJson => true,
            RunnerCapability::Pty => false,
            RunnerCapability::DisallowedTools => false,
            RunnerCapability::TitleArtifactCleanup => false,
        }
    }

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
            db_dir,
            target_task_id,
            slot_label,
            active_prefix,
            ..
        } = opts;

        let binary = resolve_codex_binary();
        let mut args: Vec<String> = Vec::new();
        match permission_mode {
            PermissionMode::Dangerous => {
                args.push("exec".to_string());
                args.push("--json".to_string());
                args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
            }
            PermissionMode::Scoped { .. } | PermissionMode::Auto { .. } => {
                args.push("-a".to_string());
                args.push("never".to_string());
                args.push("exec".to_string());
                args.push("--json".to_string());
                args.push("--sandbox".to_string());
                args.push("workspace-write".to_string());
            }
        }
        args.push("--ephemeral".to_string());
        args.push("--skip-git-repo-check".to_string());
        if let Some(cwd) = working_dir {
            args.push("--cd".to_string());
            args.push(cwd.to_string_lossy().into_owned());
        }
        push_optional_flag(&mut args, "-m", model);
        args.push("-".to_string());

        let mut cmd = Command::new(&binary);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_common_env(
            &mut cmd,
            db_dir,
            active_prefix,
            working_dir,
            RunnerKind::Codex,
        );
        let mut child = spawn_with_context(&mut cmd, &binary, "Codex")?;

        // Writer thread: a blocking write_all on the main thread would deadlock
        // when the OS pipe buffer fills (~64 KiB) and Codex is busy with the
        // request — parent blocks on write while Codex blocks producing output.
        // The thread tolerates an early Codex exit (BrokenPipe / any other IO
        // error is non-fatal): stdout-read + exit_code at the bottom of this
        // function are authoritative for the spawn result.
        let stdin_pipe = child
            .stdin
            .take()
            .expect("stdin should be piped (Stdio::piped() was set on spawn)");
        let prompt_owned = prompt.to_string();
        let writer_handle = std::thread::spawn(move || {
            use std::io::Write as _;
            let mut stdin = stdin_pipe;
            // Best-effort: BrokenPipe is the expected error on early child
            // exit. Explicit drop closes the write side of the pipe so Codex
            // observes stdin EOF and proceeds with the captured prompt.
            let _ = stdin.write_all(prompt_owned.as_bytes());
            drop(stdin);
        });

        let watchdog = spawn_watchdog(child.id(), signal_flag, timeout, target_task_id);

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_handle = spawn_stderr_sniffer(&mut child, Arc::clone(&stderr_buf), "Codex");
        let reader = BufReader::new(
            child
                .stdout
                .take()
                .expect("stdout should be piped (Stdio::piped() was set on spawn)"),
        );
        let (output, conversation, permission_denials) = if stream_json {
            drive_stream(
                reader,
                &CodexStreamFormat,
                target_task_id,
                &watchdog.completion_epoch,
                slot_label,
            )
        } else {
            (
                read_plain_stdout(reader, slot_label, "Codex"),
                None,
                Vec::new(),
            )
        };
        let status = child.wait().map_err(|e| TaskMgrError::IoErrorWithContext {
            file_path: binary.clone(),
            operation: "waiting for Codex subprocess to exit".to_string(),
            source: e,
        })?;
        let (timed_out, completion_killed) = watchdog.teardown();
        let _ = stderr_handle.join();
        // Join the writer thread AFTER the child has exited so we don't hold
        // onto the stdin pipe past child reaping. join() failures are
        // non-fatal — the stdin write is best-effort by design.
        let _ = writer_handle.join();
        let exit_code = exit_code_from_status(status);

        if exit_code != 0 {
            let stderr_str = stderr_buf.lock().map(|b| b.clone()).unwrap_or_default();
            // Post-exit auth-failure classification matches markers ONLY on
            // structured `[Error: ...]` lines emitted by the stream parser's
            // `type:"error"` / `type:"turn.failed"` handler. An
            // `agent_message` that quotes "HTTP 401" lands in `assistant_buf`
            // (and thus `output`), NOT in the conversation transcript with
            // the `[Error: ` prefix — so it is NOT misclassified.
            if let Some(ref conv) = conversation
                && codex_conversation_indicates_auth_failure(conv)
            {
                return Err(TaskMgrError::CodexAuthFailure {
                    hint: CODEX_AUTH_FAILURE_HINT.to_string(),
                });
            }
            if crate::loop_engine::detection::is_transient_backend(&stderr_str) {
                return Err(TaskMgrError::TransientBackend {
                    retry_after_secs: crate::loop_engine::detection::parse_retry_after_secs(
                        &stderr_str,
                    ),
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
            session_id: None,
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

fn resolve_codex_binary() -> String {
    if let Ok(env_path) = std::env::var("CODEX_BINARY")
        && !env_path.trim().is_empty()
    {
        return env_path;
    }
    "codex".to_string()
}

/// Returns `true` when the Codex stream-json transcript contains a structured
/// auth-failure signal: an `[Error: ...]` line (emitted from a `type:"error"`
/// or `type:"turn.failed"` stream event) whose message contains a marker from
/// [`CODEX_AUTH_FAILURE_MARKERS`].
///
/// **Why structured-only**: the Codex stream parser routes `type:"error"` and
/// `type:"turn.failed"` events to the conversation transcript with the
/// `[Error: ` prefix, while plain `agent_message` text goes to the output
/// channel. Matching only on `[Error: ` lines means a model reply that
/// quotes "HTTP 401" in agent text is NOT classified as an auth failure.
fn codex_conversation_indicates_auth_failure(conversation: &str) -> bool {
    for line in conversation.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("[Error: ") else {
            continue;
        };
        let body = rest.strip_suffix(']').unwrap_or(rest);
        let lower = body.to_ascii_lowercase();
        if CODEX_AUTH_FAILURE_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
        {
            return true;
        }
    }
    false
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

/// Grok-specific: spawn a stderr-capture thread that writes child stderr to a
/// per-slot-per-iteration file AND buffers up to [`GROK_STDERR_SNIFF_CAP_BYTES`]
/// for the post-exit auth-failure sniff. Console tee is intentionally absent —
/// grok telemetry dumps and 502 HTML no longer flood the operator console.
///
/// `capture_path`: the file to write stderr lines to. When `None` (e.g. no
/// `db_dir` was provided), lines are silently dropped from the file side only.
/// When `Some(path)` but the file cannot be opened, one `tracing::warn!` is
/// emitted and lines are dropped — the thread does NOT crash.
///
/// The sniff buffer (return value) is byte-for-byte identical to the previous
/// implementation: the `Arc<Mutex<String>>` fills from child stderr up to
/// [`GROK_STDERR_SNIFF_CAP_BYTES`], exactly as before.
fn spawn_grok_stderr_sniffer(
    child: &mut std::process::Child,
    capture_path: Option<PathBuf>,
) -> (Arc<Mutex<String>>, std::thread::JoinHandle<()>) {
    let stderr_pipe = child
        .stderr
        .take()
        .expect("stderr should be piped (Stdio::piped() was set on spawn)");
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let buf = Arc::clone(&stderr_buf);
    let handle = std::thread::spawn(move || {
        let mut file: Option<std::fs::File> = capture_path.as_ref().and_then(|p| {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::File::create(p) {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!(
                        path = %p.display(),
                        error = %e,
                        "grok stderr capture file could not be opened; lines will be dropped"
                    );
                    None
                }
            }
        });
        let reader = BufReader::new(stderr_pipe);
        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    if let Some(ref mut f) = file {
                        let _ = writeln!(f, "{line}");
                    }
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
    });
    (stderr_buf, handle)
}

fn spawn_stderr_sniffer(
    child: &mut std::process::Child,
    stderr_buf: Arc<Mutex<String>>,
    label: &'static str,
) -> std::thread::JoinHandle<()> {
    let stderr_pipe = child
        .stderr
        .take()
        .expect("stderr should be piped (Stdio::piped() was set on spawn)");
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr_pipe);
        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    ui::emit_err(&format!("{label} stderr: {line}"));
                    if let Ok(mut b) = stderr_buf.lock()
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
}

/// Compute the per-slot-per-iteration capture file path for grok stderr.
///
/// Path scheme: `<db_dir>/logs/<prefix>-<run>-<slot>-iter<N>-grok-stderr.log`
///
/// Returns `None` when `db_dir` is absent (non-loop callers that don't provide
/// a DB directory). The prefix, run-id, slot, and iteration components all fall
/// back to placeholder strings when their respective opts fields are `None` so
/// the scheme is always fully specified for callers that do provide `db_dir`.
///
/// All `&str` components are sanitized to `[A-Za-z0-9_-]` via
/// [`sanitize_path_component`] before joining, so a callsite passing a
/// `..` / `/` / NUL-bearing string cannot escape `<db_dir>/logs/`. The current
/// sources (8-char hex PRD prefix, internal UUID run-id, fixed-format slot
/// label) are already safe; this is defense-in-depth.
fn grok_stderr_capture_path(
    db_dir: Option<&Path>,
    active_prefix: Option<&str>,
    run_id: Option<&str>,
    slot_label: Option<&str>,
    iteration: Option<u32>,
) -> Option<PathBuf> {
    let db_dir = db_dir?;
    let prefix = active_prefix
        .map(|s| sanitize_path_component(s, "no-prefix"))
        .unwrap_or_else(|| "no-prefix".to_string());
    let run = run_id
        .map(|s| sanitize_path_component(s, "no-run"))
        .unwrap_or_else(|| "no-run".to_string());
    // Sanitize slot_label ("[slot 1]") to a filename-safe token ("slot1").
    let slot = slot_label
        .map(|s| sanitize_path_component(s, "noSlot"))
        .unwrap_or_else(|| "noSlot".to_string());
    let iter = iteration.unwrap_or(0);
    Some(
        db_dir
            .join("logs")
            .join(format!("{prefix}-{run}-{slot}-iter{iter}-grok-stderr.log")),
    )
}

/// Filter `s` to `[A-Za-z0-9_-]` (a single filename-safe path component).
/// Falls back to `fallback` if filtering empties the string.
///
/// Path-traversal hardening for [`grok_stderr_capture_path`]: `/`, `\`, `.`,
/// and NUL are stripped, so even a hostile caller cannot break out of
/// `<db_dir>/logs/`. `-` and `_` are preserved (UUID dashes, kebab-case run
/// ids), unlike a bare `is_alphanumeric()` filter.
fn sanitize_path_component(s: &str, fallback: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// Wire subprocess environment variables common to every LLM runner.
///
/// Sets `LOOP_ALLOW_DESTRUCTIVE`, `TASK_MGR_DIR` (canonicalized), and
/// `TASK_MGR_ACTIVE_PREFIX`; applies `current_dir`; puts the child in its
/// own process group on Unix. For Grok, also sets
/// `GROK_TELEMETRY_TRACE_UPLOAD=0` to suppress BatchSpanProcessor export
/// noise from grok's OpenTelemetry background exporter.
fn apply_common_env(
    cmd: &mut Command,
    db_dir: Option<&Path>,
    active_prefix: Option<&str>,
    working_dir: Option<&Path>,
    runner_kind: RunnerKind,
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
    if runner_kind == RunnerKind::Grok {
        cmd.env("GROK_TELEMETRY_TRACE_UPLOAD", "0");
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

/// Write `prompt` to a freshly-created temp file for runners that take the
/// prompt via a path flag (Grok's `--prompt-file`) rather than stdin.
///
/// The returned [`tempfile::NamedTempFile`] MUST be kept alive until the child
/// process has finished reading it — its `Drop` unlinks the file. Used instead
/// of an inline `-p <prompt>` arg to stay clear of Linux `MAX_ARG_STRLEN`
/// (128 KiB) on large prompts. Created mode `0600` (tempfile default): the
/// prompt may carry a full diff / proprietary code, so it stays owner-only
/// and never appears in argv (`/proc/<pid>/cmdline`).
fn write_prompt_to_tempfile(
    prompt: &str,
    binary: &str,
    provider_label: &str,
) -> TaskMgrResult<tempfile::NamedTempFile> {
    use std::io::Write;
    let ctx = |operation: String| {
        move |e: std::io::Error| TaskMgrError::IoErrorWithContext {
            file_path: binary.to_string(),
            operation,
            source: e,
        }
    };
    let mut file = tempfile::Builder::new()
        .prefix("task-mgr-prompt-")
        .suffix(".txt")
        .tempfile()
        .map_err(ctx(format!(
            "creating temp prompt file for {provider_label} subprocess"
        )))?;
    file.write_all(prompt.as_bytes()).map_err(ctx(format!(
        "writing prompt to temp file for {provider_label} subprocess"
    )))?;
    file.flush().map_err(ctx(format!(
        "flushing temp prompt file for {provider_label} subprocess"
    )))?;
    Ok(file)
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
                ui::emit_prefixed(slot_label, &line);
                buf.push_str(&line);
                buf.push('\n');
            }
            Err(e) if is_pty_read_eof(&e) => break,
            Err(e) => {
                ui::emit_prefixed(
                    slot_label,
                    &format!("Warning: error reading {provider_label} stdout: {e}"),
                );
                break;
            }
        }
    }
    buf
}

/// Canonical &'static str label for a [`RunnerCapability`] variant.
///
/// Used as the `capability_name` field of
/// [`TaskMgrError::UnsupportedRunnerCapability`]. Exhaustive match: a new
/// variant added to `RunnerCapability` forces a compile error here so the
/// label is never silently `"unknown"`.
fn capability_name(cap: RunnerCapability) -> &'static str {
    match cap {
        RunnerCapability::Effort => "Effort",
        RunnerCapability::StreamJson => "StreamJson",
        RunnerCapability::Pty => "Pty",
        RunnerCapability::DisallowedTools => "DisallowedTools",
        RunnerCapability::TitleArtifactCleanup => "TitleArtifactCleanup",
    }
}

/// Function-pointer predicate that returns `true` when an opts field
/// encodes a non-default, non-whitespace request for the paired capability.
type CapabilityFieldCheck = for<'a> fn(&RunnerOpts<'a>) -> bool;

/// Single source of truth mapping each [`RunnerCapability`] variant to the
/// [`RunnerOpts`] field whose presence asks the runner to honor that
/// capability, plus the field's snake_case name for error reporting.
///
/// Every enforced variant has exactly one row. The completeness-guard test
/// [`checks_table_covers_every_capability_variant`] iterates all variants
/// and asserts each appears here — a new variant added without a matching
/// row fails the test at unit-test time, before the silent-no-op
/// regression can reach production.
///
/// Empty / whitespace-only `Option<&str>` values are treated as "no
/// opinion" (`trim().is_empty()`) — matching the same convention used by
/// [`push_optional_flag`]. Only a meaningful value triggers enforcement.
const CHECKS: &[(RunnerCapability, CapabilityFieldCheck, &str)] = &[
    (RunnerCapability::Pty, |o| o.use_pty, "use_pty"),
    (
        RunnerCapability::StreamJson,
        |o| o.stream_json,
        "stream_json",
    ),
    (
        RunnerCapability::Effort,
        |o| o.effort.is_some_and(|e| !e.trim().is_empty()),
        "effort",
    ),
    (
        RunnerCapability::DisallowedTools,
        |o| o.disallowed_tools.is_some_and(|d| !d.trim().is_empty()),
        "disallowed_tools",
    ),
    (
        RunnerCapability::TitleArtifactCleanup,
        |o| o.cleanup_title_artifact,
        "cleanup_title_artifact",
    ),
];

/// Refuse a dispatch whose [`RunnerOpts`] sets a capability-driven field
/// the chosen runner does not support, before any subprocess is spawned.
///
/// Walks [`CHECKS`] in order; the first row whose `field_is_set` predicate
/// fires AND for which `runner.supports(cap)` is `false` returns
/// [`TaskMgrError::UnsupportedRunnerCapability`] carrying the runner kind,
/// canonical capability label, and the opts field name.
///
/// `runner` is `&dyn LlmRunner` purely for the duration of this call so a
/// single body can be runner-polymorphic; the hot spawn path in
/// [`dispatch`] remains static-dispatch on [`RunnerKind`].
fn enforce_capabilities(
    runner: &dyn LlmRunner,
    kind: RunnerKind,
    opts: &RunnerOpts<'_>,
) -> TaskMgrResult<()> {
    for (cap, is_set, field_name) in CHECKS {
        if is_set(opts) && !runner.supports(*cap) {
            return Err(TaskMgrError::UnsupportedRunnerCapability {
                runner_kind: kind,
                capability_name: capability_name(*cap),
                field_name,
            });
        }
    }
    Ok(())
}

/// Route a runner invocation to the correct backend, enforcing capability
/// constraints pre-spawn and cleaning up per-session artifacts post-spawn.
///
/// `RunnerKind::Claude` → [`ClaudeRunner::spawn`]; `RunnerKind::Grok` →
/// [`GrokRunner::spawn`].
///
/// **Pre-spawn**: `enforce_capabilities` walks the `CHECKS` registry. Any
/// `RunnerOpts` field encoding a capability the chosen runner does not support
/// returns [`TaskMgrError::UnsupportedRunnerCapability`] before any subprocess
/// is launched (fail-closed). See `enforce_capabilities` and `CHECKS`.
///
/// **Post-spawn**: after spawn returns, [`LlmRunner::cleanup_session`] is
/// called for the `(session_id, cwd)` tuple when `RunnerResult::session_id`
/// is `Some`. Cleanup runs on both clean-exit and non-zero-exit spawns. The
/// return value is always the spawn's original `RunnerResult` / `Err`,
/// never modified by cleanup.
///
/// `cwd` is `opts.working_dir` when `Some`; falls back to
/// `std::env::current_dir()`, then `PathBuf::default()` if that fails.
///
/// # Errors
///
/// Returns whatever the underlying backend returns. Grok adds one provider-
/// specific error variant ([`TaskMgrError::GrokAuthFailure`]); Claude has no
/// equivalent. Either backend may surface
/// [`TaskMgrError::UnsupportedRunnerCapability`] from the pre-spawn check.
///
/// # Best-effort limitation (PRD §6)
///
/// When spawn itself returns `Err` (e.g. binary not found), the session UUID
/// is not surfaced in the error type, so cleanup is skipped. In practice no
/// artifact is written because the CLI never ran.
///
/// When `cleanup_session` returns `Err`, a single `[cleanup warn] <provider>:
/// <error> (<cwd>)` line is emitted to stderr via [`CLEANUP_WARN_ONCE`];
/// subsequent errors in the same process are silent.
pub fn dispatch(
    kind: RunnerKind,
    prompt: &str,
    permission_mode: &PermissionMode,
    opts: RunnerOpts<'_>,
) -> TaskMgrResult<RunnerResult> {
    // Resolve cwd once for post-spawn cleanup (FEAT-006).
    // The brief &dyn LlmRunner borrow inside enforce_capabilities keeps capability enforcement
    // runner-polymorphic without putting Box<dyn LlmRunner> on the hot path.
    let cwd: PathBuf = opts
        .working_dir
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let result = match kind {
        RunnerKind::Claude => {
            enforce_capabilities(&ClaudeRunner, kind, &opts)?;
            ClaudeRunner.spawn(prompt, permission_mode, opts)
        }
        RunnerKind::Grok => {
            enforce_capabilities(&GrokRunner, kind, &opts)?;
            GrokRunner.spawn(prompt, permission_mode, opts)
        }
        RunnerKind::Codex => {
            enforce_capabilities(&CodexRunner, kind, &opts)?;
            CodexRunner.spawn(prompt, permission_mode, opts)
        }
    };
    if let Ok(ref r) = result
        && let Some(sid) = r.session_id
    {
        let cleanup_result = match kind {
            RunnerKind::Claude => ClaudeRunner.cleanup_session(sid, &cwd),
            RunnerKind::Grok => GrokRunner.cleanup_session(sid, &cwd),
            RunnerKind::Codex => CodexRunner.cleanup_session(sid, &cwd),
        };
        if let Err(ref e) = cleanup_result
            && !CLEANUP_WARN_ONCE.swap(true, Ordering::Relaxed)
        {
            let provider = match kind {
                RunnerKind::Claude => "claude",
                RunnerKind::Grok => "grok",
                RunnerKind::Codex => "codex",
            };
            tracing::warn!(
                provider = %provider,
                error = %e,
                cwd = %cwd.display(),
                "session cleanup failed",
            );
        }
    }
    result
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

    /// AC: every (runner × capability) pair matches the declared support
    /// matrix. 2 runners × 4 capabilities = 8 assertions.
    ///
    /// The expected table is written inline (not derived from
    /// `supports()`) so a copy-paste flip of any bit in either production
    /// impl produces a test failure that names the offending pair.
    #[test]
    fn supports_matrix_matches_declared_capability_table() {
        use RunnerCapability::*;
        let expectations: &[(&str, &dyn LlmRunner, RunnerCapability, bool)] = &[
            ("claude", &ClaudeRunner, Effort, true),
            ("claude", &ClaudeRunner, StreamJson, true),
            ("claude", &ClaudeRunner, Pty, true),
            ("claude", &ClaudeRunner, DisallowedTools, true),
            ("claude", &ClaudeRunner, TitleArtifactCleanup, true),
            ("grok", &GrokRunner, Effort, true),
            ("grok", &GrokRunner, StreamJson, true),
            ("grok", &GrokRunner, Pty, false),
            ("grok", &GrokRunner, DisallowedTools, true),
            ("grok", &GrokRunner, TitleArtifactCleanup, false),
        ];
        for (name, runner, cap, expected) in expectations {
            assert_eq!(
                runner.supports(*cap),
                *expected,
                "{name}.supports({cap:?}) expected {expected}",
            );
        }
    }

    /// AC#4: enforcement completeness guard. Every `RunnerCapability` variant
    /// MUST appear in [`CHECKS`]; otherwise its field silently no-ops at
    /// dispatch and FEAT-003's contract is violated.
    ///
    /// `all_variants` is hand-rolled (no `strum` dep). The companion
    /// `assert_exhaustive` match has NO wildcard arm — a new variant added
    /// without updating this list fails to compile, which in turn fails the
    /// runtime "every variant in CHECKS" assertion below.
    #[test]
    fn checks_table_covers_every_capability_variant() {
        use RunnerCapability::*;
        let all_variants: &[RunnerCapability] = &[
            Effort,
            StreamJson,
            Pty,
            DisallowedTools,
            TitleArtifactCleanup,
        ];
        fn assert_exhaustive(cap: RunnerCapability) {
            match cap {
                Effort | StreamJson | Pty | DisallowedTools | TitleArtifactCleanup => {}
            }
        }
        for v in all_variants {
            assert_exhaustive(*v);
            assert!(
                CHECKS.iter().any(|(c, _, _)| c == v),
                "RunnerCapability::{v:?} missing from CHECKS registry — \
                 add a row in runner.rs::CHECKS or the dispatch guard \
                 will silently ignore its RunnerOpts field"
            );
        }
        assert_eq!(
            CHECKS.len(),
            all_variants.len(),
            "CHECKS must contain exactly one row per variant; \
             duplicate or stale rows mean enforcement order matters in ways \
             the surface contract doesn't promise"
        );
    }

    /// AC#6: `capability_name` returns the canonical label per variant.
    /// Failure here means [`TaskMgrError::UnsupportedRunnerCapability`]'s
    /// `capability_name` field carries the wrong string for operators.
    #[test]
    fn capability_name_returns_canonical_labels() {
        assert_eq!(capability_name(RunnerCapability::Effort), "Effort");
        assert_eq!(capability_name(RunnerCapability::StreamJson), "StreamJson");
        assert_eq!(capability_name(RunnerCapability::Pty), "Pty");
        assert_eq!(
            capability_name(RunnerCapability::DisallowedTools),
            "DisallowedTools"
        );
    }

    /// AC#7 (Grok × use_pty: true) → Err(UnsupportedRunnerCapability {…}).
    /// Asserts the specific error variant AND every static-string field so
    /// a regression that flipped capability_name or field_name would fail
    /// here, not just produce a vague Err.
    #[test]
    fn enforce_capabilities_grok_use_pty_rejected() {
        let opts = RunnerOpts {
            use_pty: true,
            ..RunnerOpts::default()
        };
        let err = enforce_capabilities(&GrokRunner, RunnerKind::Grok, &opts)
            .expect_err("Grok must refuse use_pty: true");
        match err {
            TaskMgrError::UnsupportedRunnerCapability {
                runner_kind,
                capability_name,
                field_name,
            } => {
                assert_eq!(runner_kind, RunnerKind::Grok);
                assert_eq!(capability_name, "Pty");
                assert_eq!(field_name, "use_pty");
            }
            other => panic!("expected UnsupportedRunnerCapability, got {other:?}"),
        }
    }

    /// AC#7 (Claude × use_pty: true) → Ok. Claude supports PTY; the field
    /// being set is fine.
    #[test]
    fn enforce_capabilities_claude_use_pty_accepted() {
        let opts = RunnerOpts {
            use_pty: true,
            ..RunnerOpts::default()
        };
        enforce_capabilities(&ClaudeRunner, RunnerKind::Claude, &opts)
            .expect("Claude must accept use_pty: true");
    }

    /// AC#7 (both × RunnerOpts::default()) → Ok. No capability-driven
    /// field is set, so no row in CHECKS fires.
    #[test]
    fn enforce_capabilities_default_opts_always_ok() {
        enforce_capabilities(&ClaudeRunner, RunnerKind::Claude, &RunnerOpts::default())
            .expect("Claude must accept RunnerOpts::default()");
        enforce_capabilities(&GrokRunner, RunnerKind::Grok, &RunnerOpts::default())
            .expect("Grok must accept RunnerOpts::default()");
    }

    /// AC#7 (Claude × stream_json: true) and (Grok × stream_json: true) → Ok.
    /// Both runners declare `StreamJson` support.
    #[test]
    fn enforce_capabilities_stream_json_both_ok() {
        let opts = RunnerOpts {
            stream_json: true,
            ..RunnerOpts::default()
        };
        enforce_capabilities(&ClaudeRunner, RunnerKind::Claude, &opts)
            .expect("Claude must accept stream_json: true");
        enforce_capabilities(&GrokRunner, RunnerKind::Grok, &opts)
            .expect("Grok must accept stream_json: true");
    }

    /// AC#5 + AC#7: `Some("")` is treated as "no opinion" by the
    /// `trim().is_empty()` predicate; both runners accept it regardless of
    /// declared support.
    #[test]
    fn enforce_capabilities_effort_empty_value_is_no_opinion() {
        let opts = RunnerOpts {
            effort: Some(""),
            ..RunnerOpts::default()
        };
        enforce_capabilities(&ClaudeRunner, RunnerKind::Claude, &opts)
            .expect("empty effort must be treated as unset");
        enforce_capabilities(&GrokRunner, RunnerKind::Grok, &opts)
            .expect("empty effort must be treated as unset");
    }

    /// AC#5 + AC#7: whitespace-only effort is also "no opinion". Same
    /// convention as [`push_optional_flag`] — a stray space from a config
    /// file must not trip enforcement.
    #[test]
    fn enforce_capabilities_effort_whitespace_is_no_opinion() {
        let opts = RunnerOpts {
            effort: Some("  "),
            ..RunnerOpts::default()
        };
        enforce_capabilities(&ClaudeRunner, RunnerKind::Claude, &opts)
            .expect("whitespace-only effort must be treated as unset");
        enforce_capabilities(&GrokRunner, RunnerKind::Grok, &opts)
            .expect("whitespace-only effort must be treated as unset");
    }

    /// AC#5 + AC#7: same "no opinion" treatment for `disallowed_tools`
    /// (the other Option<&str> field in the registry).
    #[test]
    fn enforce_capabilities_disallowed_tools_empty_is_no_opinion() {
        let opts = RunnerOpts {
            disallowed_tools: Some(""),
            ..RunnerOpts::default()
        };
        enforce_capabilities(&ClaudeRunner, RunnerKind::Claude, &opts)
            .expect("empty disallowed_tools must be treated as unset");
        enforce_capabilities(&GrokRunner, RunnerKind::Grok, &opts)
            .expect("empty disallowed_tools must be treated as unset");
    }

    /// AC#1: `dispatch` calls `enforce_capabilities` BEFORE the spawn
    /// match. We assert this by setting `use_pty: true` on a Grok dispatch
    /// with no `GROK_BINARY` configured — if enforcement ran AFTER spawn we
    /// would see an `IoErrorWithContext` from the missing binary; instead we
    /// must see `UnsupportedRunnerCapability` because the check fires first.
    ///
    /// No mutex needed: the Err path returns before any env read.
    #[test]
    fn dispatch_grok_use_pty_returns_unsupported_capability_before_spawn() {
        let perm = scoped_coding();
        let err = dispatch(
            RunnerKind::Grok,
            "probe",
            &perm,
            RunnerOpts {
                use_pty: true,
                ..RunnerOpts::default()
            },
        )
        .expect_err("dispatch must reject Grok + use_pty before spawning");
        match err {
            TaskMgrError::UnsupportedRunnerCapability {
                runner_kind,
                capability_name,
                field_name,
            } => {
                assert_eq!(runner_kind, RunnerKind::Grok);
                assert_eq!(capability_name, "Pty");
                assert_eq!(field_name, "use_pty");
            }
            other => panic!(
                "enforcement must run before spawn; got {other:?} \
                 (an IoErrorWithContext here would mean dispatch reached \
                 the spawn match before the capability check)"
            ),
        }
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
    ///
    /// Reads the prompt from the `--prompt-file <path>` arg (not stdin): the
    /// real GrokRunner delivers the prompt via that flag with stdin set to
    /// null, so a `cat`-from-stdin mock would see an empty prompt.
    fn make_grok_marker_script(name: &str, marker: &str) -> std::path::PathBuf {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let path = std::env::temp_dir().join(format!("task_mgr_grok_test_{name}_marker.sh"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, "PROMPT=''").unwrap();
            writeln!(f, "while [ $# -gt 0 ]; do").unwrap();
            writeln!(f, "  case \"$1\" in").unwrap();
            writeln!(f, "    --prompt-file) PROMPT=$(cat \"$2\"); shift 2 ;;").unwrap();
            writeln!(f, "    *) shift ;;").unwrap();
            writeln!(f, "  esac").unwrap();
            writeln!(f, "done").unwrap();
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

    /// TEST-INIT-002 — cleanup_title_artifact: true is rejected at dispatch for
    /// Grok with `UnsupportedRunnerCapability`. Grok has no ai-title-jsonl leak;
    /// `TitleArtifactCleanup` is `false` on `GrokRunner`. No subprocess should
    /// be launched — dispatch must return Err before spawning.
    #[test]
    fn dispatch_grok_rejects_cleanup_title_artifact() {
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
        match result {
            Err(TaskMgrError::UnsupportedRunnerCapability {
                runner_kind,
                capability_name,
                field_name,
            }) => {
                assert_eq!(runner_kind, RunnerKind::Grok);
                assert_eq!(capability_name, "TitleArtifactCleanup");
                assert_eq!(field_name, "cleanup_title_artifact");
            }
            other => panic!("expected UnsupportedRunnerCapability, got {other:?}"),
        }
    }

    /// `RunnerKind::supports` must agree with the zero-sized runner trait
    /// impls for every capability — it exists so engine call sites can shape
    /// `RunnerOpts` to the selected runner without a trait object, and a drift
    /// between the two would silently re-introduce the dispatch-rejection bug.
    #[test]
    fn runner_kind_supports_matches_trait_impls() {
        use RunnerCapability::*;
        for cap in [
            Effort,
            StreamJson,
            Pty,
            DisallowedTools,
            TitleArtifactCleanup,
        ] {
            assert_eq!(
                RunnerKind::Claude.supports(cap),
                ClaudeRunner.supports(cap),
                "RunnerKind::Claude.supports({cap:?}) drifted from ClaudeRunner",
            );
            assert_eq!(
                RunnerKind::Grok.supports(cap),
                GrokRunner.supports(cap),
                "RunnerKind::Grok.supports({cap:?}) drifted from GrokRunner",
            );
        }
        // Spot-check the specific bit that caused the production crash.
        assert!(!RunnerKind::Grok.supports(TitleArtifactCleanup));
        assert!(RunnerKind::Claude.supports(TitleArtifactCleanup));
    }

    /// Mock grok that records its argv (one arg per line) to the file named
    /// by `GROK_ARGV_OUT`, then prints a marker. Lets tests assert the exact
    /// flags GrokRunner passes without coupling to spawn internals.
    fn make_grok_argv_recorder() -> std::path::PathBuf {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let path = std::env::temp_dir().join("task_mgr_grok_argv_recorder.sh");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#": > "$GROK_ARGV_OUT""#).unwrap();
            writeln!(
                f,
                r#"for a in "$@"; do printf '%s\n' "$a" >> "$GROK_ARGV_OUT"; done"#
            )
            .unwrap();
            writeln!(f, r#"echo "ARGV_RECORDED""#).unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Run `dispatch(Grok)` against the argv-recorder mock and return the
    /// recorded argv lines. Holds [`GROK_BINARY_MUTEX`]; cleans up env + files.
    fn dispatch_grok_recording_argv(
        prompt: &str,
        perm: &PermissionMode,
        stream_json: bool,
    ) -> Vec<String> {
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let script = make_grok_argv_recorder();
        let argv_out = std::env::temp_dir().join("task_mgr_grok_argv_out.txt");
        unsafe { std::env::set_var("GROK_BINARY", script.to_str().unwrap()) };
        unsafe { std::env::set_var("GROK_ARGV_OUT", argv_out.to_str().unwrap()) };
        let result = dispatch(
            RunnerKind::Grok,
            prompt,
            perm,
            RunnerOpts {
                stream_json,
                ..RunnerOpts::default()
            },
        );
        unsafe { std::env::remove_var("GROK_BINARY") };
        unsafe { std::env::remove_var("GROK_ARGV_OUT") };
        let argv = std::fs::read_to_string(&argv_out).unwrap_or_default();
        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_file(&argv_out);
        result.expect("dispatch(Grok) returned Err");
        argv.lines().map(str::to_owned).collect()
    }

    /// Regression: GrokRunner must NOT pass Claude's `--verbose` flag (grok
    /// rejects it — its only near-match is `-v/--version`), must request
    /// streaming output via `--output-format streaming-json`, and must deliver
    /// the prompt via `--prompt-file` (never a bare `-p`/`--single`, which
    /// grok treats as a value-less flag and which would also blow
    /// `MAX_ARG_STRLEN` on a large prompt).
    #[test]
    fn grok_stream_args_omit_verbose_and_deliver_prompt_via_file() {
        let perm = scoped_coding();
        let argv = dispatch_grok_recording_argv("probe-prompt", &perm, true);
        assert!(
            !argv.iter().any(|a| a == "--verbose"),
            "grok must not receive --verbose, got {argv:?}",
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--output-format" && w[1] == "streaming-json"),
            "stream_json mode must pass `--output-format streaming-json`, got {argv:?}",
        );
        assert!(
            argv.iter().any(|a| a == "--prompt-file"),
            "grok must deliver the prompt via --prompt-file, got {argv:?}",
        );
        assert!(
            !argv.iter().any(|a| a == "-p" || a == "--single"),
            "grok must not use a bare -p/--single prompt flag, got {argv:?}",
        );
        // WORKAROUND(grok-cli-headless-subagent-coordinator): headless grok must
        // disable subagent spawning — a "use the <X> agent" review instruction
        // otherwise aborts the parent session at turn 0 when the coordinator
        // cancels the background spawn. See GrokRunner::spawn.
        assert!(
            argv.iter().any(|a| a == "--no-subagents"),
            "grok must receive --no-subagents so it reviews inline, got {argv:?}",
        );
    }

    /// In plain (non-stream) mode grok omits the stream-format flags entirely,
    /// but still delivers the prompt via `--prompt-file`.
    #[test]
    fn grok_plain_args_omit_stream_flags_but_keep_prompt_file() {
        let perm = scoped_coding();
        let argv = dispatch_grok_recording_argv("probe-prompt", &perm, false);
        assert!(
            !argv
                .iter()
                .any(|a| a == "--output-format" || a == "streaming-json" || a == "--verbose"),
            "plain mode must omit stream-format flags, got {argv:?}",
        );
        assert!(
            argv.iter().any(|a| a == "--prompt-file"),
            "prompt is always delivered via --prompt-file, got {argv:?}",
        );
    }

    /// A prompt larger than Linux `MAX_ARG_STRLEN` (128 KiB) must round-trip
    /// intact. An inline `-p <prompt>` would fail with `E2BIG` here — this is
    /// the regression that drove file-based delivery. The tail sentinel
    /// confirms the *entire* prompt survived, not a truncated prefix.
    #[test]
    fn grok_delivers_oversized_prompt_via_prompt_file() {
        let perm = scoped_coding();
        let prompt = format!("{}TAIL_SENTINEL_OK", "x".repeat(200 * 1024));
        let result = spawn_grok_echo(&prompt, &perm, false).expect("dispatch(Grok) returned Err");
        assert_eq!(result.exit_code, 0, "expected clean exit, got {result:?}");
        assert!(
            result.output.contains("TAIL_SENTINEL_OK"),
            "oversized prompt tail must survive the --prompt-file round-trip",
        );
    }

    /// Regression for the engine call-site bug: both dispatch sites once
    /// hardcoded `cleanup_title_artifact: true`, which `dispatch` rejects for
    /// Grok. The fix gates the field on [`RunnerKind::supports`]. This asserts
    /// the gated value resolves to `false` for Grok AND that dispatch then
    /// proceeds to spawn rather than returning `UnsupportedRunnerCapability`.
    #[test]
    fn grok_dispatch_accepts_capability_gated_cleanup_title_artifact() {
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let gated = RunnerKind::Grok.supports(RunnerCapability::TitleArtifactCleanup);
        assert!(!gated, "Grok must not advertise TitleArtifactCleanup");
        let script = make_grok_marker_script("cleanup_gate", "GATE_OK");
        unsafe { std::env::set_var("GROK_BINARY", script.to_str().unwrap()) };
        let perm = scoped_coding();
        let result = dispatch(
            RunnerKind::Grok,
            "gate-probe",
            &perm,
            RunnerOpts {
                cleanup_title_artifact: gated,
                ..RunnerOpts::default()
            },
        );
        unsafe { std::env::remove_var("GROK_BINARY") };
        let _ = std::fs::remove_file(&script);
        let r = result.expect("dispatch(Grok) must not reject a capability-gated cleanup flag");
        assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
    }

    /// TEST-INIT-002 — GrokRunner discovers session id from pre/post dir diff.
    ///
    /// The mock grok binary reads `GROK_TEST_SESSION_DIR` and creates a fixed
    /// UUID subdirectory there (simulating what the real Grok CLI does). After
    /// child exit, GrokRunner computes the diff and populates
    /// `RunnerResult::session_id` with the discovered UUID.
    ///
    /// Lock ordering: `RUNNER_HOME_ENV_MUTEX` first, then `GROK_BINARY_MUTEX`.
    /// Any future test that holds both must use the same order to avoid deadlock.
    #[test]
    fn grok_runner_discovers_session_id_from_pre_post_dir_diff() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        // HOME mutex first, then GROK_BINARY mutex — consistent acquisition order.
        let _guard_home = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard_bin = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        // Pre-create the encoded session dir (empty). Simulates a cwd with prior
        // Grok activity but no session dirs from THIS run.
        let session_dir = grok_encoded_session_dir(&fake_cwd, &fake_home);
        std::fs::create_dir_all(&session_dir).unwrap();

        // Known UUID the mock binary creates as a subdir during its execution.
        let expected_uuid_str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let expected_uuid = Uuid::parse_str(expected_uuid_str).unwrap();

        // Mock grok: create $GROK_TEST_SESSION_DIR/<uuid>/ then exit 0.
        let script_path = std::env::temp_dir().join("task_mgr_grok_session_id_capture_test.sh");
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(
                f,
                r#"mkdir -p "$GROK_TEST_SESSION_DIR/{expected_uuid_str}""#
            )
            .unwrap();
            writeln!(f, r#"echo "session-capture-ok""#).unwrap();
        }
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        unsafe { std::env::set_var("GROK_BINARY", script_path.to_str().unwrap()) };
        unsafe { std::env::set_var("GROK_TEST_SESSION_DIR", session_dir.to_str().unwrap()) };
        let _home = RunnerHomeGuard::set(&fake_home);

        let perm = scoped_coding();
        let result = dispatch(
            RunnerKind::Grok,
            "session-capture-probe",
            &perm,
            RunnerOpts {
                working_dir: Some(&fake_cwd),
                ..RunnerOpts::default()
            },
        );

        unsafe { std::env::remove_var("GROK_BINARY") };
        unsafe { std::env::remove_var("GROK_TEST_SESSION_DIR") };
        let _ = std::fs::remove_file(&script_path);

        let r = result.expect("dispatch(Grok) returned Err");
        assert_eq!(r.exit_code, 0, "expected clean exit, got {r:?}");
        assert_eq!(
            r.session_id,
            Some(expected_uuid),
            "GrokRunner must discover session_id from pre/post dir diff, got {:?}",
            r.session_id
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

    /// AC #7 (positive): a structured `[Error: 401 unauthorized]` line in the
    /// conversation transcript MUST classify as auth failure.
    #[test]
    fn codex_conversation_auth_positive_structured_401() {
        assert!(codex_conversation_indicates_auth_failure(
            "[Error: 401 unauthorized]"
        ));
    }

    /// AC #9 (positive): the broader marker list (V1's 10) covers bearer /
    /// api-key phrasings that V2's 6-marker list missed.
    #[test]
    fn codex_conversation_auth_positive_bearer_and_api_key() {
        assert!(codex_conversation_indicates_auth_failure(
            "[Error: missing bearer]"
        ));
        assert!(codex_conversation_indicates_auth_failure(
            "[Error: invalid api key]"
        ));
        assert!(codex_conversation_indicates_auth_failure(
            "[Error: invalid bearer token]"
        ));
        assert!(codex_conversation_indicates_auth_failure(
            "[Error: not authenticated]"
        ));
        assert!(codex_conversation_indicates_auth_failure(
            "[Error: missing-bearer]"
        ));
    }

    /// AC #5 + #10 (negative-control): a transcript whose AGENT TEXT mentions
    /// "HTTP 401" must NOT be classified as auth failure. A naive substring
    /// scan over the full transcript would return true; the structured
    /// `[Error: ` matcher correctly returns false because agent text lacks
    /// the `[Error: ` prefix.
    #[test]
    fn codex_conversation_auth_negative_agent_text_quoting_401() {
        let conversation = "\
Assistant: I received an HTTP 401 response from the upstream service.\n\
[ToolResult] Curl returned: HTTP/1.1 401 Unauthorized\n\
Assistant: I'll retry with credentials.\n";
        assert!(!codex_conversation_indicates_auth_failure(conversation));
    }

    /// Negative: empty conversation and a transcript with no `[Error: ]`
    /// lines must both classify as non-auth.
    #[test]
    fn codex_conversation_auth_negative_empty_and_no_error_lines() {
        assert!(!codex_conversation_indicates_auth_failure(""));
        assert!(!codex_conversation_indicates_auth_failure(
            "Assistant: nothing structured here\n"
        ));
        // An `[Error: ]` line whose body is unrelated must NOT trip.
        assert!(!codex_conversation_indicates_auth_failure(
            "[Error: file not found]"
        ));
        assert!(!codex_conversation_indicates_auth_failure(
            "[Error: rate limit exceeded]"
        ));
    }

    /// Case-insensitivity + leading whitespace tolerance: the matcher
    /// trim_start's each line and lowercases the body before scanning.
    #[test]
    fn codex_conversation_auth_case_and_whitespace() {
        assert!(codex_conversation_indicates_auth_failure(
            "  [Error: AUTHENTICATION REQUIRED]"
        ));
        assert!(codex_conversation_indicates_auth_failure(
            "\t[Error: Login Required to continue]"
        ));
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

    /// W3: broaden auth-failure coverage so the short-circuit doesn't silently
    /// fail open when grok phrases the rejection differently across CLI
    /// versions. Each line below contains only ONE of the W3-added phrases
    /// (no overlap with the original three) so it asserts the new entry
    /// independently.
    #[test]
    fn stderr_contains_auth_failure_w3_broader_phrasing() {
        // "login required" alone (without "grok ") — newer CLIs may drop the prefix.
        assert!(stderr_contains_auth_failure(
            "Error: login required to use this command"
        ));
        // "authentication required" — generic phrasing.
        assert!(stderr_contains_auth_failure(
            "Error: authentication required"
        ));
        // "authentication failed" — credentials present but rejected.
        assert!(stderr_contains_auth_failure("AUTHENTICATION FAILED"));
        // Bare "unauthorized" without a leading 401.
        assert!(stderr_contains_auth_failure("HTTP error: Unauthorized"));
        // Bare 401 without "unauthorized" word.
        assert!(stderr_contains_auth_failure("server returned 401"));
        // API-key variants — relevant if grok ever surfaces upstream xAI errors.
        assert!(stderr_contains_auth_failure("Invalid API key"));
        assert!(stderr_contains_auth_failure("api key not found"));
        assert!(stderr_contains_auth_failure("Missing API key in request"));

        // Negative controls — common error phrases that must NOT match.
        assert!(!stderr_contains_auth_failure("file not found"));
        assert!(!stderr_contains_auth_failure("rate limit exceeded"));
        assert!(!stderr_contains_auth_failure("internal server error (500)"));
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

    /// Test double for capability-gate unit tests.
    ///
    /// Callers set `supports_fn` to any closure-compatible `fn(RunnerCapability) -> bool`
    /// so each test can construct an arbitrary capability matrix without touching
    /// a production runner. `spawn` is unreachable in capability-gate tests;
    /// it exists only to satisfy `LlmRunner`.
    struct CapabilityFakeRunner {
        pub supports_fn: fn(RunnerCapability) -> bool,
    }

    impl LlmRunner for CapabilityFakeRunner {
        fn supports(&self, cap: RunnerCapability) -> bool {
            (self.supports_fn)(cap)
        }

        fn spawn(
            &self,
            _prompt: &str,
            _permission_mode: &PermissionMode,
            _opts: RunnerOpts<'_>,
        ) -> TaskMgrResult<RunnerResult> {
            unreachable!(
                "CapabilityFakeRunner exists only for capability-gate tests; spawn is never called"
            )
        }
    }

    // ---------------------------------------------------------------------
    // TEST-INIT-001 — TDD scaffolding for FEAT-001/-002.
    //
    // These tests pin the cleanup-side contracts of the Phase 1 trait
    // hygiene work. Each test names the FEAT that owns its acceptance
    // criterion in the `#[ignore]` reason (learning #2813); the gating
    // FEAT task removes the `#[ignore]` line as part of its acceptance.
    //
    // Why some tests remain #[ignore]d:
    //   - `cleanup_claude_session_artifact` is fully promoted (FEAT-001 done).
    //     The three FEAT-001 tests are now live.
    //   - `grok_encoded_session_dir` uses `urlencoding::encode` (FEAT-002 done).
    //     All three grok round-trip tests are now live.
    // ---------------------------------------------------------------------

    /// Serializes env-var mutation across HOME-sensitive tests; HOME is
    /// process-global and leaking it into concurrent tests would make them
    /// flaky. Mirrors the pattern at `claude.rs:3056` so the two test
    /// modules don't race against each other when both touch HOME.
    static RUNNER_HOME_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Restores HOME (or unsets it) on drop, so a failed assertion doesn't
    /// leak the fake HOME into subsequent tests. Mirrors `claude.rs:3060`.
    struct RunnerHomeGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl RunnerHomeGuard {
        fn set(value: &Path) -> Self {
            let previous = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", value) };
            Self { previous }
        }
    }

    impl Drop for RunnerHomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    // -----------------------------------------------------------------
    // Test infrastructure: FakeRunner
    //
    // `#[cfg(test)]` only — NOT a production dry-run mode (PRD non-goals §5).
    // Production dispatch always routes through `RunnerKind` → a real CLI binary.
    //
    // Design: closures for both spawn and cleanup results so tests capture
    // any pre-built RunnerResult or error without needing RunnerResult: Clone.
    // -----------------------------------------------------------------

    /// Test-only seam implementing [`LlmRunner`] without invoking a real CLI subprocess.
    ///
    /// Use this struct to verify dispatch's post-spawn cleanup hook and any other
    /// code that calls through the `LlmRunner` trait. It is `#[cfg(test)]` only —
    /// NOT a production dry-run mode (PRD non-goals §5).
    ///
    /// # Construction
    ///
    /// ```ignore
    /// let recorder = Arc::new(Mutex::new(Vec::new()));
    /// let runner = FakeRunner::new(
    ///     Box::new(|| Ok(RunnerResult { exit_code: 0, output: "ok".into(), … })),
    ///     Some(Box::new(|cwd| { std::fs::write(cwd.join("uuid.jsonl"), "").unwrap(); })),
    ///     Arc::clone(&recorder),
    ///     Box::new(|| Ok(())),
    /// );
    /// ```
    pub(super) type SpawnFn = Box<dyn Fn() -> TaskMgrResult<RunnerResult> + Send + Sync>;
    pub(super) type ArtifactFn = Box<dyn Fn(&Path) + Send + Sync>;
    pub(super) type CleanupFn = Box<dyn Fn() -> TaskMgrResult<()> + Send + Sync>;

    pub(super) struct FakeRunner {
        /// Returns the RunnerResult (or Err) for each `spawn()` call.
        spawn_fn: SpawnFn,
        /// Optional side-effect closure invoked inside `spawn()` BEFORE returning,
        /// simulating what the real CLI writes to disk (e.g. a `<uuid>.jsonl` for
        /// Claude, or a `<uuid>/` directory for Grok). Receives the cwd `&Path`.
        artifact_fn: Option<ArtifactFn>,
        /// Records every `(session_id, cwd)` pair passed to `cleanup_session()`.
        cleanup_recorder: Arc<Mutex<Vec<(Uuid, PathBuf)>>>,
        /// Returns `Ok(())` or `Err(…)` for each `cleanup_session()` call.
        cleanup_fn: CleanupFn,
    }

    impl FakeRunner {
        pub(super) fn new(
            spawn_fn: SpawnFn,
            artifact_fn: Option<ArtifactFn>,
            cleanup_recorder: Arc<Mutex<Vec<(Uuid, PathBuf)>>>,
            cleanup_fn: CleanupFn,
        ) -> Self {
            Self {
                spawn_fn,
                artifact_fn,
                cleanup_recorder,
                cleanup_fn,
            }
        }
    }

    impl LlmRunner for FakeRunner {
        fn spawn(
            &self,
            _prompt: &str,
            _permission_mode: &PermissionMode,
            opts: RunnerOpts<'_>,
        ) -> TaskMgrResult<RunnerResult> {
            let cwd = opts
                .working_dir
                .map(|p| p.to_path_buf())
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            if let Some(ref f) = self.artifact_fn {
                f(&cwd);
            }
            (self.spawn_fn)()
        }

        fn cleanup_session(&self, session_id: Uuid, cwd: &Path) -> TaskMgrResult<()> {
            if let Ok(mut recorder) = self.cleanup_recorder.lock() {
                recorder.push((session_id, cwd.to_path_buf()));
            }
            (self.cleanup_fn)()
        }
    }

    /// AC (FEAT-008): `cleanup_session` pushes the exact `(session_id, cwd)` pair
    /// onto the recorder and returns the configured `Ok(())`.
    #[test]
    fn fake_runner_cleanup_records_session_and_cwd() {
        let recorder: Arc<Mutex<Vec<(Uuid, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));
        let runner = FakeRunner::new(
            Box::new(|| {
                Ok(RunnerResult {
                    exit_code: 0,
                    output: String::new(),
                    conversation: None,
                    timed_out: false,
                    completion_killed: false,
                    permission_denials: Vec::new(),
                    session_id: None,
                })
            }),
            None,
            Arc::clone(&recorder),
            Box::new(|| Ok(())),
        );

        let session_id = Uuid::new_v4();
        let cwd = Path::new("/tmp/fake-runner-test-cwd");
        let result = runner.cleanup_session(session_id, cwd);

        assert!(
            result.is_ok(),
            "cleanup_fn=Ok must return Ok, got {result:?}"
        );
        let recorded = recorder.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "one cleanup_session call must push one entry"
        );
        assert_eq!(
            recorded[0].0, session_id,
            "recorded session_id must match arg"
        );
        assert_eq!(
            recorded[0].1,
            PathBuf::from(cwd),
            "recorded cwd must match arg"
        );
    }

    /// AC (FEAT-008): configuring `cleanup_fn` to return `Err` is surfaced to the
    /// caller unchanged — the recorder still captures the call first.
    #[test]
    fn fake_runner_cleanup_returns_configured_err() {
        let recorder: Arc<Mutex<Vec<(Uuid, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));
        let runner = FakeRunner::new(
            Box::new(|| {
                Ok(RunnerResult {
                    exit_code: 0,
                    output: String::new(),
                    conversation: None,
                    timed_out: false,
                    completion_killed: false,
                    permission_denials: Vec::new(),
                    session_id: None,
                })
            }),
            None,
            Arc::clone(&recorder),
            Box::new(|| {
                Err(TaskMgrError::IoErrorWithContext {
                    file_path: "/fake/path".to_string(),
                    operation: "fake cleanup error".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "fake denied",
                    ),
                })
            }),
        );

        let session_id = Uuid::new_v4();
        let result = runner.cleanup_session(session_id, Path::new("/tmp"));
        assert!(
            result.is_err(),
            "cleanup_fn=Err must return Err, got {result:?}"
        );
        // Recorder still captures the call even when cleanup returns Err.
        let recorded = recorder.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "recorder must capture the call even on cleanup Err"
        );
    }

    /// AC (FEAT-008): `spawn` invokes `artifact_fn` with the resolved cwd BEFORE
    /// returning the templated `RunnerResult`. Tests that simulate CLI side-effects
    /// (writing files / dirs) rely on this ordering.
    #[test]
    fn fake_runner_spawn_invokes_artifact_fn_with_working_dir() {
        let artifact_log: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let log_clone = Arc::clone(&artifact_log);
        let recorder: Arc<Mutex<Vec<(Uuid, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));

        let runner = FakeRunner::new(
            Box::new(|| {
                Ok(RunnerResult {
                    exit_code: 0,
                    output: "spawned".into(),
                    conversation: None,
                    timed_out: false,
                    completion_killed: false,
                    permission_denials: Vec::new(),
                    session_id: None,
                })
            }),
            Some(Box::new(move |cwd| {
                log_clone.lock().unwrap().push(cwd.to_path_buf());
            })),
            Arc::clone(&recorder),
            Box::new(|| Ok(())),
        );

        let perm = scoped_coding();
        let result = runner.spawn(
            "test-prompt",
            &perm,
            RunnerOpts {
                working_dir: Some(Path::new("/tmp/test-working-dir")),
                ..RunnerOpts::default()
            },
        );
        assert!(
            result.is_ok(),
            "spawn must return the configured result, got {result:?}"
        );
        assert!(
            result.unwrap().output.contains("spawned"),
            "spawn must return the configured RunnerResult"
        );
        let log = artifact_log.lock().unwrap();
        assert_eq!(
            log.len(),
            1,
            "artifact_fn must be called exactly once inside spawn"
        );
        assert_eq!(
            log[0],
            PathBuf::from("/tmp/test-working-dir"),
            "artifact_fn receives opts.working_dir as cwd"
        );
    }

    /// `#[cfg(test)]` stub that implements `LlmRunner` without overriding
    /// `cleanup_session`. Exists exclusively to exercise the trait's
    /// default-method body. `spawn` is `unreachable!()` because the
    /// default-impl tests never invoke it.
    struct FakeNoOpRunner;

    impl LlmRunner for FakeNoOpRunner {
        fn spawn(
            &self,
            _prompt: &str,
            _permission_mode: &PermissionMode,
            _opts: RunnerOpts<'_>,
        ) -> TaskMgrResult<RunnerResult> {
            unreachable!("FakeNoOpRunner exists only to test the trait default cleanup_session")
        }
    }

    /// AC (TEST-INIT-001 #1): the `LlmRunner::cleanup_session` default impl
    /// returns `Ok(())` for any (session_id, cwd) pair when the implementing
    /// type does NOT override the method. Drives the FEAT-002 contract that
    /// providers without a headless artifact opt out of cleanup by simply
    /// inheriting the default — no boilerplate `fn cleanup_session(...) {
    /// Ok(()) }` overrides scattered through future impls.
    #[test]
    fn cleanup_session_default_impl_returns_ok() {
        let session = Uuid::new_v4();
        let cwd = Path::new("/tmp/does-not-matter-for-noop-default");
        let result = FakeNoOpRunner.cleanup_session(session, cwd);
        assert!(
            result.is_ok(),
            "default cleanup_session impl must be a silent no-op success, got {result:?}"
        );
    }

    /// AC (TEST-INIT-001 #2): `grok_encoded_session_dir(/home/user/repo,
    /// /home/user)` returns `<home>/.grok/sessions/%2Fhome%2Fuser%2Frepo/`.
    /// The `/` → `%2F` substitution is the documented percent-encoding
    /// contract (FEAT-002 will satisfy via `urlencoding::encode`).
    #[test]
    fn grok_encoded_session_dir_matches_observed_on_disk_path() {
        let cwd = Path::new("/home/user/repo");
        let home = Path::new("/home/user");
        let got = grok_encoded_session_dir(cwd, home);
        let expected: PathBuf = [
            home.to_string_lossy().as_ref(),
            ".grok",
            "sessions",
            "%2Fhome%2Fuser%2Frepo",
        ]
        .iter()
        .collect();
        assert_eq!(
            got, expected,
            "grok_encoded_session_dir must produce <home>/.grok/sessions/<encoded-cwd>/, \
             got {got:?}",
        );
    }

    /// AC (TEST-INIT-001 #3): a trailing-slash cwd MUST normalize to the
    /// same directory as the no-slash form. Closes the divergence that
    /// would otherwise let `/foo/` and `/foo` accumulate two distinct
    /// per-session directories that never get cleaned together.
    #[test]
    fn grok_encoded_session_dir_trailing_slash_normalizes_to_no_slash() {
        let home = Path::new("/home/user");
        let with_slash = grok_encoded_session_dir(Path::new("/home/user/repo/"), home);
        let no_slash = grok_encoded_session_dir(Path::new("/home/user/repo"), home);
        assert_eq!(
            with_slash, no_slash,
            "trailing slash must be trimmed before encoding; got {with_slash:?} vs {no_slash:?}",
        );
    }

    /// AC (TEST-INIT-001 #8): structural assertion — the encoded directory
    /// is absolute when `home` is absolute. Guards against an accidental
    /// future refactor that strips `home` (e.g. by encoding the absolute
    /// cwd alone and forgetting to prepend the sessions root).
    #[test]
    fn grok_encoded_session_dir_result_is_absolute_when_home_is_absolute() {
        let got = grok_encoded_session_dir(Path::new("/home/user/repo"), Path::new("/home/user"));
        assert!(
            PathBuf::from(&got).is_absolute(),
            "encoded session dir must be absolute when HOME is absolute, got {got:?}",
        );
    }

    /// AC (TEST-INIT-001 #4 + #7): the helper deletes ONLY the deterministic
    /// UUID-named jsonl and leaves a bystander jsonl in the same encoded_cwd
    /// directory untouched. Mirrors the bystander test at `claude.rs:3087`
    /// but drives the **runner.rs** symbol so FEAT-001's promotion is what
    /// gets exercised, not the legacy claude.rs site.
    ///
    /// Known-bad discriminator: a stub returning `Ok(())` without removing
    /// anything would pass the return-value check but fail
    /// `assert!(!target.exists())`. The presence of the existence-check is
    /// load-bearing — do not relax it.
    #[test]
    fn cleanup_claude_session_artifact_deletes_target_preserves_bystander() {
        use crate::loop_engine::claude::encoded_cwd_dir;
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let projects_dir = encoded_cwd_dir(&fake_cwd, &fake_home);
        std::fs::create_dir_all(&projects_dir).unwrap();

        let bystander_uuid = Uuid::new_v4();
        let bystander = projects_dir.join(format!("{}.jsonl", bystander_uuid));
        std::fs::write(&bystander, "untouched").unwrap();

        let target_uuid = Uuid::new_v4();
        let target = projects_dir.join(format!("{}.jsonl", target_uuid));
        std::fs::write(&target, "to-be-deleted").unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let result = cleanup_claude_session_artifact(target_uuid, Some(&fake_cwd));

        assert!(
            result.is_ok(),
            "expected Ok on successful removal, got {result:?}"
        );
        assert!(
            !target.exists(),
            "cleanup_claude_session_artifact must remove the UUID-matched target file",
        );
        assert!(
            bystander.exists(),
            "cleanup_claude_session_artifact must not touch a .jsonl with a different UUID",
        );
    }

    /// AC (TEST-INIT-001 #5): a missing target file is silent success.
    /// Claude may crash before writing the ai-title artifact (or a future
    /// upstream may stop leaking entirely); `NotFound` must collapse to
    /// `Ok(())` so dispatch doesn't paint a misleading red banner.
    #[test]
    fn cleanup_claude_session_artifact_missing_target_is_ok() {
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();
        // Deliberately do NOT create the encoded projects dir or any file.

        let _home = RunnerHomeGuard::set(&fake_home);
        let result = cleanup_claude_session_artifact(Uuid::new_v4(), Some(&fake_cwd));

        assert!(
            result.is_ok(),
            "NotFound must be silent success, got {result:?}",
        );
    }

    /// AC (TEST-INIT-001 #6): a non-`NotFound` IO error (here:
    /// `PermissionDenied` from a read-only parent dir) returns
    /// `Err(TaskMgrError::IoErrorWithContext { .. })` so dispatch can route
    /// it to the warn-once banner. The exact variant must be
    /// `IoErrorWithContext` (not bare `IoError`) so the operator sees the
    /// path + operation context.
    ///
    /// Skipped on root (euid 0) — root bypasses unix DAC permission checks
    /// and `remove_file` would succeed against a 0o555 parent.
    #[test]
    fn cleanup_claude_session_artifact_propagates_permission_denied_as_io_error() {
        use crate::loop_engine::claude::encoded_cwd_dir;
        use std::os::unix::fs::PermissionsExt as _;

        // SAFETY: getuid() is always safe; just an FFI call returning u32.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!(
                "skipping cleanup_claude_session_artifact_propagates_permission_denied_as_io_error: \
                 root bypasses DAC checks"
            );
            return;
        }

        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let projects_dir = encoded_cwd_dir(&fake_cwd, &fake_home);
        std::fs::create_dir_all(&projects_dir).unwrap();

        let target_uuid = Uuid::new_v4();
        let target = projects_dir.join(format!("{}.jsonl", target_uuid));
        std::fs::write(&target, "blocked-by-readonly-parent").unwrap();

        // Make the parent dir read+exec only — file exists but cannot be removed.
        let original = std::fs::metadata(&projects_dir).unwrap().permissions();
        std::fs::set_permissions(&projects_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let result = cleanup_claude_session_artifact(target_uuid, Some(&fake_cwd));

        // Restore perms before assertions so the tempdir can be cleaned up
        // even when assertions fail.
        std::fs::set_permissions(&projects_dir, original).unwrap();

        match result {
            Err(TaskMgrError::IoErrorWithContext { source, .. }) => {
                assert_eq!(
                    source.kind(),
                    std::io::ErrorKind::PermissionDenied,
                    "expected PermissionDenied IO kind, got {:?}",
                    source.kind(),
                );
            }
            Err(TaskMgrError::IoError(e)) => panic!(
                "expected IoErrorWithContext for cleanup failure context, got bare IoError({e:?})",
            ),
            Err(other) => panic!("expected IoErrorWithContext, got Err({other:?})"),
            Ok(()) => panic!("expected Err on PermissionDenied, got Ok(())"),
        }
    }

    // ---------------------------------------------------------------------
    // TEST-INIT-002 — TDD scaffolding for FEAT-005 + FEAT-006.
    //
    // These tests pin the concrete `LlmRunner::cleanup_session` overrides on
    // `ClaudeRunner` and `GrokRunner`. Both impls currently inherit the
    // trait's default `Ok(())` body, so EVERY test below fails its
    // post-condition (target file/dir still exists) until FEAT-006 wires
    // the real per-runner cleanup. The tests stay `#[ignore]`d on the
    // FEAT-006 gate per learning #2813 — FEAT-006 un-ignores them as part
    // of its acceptance.
    //
    // Discriminator vs default impl: each test asserts BOTH that the call
    // returned `Ok(())` AND that the deterministic target was removed (or
    // preserved, for bystander cases). A stub returning `Ok(())` without
    // touching the filesystem (i.e. today's default impl) fails the
    // existence assertion — that's the FEAT-006 driver.
    //
    // Idempotency note: a second call against a fully-cleaned tempdir must
    // collapse `NotFound` to `Ok(())`. Learning #2847 — never enumerate-
    // and-sweep; deletion targets the exact (session_id, cwd) tuple.
    // ---------------------------------------------------------------------

    /// AC (TEST-INIT-002 #1): `ClaudeRunner::cleanup_session` removes the
    /// per-session ai-title jsonl at `<HOME>/.claude/projects/<encoded-cwd>/<uuid>.jsonl`.
    /// Mirrors the bystander fixture pattern at `claude.rs:3121-3138` but
    /// targets the runner trait method so FEAT-006's override is the path
    /// under test.
    #[test]
    fn claude_runner_cleanup_session_deletes_target_when_present() {
        use crate::loop_engine::claude::encoded_cwd_dir;
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let projects_dir = encoded_cwd_dir(&fake_cwd, &fake_home);
        std::fs::create_dir_all(&projects_dir).unwrap();

        let target_uuid = Uuid::new_v4();
        let target = projects_dir.join(format!("{}.jsonl", target_uuid));
        std::fs::write(&target, "to-be-deleted").unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let result = ClaudeRunner.cleanup_session(target_uuid, &fake_cwd);

        assert!(
            result.is_ok(),
            "expected Ok on successful removal, got {result:?}",
        );
        assert!(
            !target.exists(),
            "ClaudeRunner::cleanup_session must remove <home>/.claude/projects/<encoded-cwd>/<uuid>.jsonl",
        );
    }

    /// AC (TEST-INIT-002 #2): a second `cleanup_session` call against the
    /// already-cleaned tuple must collapse `NotFound` into silent
    /// `Ok(())`. Guards against a regression where the impl unconditionally
    /// raises `IoErrorWithContext` on missing targets and paints the
    /// warn-once banner every iteration of a stable, leak-free build.
    #[test]
    fn claude_runner_cleanup_session_is_idempotent() {
        use crate::loop_engine::claude::encoded_cwd_dir;
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let projects_dir = encoded_cwd_dir(&fake_cwd, &fake_home);
        std::fs::create_dir_all(&projects_dir).unwrap();

        let target_uuid = Uuid::new_v4();
        let target = projects_dir.join(format!("{}.jsonl", target_uuid));
        std::fs::write(&target, "first-pass-removes-me").unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let first = ClaudeRunner.cleanup_session(target_uuid, &fake_cwd);
        assert!(first.is_ok(), "first cleanup expected Ok, got {first:?}");
        assert!(!target.exists(), "first cleanup must remove the target");

        let second = ClaudeRunner.cleanup_session(target_uuid, &fake_cwd);
        assert!(
            second.is_ok(),
            "second cleanup against missing target must be silent Ok (NotFound collapses), got {second:?}",
        );
    }

    /// AC (TEST-INIT-002 #3): two jsonl files share the encoded_cwd dir.
    /// `cleanup_session(target_uuid, cwd)` removes ONLY the matching uuid.jsonl
    /// and leaves the unrelated jsonl untouched. Drives the "deterministic
    /// UUID lookup, never enumerate-and-sweep" invariant (learning #2847).
    #[test]
    fn claude_runner_cleanup_session_preserves_unrelated_jsonl() {
        use crate::loop_engine::claude::encoded_cwd_dir;
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let projects_dir = encoded_cwd_dir(&fake_cwd, &fake_home);
        std::fs::create_dir_all(&projects_dir).unwrap();

        let bystander_uuid = Uuid::new_v4();
        let bystander = projects_dir.join(format!("{}.jsonl", bystander_uuid));
        std::fs::write(&bystander, "untouched").unwrap();

        let target_uuid = Uuid::new_v4();
        let target = projects_dir.join(format!("{}.jsonl", target_uuid));
        std::fs::write(&target, "to-be-deleted").unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let result = ClaudeRunner.cleanup_session(target_uuid, &fake_cwd);
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(!target.exists(), "target uuid.jsonl must be removed");
        assert!(
            bystander.exists(),
            "bystander uuid.jsonl in the same encoded_cwd dir must NOT be touched",
        );
    }

    /// AC (TEST-INIT-002 #4): `GrokRunner::cleanup_session` recursively
    /// removes the per-session DIRECTORY at
    /// `<HOME>/.grok/sessions/<percent-encoded-cwd>/<uuid>/`. Grok leaks a
    /// directory of artifacts per session (strictly worse than Claude's
    /// single file), so the impl must `remove_dir_all` the uuid subdir —
    /// not just unlink one file inside it.
    #[test]
    fn grok_runner_cleanup_session_recursively_deletes_session_dir() {
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let encoded = grok_encoded_session_dir(&fake_cwd, &fake_home);
        let target_uuid = Uuid::new_v4();
        let target_dir = encoded.join(target_uuid.to_string());
        // Populate the session dir with nested children so a non-recursive
        // remove would fail loudly. Mirrors the on-disk shape FEAT-005 captures.
        std::fs::create_dir_all(target_dir.join("nested")).unwrap();
        std::fs::write(target_dir.join("session.json"), "{}").unwrap();
        std::fs::write(target_dir.join("nested/chunk.json"), "{}").unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let result = GrokRunner.cleanup_session(target_uuid, &fake_cwd);

        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            !target_dir.exists(),
            "GrokRunner::cleanup_session must recursively remove <home>/.grok/sessions/<encoded-cwd>/<uuid>/",
        );
    }

    /// AC (TEST-INIT-002 #5): `prompt_history.jsonl` at
    /// `<HOME>/.grok/sessions/<percent-encoded-cwd>/prompt_history.jsonl`
    /// accumulates across sessions by design. The cleanup MUST leave it
    /// alone; widening the delete to the encoded_cwd dir is the failure
    /// mode this test guards.
    #[test]
    fn grok_runner_cleanup_session_preserves_prompt_history_jsonl() {
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let encoded = grok_encoded_session_dir(&fake_cwd, &fake_home);
        std::fs::create_dir_all(&encoded).unwrap();
        let prompt_history = encoded.join("prompt_history.jsonl");
        std::fs::write(&prompt_history, "{\"keep\":\"me\"}\n").unwrap();

        let target_uuid = Uuid::new_v4();
        let target_dir = encoded.join(target_uuid.to_string());
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("session.json"), "{}").unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let result = GrokRunner.cleanup_session(target_uuid, &fake_cwd);

        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(!target_dir.exists(), "target session dir must be removed");
        assert!(
            prompt_history.exists(),
            "prompt_history.jsonl at <encoded-cwd>/prompt_history.jsonl must be preserved \
             (it accumulates across sessions by design)",
        );
        // Content untouched, not just existence — the file might have been
        // truncated by a buggy impl that opened it for write.
        let body = std::fs::read_to_string(&prompt_history).unwrap();
        assert_eq!(
            body, "{\"keep\":\"me\"}\n",
            "prompt_history.jsonl content mutated"
        );
    }

    /// AC (TEST-INIT-002 #6): two session uuid subdirs share the
    /// encoded_cwd dir. `cleanup_session(target_uuid, cwd)` removes ONLY
    /// the matching one. Parallel-slot loops have multiple grok sessions
    /// in flight under the same cwd; a wrong impl that sweeps the
    /// encoded_cwd dir would nuke a peer slot's live session.
    #[test]
    fn grok_runner_cleanup_session_preserves_other_uuid_subdirs() {
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let encoded = grok_encoded_session_dir(&fake_cwd, &fake_home);

        let bystander_uuid = Uuid::new_v4();
        let bystander_dir = encoded.join(bystander_uuid.to_string());
        std::fs::create_dir_all(&bystander_dir).unwrap();
        std::fs::write(bystander_dir.join("session.json"), "peer-slot").unwrap();

        let target_uuid = Uuid::new_v4();
        let target_dir = encoded.join(target_uuid.to_string());
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("session.json"), "to-be-deleted").unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let result = GrokRunner.cleanup_session(target_uuid, &fake_cwd);

        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(!target_dir.exists(), "target uuid dir must be removed");
        assert!(
            bystander_dir.exists(),
            "peer uuid session dir under the same encoded_cwd must NOT be touched",
        );
        assert!(
            bystander_dir.join("session.json").exists(),
            "peer session.json must remain intact (no enumerate-and-sweep)",
        );
    }

    /// AC (TEST-INIT-002 #7): a second `GrokRunner::cleanup_session` call
    /// against the already-removed tuple is silent `Ok(())`. Symmetric to
    /// the Claude idempotency case; Grok's `remove_dir_all` raises
    /// `NotFound` on a missing dir and the impl must collapse it.
    #[test]
    fn grok_runner_cleanup_session_is_idempotent() {
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let encoded = grok_encoded_session_dir(&fake_cwd, &fake_home);
        let target_uuid = Uuid::new_v4();
        let target_dir = encoded.join(target_uuid.to_string());
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("session.json"), "first-pass-removes-me").unwrap();

        let _home = RunnerHomeGuard::set(&fake_home);
        let first = GrokRunner.cleanup_session(target_uuid, &fake_cwd);
        assert!(first.is_ok(), "first cleanup expected Ok, got {first:?}");
        assert!(
            !target_dir.exists(),
            "first cleanup must remove the target dir"
        );

        let second = GrokRunner.cleanup_session(target_uuid, &fake_cwd);
        assert!(
            second.is_ok(),
            "second cleanup against missing dir must be silent Ok (NotFound collapses), got {second:?}",
        );
    }

    /// AC (TEST-INIT-002 #8): both impls accept an absolute, path-resolved
    /// `&Path` for `cwd` — the shape dispatch will hand them in FEAT-006
    /// after resolving `RunnerOpts.working_dir` or falling back to
    /// `current_dir`. This is primarily a type/wiring sanity check: a
    /// resolved cwd produces a deterministic encoded dir on both providers
    /// and the cleanup call returns `Ok(())`. Empty/missing-dir state on
    /// disk is fine — idempotency already covers that path.
    #[test]
    fn runners_cleanup_session_accepts_resolved_cwd_path() {
        let _guard = RUNNER_HOME_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        // Resolved absolute path, matching the dispatch wiring contract.
        let resolved_cwd: PathBuf = fake_home.join("resolved-workspace");
        std::fs::create_dir_all(&resolved_cwd).unwrap();
        assert!(
            resolved_cwd.is_absolute(),
            "test fixture must mirror dispatch's path-resolved cwd shape",
        );

        let _home = RunnerHomeGuard::set(&fake_home);
        let session = Uuid::new_v4();

        let claude = ClaudeRunner.cleanup_session(session, &resolved_cwd);
        assert!(
            claude.is_ok(),
            "ClaudeRunner::cleanup_session must accept an absolute resolved cwd, got {claude:?}",
        );

        let grok = GrokRunner.cleanup_session(session, &resolved_cwd);
        assert!(
            grok.is_ok(),
            "GrokRunner::cleanup_session must accept an absolute resolved cwd, got {grok:?}",
        );
    }

    /// AC: a FakeRunner that supports no capabilities refuses every
    /// capability-driven RunnerOpts field (Pty, StreamJson, Effort,
    /// DisallowedTools) via enforce_capabilities.
    #[test]
    fn fake_runner_all_false_refuses_every_capability_driven_field() {
        let runner = CapabilityFakeRunner {
            supports_fn: |_| false,
        };
        let cases: &[(&str, RunnerOpts<'_>)] = &[
            (
                "use_pty",
                RunnerOpts {
                    use_pty: true,
                    ..RunnerOpts::default()
                },
            ),
            (
                "stream_json",
                RunnerOpts {
                    stream_json: true,
                    ..RunnerOpts::default()
                },
            ),
            (
                "effort",
                RunnerOpts {
                    effort: Some("high"),
                    ..RunnerOpts::default()
                },
            ),
            (
                "disallowed_tools",
                RunnerOpts {
                    disallowed_tools: Some("BashTool"),
                    ..RunnerOpts::default()
                },
            ),
        ];
        for (field, opts) in cases {
            let err = enforce_capabilities(&runner, RunnerKind::Claude, opts)
                .expect_err(&format!("expected Err for field {field}"));
            assert!(
                matches!(err, TaskMgrError::UnsupportedRunnerCapability { .. }),
                "expected UnsupportedRunnerCapability for field {field}, got {err:?}"
            );
        }
    }

    /// AC: a FakeRunner that supports only StreamJson accepts stream_json: true
    /// but refuses use_pty: true, demonstrating per-capability control.
    #[test]
    fn fake_runner_stream_json_only_accepts_stream_json_refuses_pty() {
        let runner = CapabilityFakeRunner {
            supports_fn: |cap| matches!(cap, RunnerCapability::StreamJson),
        };

        enforce_capabilities(
            &runner,
            RunnerKind::Claude,
            &RunnerOpts {
                stream_json: true,
                ..RunnerOpts::default()
            },
        )
        .expect("StreamJson-capable runner must accept stream_json: true");

        let err = enforce_capabilities(
            &runner,
            RunnerKind::Claude,
            &RunnerOpts {
                use_pty: true,
                ..RunnerOpts::default()
            },
        )
        .expect_err("StreamJson-only runner must refuse use_pty: true");
        match err {
            TaskMgrError::UnsupportedRunnerCapability {
                runner_kind,
                capability_name,
                field_name,
            } => {
                assert_eq!(runner_kind, RunnerKind::Claude);
                assert_eq!(capability_name, "Pty");
                assert_eq!(field_name, "use_pty");
            }
            other => panic!("expected UnsupportedRunnerCapability, got {other:?}"),
        }
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

    // ── FEAT-006: grok stderr capture tests ─────────────────────────────

    /// AC(FEAT-006): GROK_TELEMETRY_TRACE_UPLOAD=0 is forwarded to the grok
    /// child env (cuts BatchSpanProcessor export noise). The env var must NOT
    /// be set on the Claude child — that is enforced by the `RunnerKind::Grok`
    /// guard in `apply_common_env`, verified here by inspecting the child env
    /// via a mock shell script that echoes the variable's value.
    #[test]
    fn grok_telemetry_trace_upload_set_on_grok_child() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let out_file = std::env::temp_dir().join("task_mgr_grok_telemetry_env_check.txt");
        let script = std::env::temp_dir().join("task_mgr_grok_telemetry_env_check.sh");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(
                f,
                r#"printf 'GROK_TELEMETRY_TRACE_UPLOAD=%s\n' "$GROK_TELEMETRY_TRACE_UPLOAD" >> "{out}""#,
                out = out_file.display()
            )
            .unwrap();
            writeln!(f, r#"echo "ok""#).unwrap();
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_file(&out_file);
        unsafe { std::env::set_var("GROK_BINARY", script.to_str().unwrap()) };

        let perm = scoped_coding();
        dispatch(
            RunnerKind::Grok,
            "telemetry-probe",
            &perm,
            RunnerOpts::default(),
        )
        .expect("dispatch(Grok) returned Err");

        unsafe { std::env::remove_var("GROK_BINARY") };
        let _ = std::fs::remove_file(&script);

        let content = std::fs::read_to_string(&out_file).unwrap_or_default();
        let _ = std::fs::remove_file(&out_file);
        assert!(
            content.contains("GROK_TELEMETRY_TRACE_UPLOAD=0"),
            "grok child must receive GROK_TELEMETRY_TRACE_UPLOAD=0, got: {content:?}",
        );
    }

    /// AC(FEAT-006 CONTRACT): `spawn_grok_stderr_sniffer` still returns
    /// `(Arc<Mutex<String>>, JoinHandle)` and the buffer fills from child stderr
    /// byte-for-byte as before. The capture path replaces the console tee but
    /// must NOT alter the sniff buffer used by the auth-failure short-circuit.
    #[test]
    fn grok_stderr_sniffer_buffer_fills_unchanged_after_capture_path_change() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::TempDir::new().unwrap();

        let script = tmp.path().join("emit_stderr.sh");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"printf 'auth failure line\n' >&2"#).unwrap();
            writeln!(f, r#"printf 'second line\n' >&2"#).unwrap();
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut child = Command::new(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let (buf, handle) = spawn_grok_stderr_sniffer(&mut child, None);
        let _ = child.wait();
        let _ = handle.join();

        let content = buf.lock().unwrap().clone();
        assert!(
            content.contains("auth failure line"),
            "sniff buffer must contain line 1, got: {content:?}",
        );
        assert!(
            content.contains("second line"),
            "sniff buffer must contain line 2, got: {content:?}",
        );
    }

    /// AC(FEAT-006): two concurrent slots produce two distinct capture files
    /// (no interleave) — slot index is in the path. Verified at the path-
    /// computation level: `grok_stderr_capture_path` with different slot labels
    /// must return distinct, non-overlapping paths.
    #[test]
    fn grok_stderr_capture_path_concurrent_slots_produce_distinct_paths() {
        let db = std::path::PathBuf::from("/fake/.task-mgr");
        let run = Some("run-abc");
        let iter = Some(3u32);

        let p0 = grok_stderr_capture_path(Some(&db), Some("pfx"), run, Some("[slot 0]"), iter);
        let p1 = grok_stderr_capture_path(Some(&db), Some("pfx"), run, Some("[slot 1]"), iter);

        assert_ne!(p0, p1, "different slot labels must produce different paths");

        let n0 = p0
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let n1 = p1
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            n0.contains("slot0"),
            "slot-0 path must contain 'slot0', got {n0:?}"
        );
        assert!(
            n1.contains("slot1"),
            "slot-1 path must contain 'slot1', got {n1:?}"
        );
    }

    /// Defense-in-depth: a hostile `active_prefix` / `run_id` cannot escape
    /// `<db_dir>/logs/` via `..` or path separators. Every `&str` component
    /// is sanitized to `[A-Za-z0-9_-]` before being joined into the filename.
    #[test]
    fn grok_stderr_capture_path_sanitizes_components_against_traversal() {
        let db = std::path::PathBuf::from("/fake/.task-mgr");
        let logs_dir = db.join("logs");

        // Hostile prefix, run, and slot. Path separators, `..`, and NUL would
        // each independently break out of `<db_dir>/logs/` if passed through.
        let path = grok_stderr_capture_path(
            Some(&db),
            Some("../../etc"),
            Some("/passwd\0"),
            Some("[../slot]"),
            Some(0),
        )
        .expect("db_dir provided → Some(path)");

        assert_eq!(
            path.parent(),
            Some(logs_dir.as_path()),
            "sanitized path must stay under <db_dir>/logs/, got {path:?}",
        );
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            !name.contains("..") && !name.contains('/') && !name.contains('\0'),
            "filename must be filename-safe, got {name:?}",
        );
        assert!(
            name.ends_with("-grok-stderr.log"),
            "filename must keep the capture suffix, got {name:?}",
        );

        // Fallback path: a component that sanitizes to empty falls back to the
        // documented placeholder, not an empty string (which would corrupt the
        // dash-separated scheme).
        let p2 = grok_stderr_capture_path(Some(&db), Some("///"), Some("..."), None, None)
            .expect("db_dir provided → Some(path)");
        let n2 = p2.file_name().unwrap().to_string_lossy();
        assert!(
            n2.starts_with("no-prefix-no-run-noSlot-iter0-"),
            "empty-after-sanitize must fall back to placeholders, got {n2:?}",
        );
    }

    /// AC(FEAT-006): sniffer writes to the capture file; both the file and the
    /// sniff buffer contain the stderr lines — the write path and the sniff
    /// path are independent.
    #[test]
    fn grok_stderr_sniffer_writes_lines_to_capture_file() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::TempDir::new().unwrap();
        let capture_path = tmp.path().join("logs").join("test-grok-stderr.log");

        let script = tmp.path().join("emit.sh");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"printf 'line-alpha\n' >&2"#).unwrap();
            writeln!(f, r#"printf 'line-beta\n' >&2"#).unwrap();
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut child = Command::new(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let (buf, handle) = spawn_grok_stderr_sniffer(&mut child, Some(capture_path.clone()));
        let _ = child.wait();
        let _ = handle.join();

        let file_content = std::fs::read_to_string(&capture_path).unwrap();
        assert!(
            file_content.contains("line-alpha"),
            "capture file must contain line-alpha"
        );
        assert!(
            file_content.contains("line-beta"),
            "capture file must contain line-beta"
        );

        let sniff = buf.lock().unwrap().clone();
        assert!(
            sniff.contains("line-alpha"),
            "sniff buffer must also contain line-alpha"
        );
    }

    /// AC(FEAT-006 failure mode): when the capture file cannot be opened (e.g.
    /// an ancestor path is a regular file), the sniffer thread must not crash.
    /// Lines are dropped from the file side, but the sniff buffer still fills.
    #[test]
    fn grok_stderr_sniffer_bad_capture_path_is_graceful() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::TempDir::new().unwrap();

        // Make a regular file where a directory would need to be, so
        // create_dir_all and File::create both fail.
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, "i am a file").unwrap();
        let bad_path = blocker.join("subdir").join("stderr.log");

        let script = tmp.path().join("emit.sh");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"printf 'still-sniffed\n' >&2"#).unwrap();
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut child = Command::new(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let (buf, handle) = spawn_grok_stderr_sniffer(&mut child, Some(bad_path));
        let _ = child.wait();
        let _ = handle.join(); // must not panic

        let sniff = buf.lock().unwrap().clone();
        assert!(
            sniff.contains("still-sniffed"),
            "sniff buffer must fill even when capture file can't be opened, got: {sniff:?}",
        );
    }
}
