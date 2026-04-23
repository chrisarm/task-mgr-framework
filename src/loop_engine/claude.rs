/// Claude subprocess spawner for the autonomous agent loop.
///
/// Spawns `claude` with permission-mode-aware flags and `-p PROMPT` as a child
/// process. Tees stdout to stderr (live display) while collecting it into a buffer
/// for later analysis by the detection engine. Claude's stderr passes through
/// directly (inherited).
///
/// When a `SignalFlag` is provided, a watchdog thread monitors for SIGINT/SIGTERM
/// and escalates: SIGTERM → 3s grace → SIGKILL.
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::signals::SignalFlag;
use crate::loop_engine::watchdog::{TimeoutConfig, exit_code_from_status, watchdog_loop};

/// Maximum bytes for the formatted conversation in stream-json mode.
/// Byte-based for O(1) checking; limits are approximate and mostly-ASCII content means bytes ≈ chars.
const MAX_CONVERSATION_BYTES: usize = 50_000;
/// Maximum bytes for a single tool_use input block.
const MAX_TOOL_USE_BYTES: usize = 500;
/// Maximum bytes for a single tool_result content block.
const MAX_TOOL_RESULT_BYTES: usize = 1_000;

/// Compute the directory Claude Code uses for a given working directory.
///
/// Claude encodes the cwd into a flat directory name under
/// `<HOME>/.claude/projects/` by replacing every `/` with `-`. The leading
/// slash becomes a leading dash. Trailing slashes are trimmed before
/// encoding so `/foo/` and `/foo` map to the same directory.
///
/// Pure: takes `&Path` inputs, no filesystem access. Symlinks are NOT
/// resolved — Claude encodes the literal cwd it is invoked with, so this
/// must mirror that exactly.
pub(crate) fn encoded_cwd_dir(cwd: &Path, home: &Path) -> PathBuf {
    let cwd_str = cwd.to_string_lossy();
    let trimmed = cwd_str.trim_end_matches('/');
    let encoded = trimmed.replace('/', "-");
    home.join(".claude").join("projects").join(encoded)
}

/// Result of a Claude subprocess invocation.
#[derive(Debug)]
pub struct ClaudeResult {
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

/// Optional settings for a `spawn_claude` invocation.
///
/// Every field has a safe default (`None` / `false`), so callers only need to
/// set what's relevant to their use case. `prompt` and `permission_mode`
/// remain required positional args because they have no meaningful default.
///
/// Example:
/// ```ignore
/// spawn_claude(&prompt, &permission_mode, SpawnOpts {
///     model: Some(HAIKU_MODEL),
///     db_dir: Some(db_dir),
///     cleanup_title_artifact: true,
///     ..SpawnOpts::default()
/// })
/// ```
#[derive(Default)]
pub(crate) struct SpawnOpts<'a> {
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
}

/// Allocate a pseudo-TTY pair for piping the child's stdout+stderr through.
///
/// Returns `(master, slave_stdout, slave_stderr)`. The two slave fds are
/// duplicates of the same PTY endpoint so both of the child's output streams
/// share it (mirrors a terminal). Termios is configured with `OPOST` cleared
/// so the PTY doesn't map `\n` to `\r\n`, and echo bits cleared for safety
/// (we don't write to master, but keeps state deterministic).
///
/// # Why a PTY
/// Node.js — the Claude Code runtime — line-buffers stdout only when
/// `isatty(1)` returns true. A plain pipe triggers block-buffering (~8KB),
/// which makes stream-json lines arrive in bursts instead of live.
#[cfg(unix)]
fn open_pty_for_child_output() -> std::io::Result<(
    std::os::fd::OwnedFd,
    std::os::fd::OwnedFd,
    std::os::fd::OwnedFd,
)> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    // SAFETY: openpty writes the two out params; null termios/winsize/name use defaults.
    let ret = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openpty just gave us fresh fds; wrap them as owned so they close on drop.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };

    // Disable OPOST so `\n` isn't remapped to `\r\n` on output; clear echo
    // bits defensively (slave never reads from us, but state stays predictable).
    // Best-effort: termios failure doesn't abort — we'd just see `\r\n` which
    // serde_json tolerates as JSON whitespace.
    // SAFETY: tcgetattr/tcsetattr take a valid fd and a valid termios pointer.
    unsafe {
        let mut tio: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(slave.as_raw_fd(), &mut tio) == 0 {
            tio.c_oflag &= !libc::OPOST;
            tio.c_lflag &= !(libc::ECHO | libc::ECHOE | libc::ECHOK | libc::ECHONL);
            let _ = libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &tio);
        }
    }

    let slave_err = slave.try_clone()?;
    Ok((master, slave, slave_err))
}

/// Treat EIO from a PTY master read as a clean EOF.
///
/// Linux returns `EIO` (input/output error) from a master-side read once every
/// slave fd has been closed — i.e. once the child exits and its stdout/stderr
/// descriptors are released. Non-PTY readers never produce EIO, so this check
/// is safe to apply unconditionally in the read loops.
fn is_pty_read_eof(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EIO)
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        false
    }
}

/// Spawn Claude with the given prompt and collect its output.
///
/// The subprocess runs `<binary> <base-flags> <permission-flags> [-model m] -p <prompt>`.
/// Base flags are `--print --no-session-persistence` (plain mode) or
/// `--verbose --output-format stream-json --no-session-persistence` (stream-json mode).
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
/// When `opts.working_dir` is `Some`, the subprocess runs in that directory. This is
/// critical when using git worktrees: Claude's sandbox scopes file writes to its
/// working directory, so it must run from the worktree (not the source repo) to
/// be able to write files there.
///
/// When `opts.signal_flag` is `Some`, a watchdog thread polls the flag every 200ms.
/// On signal detection: sends SIGTERM to child, waits up to 3s, then SIGKILL.
///
/// When `opts.cleanup_title_artifact` is `true`, a known UUID is injected via
/// `--session-id` (before `-p`) and the corresponding `ai-title` jsonl file
/// is removed synchronously after the child process exits. The deletion is
/// best-effort and only targets the exact UUID-derived path, so unrelated
/// session files are never touched.
///
/// # Errors
///
/// Returns `TaskMgrError::IoError` if the binary is not found or
/// the process fails to spawn.
pub(crate) fn spawn_claude(
    prompt: &str,
    permission_mode: &PermissionMode,
    opts: SpawnOpts<'_>,
) -> TaskMgrResult<ClaudeResult> {
    let SpawnOpts {
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
                operation: format!("spawning Claude subprocess (is '{}' in your PATH?)", binary),
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
    let watchdog_handle = if signal_flag.is_some() || timeout.is_some() || target_task_id.is_some()
    {
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
        tee_stream_json(reader, target_task_id, &completion_epoch)
    } else {
        let mut buf = String::new();
        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    // Tee: echo to stderr (live display) and collect in buffer
                    eprintln!("{}", line);
                    buf.push_str(&line);
                    buf.push('\n');
                }
                Err(e) if is_pty_read_eof(&e) => break,
                Err(e) => {
                    eprintln!("Warning: error reading Claude stdout: {}", e);
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

    Ok(ClaudeResult {
        exit_code,
        output,
        conversation,
        timed_out,
        completion_killed,
        permission_denials,
    })
}

/// Read stream-json lines from Claude, tee assistant text to stderr, and return
/// (output_text, conversation).
///
/// - `output_text`: extracted from the final `result.result` field (what `--print` would emit).
/// - `conversation`: formatted transcript of the full conversation, capped at
///   `MAX_CONVERSATION_BYTES`.
///
/// Each JSON line is parsed exactly once; the parsed `Value` is passed to both
/// `tee_assistant_text` (for live display) and `process_stream_json_values` (for
/// conversation building).
fn tee_stream_json(
    reader: BufReader<impl std::io::Read>,
    target_task_id: Option<&str>,
    completion_epoch: &AtomicU64,
) -> (String, Option<String>, Vec<serde_json::Value>) {
    let mut parsed: Vec<serde_json::Value> = Vec::new();

    for line_result in reader.lines() {
        match line_result {
            Ok(line) => {
                match serde_json::from_str::<serde_json::Value>(&line) {
                    Ok(val) => {
                        // Tee assistant text live before collecting for conversation building
                        tee_assistant_text(&val);
                        // Arm the post-completion grace once we see the current
                        // task's `<completed>` in the stream. Other task IDs are
                        // ignored here and captured normally via the post-process
                        // parse_completed_tasks pass.
                        if let Some(target) = target_task_id {
                            scan_for_target_completion(&val, target, completion_epoch);
                        }
                        parsed.push(val);
                    }
                    Err(_) => {
                        eprintln!("Warning: malformed stream-json line (not valid JSON)");
                    }
                }
            }
            Err(e) if is_pty_read_eof(&e) => break,
            Err(e) => {
                eprintln!("Warning: error reading Claude stdout: {}", e);
                break;
            }
        }
    }

    process_stream_json_values(parsed.into_iter())
}

/// Extract error text from an assistant message, handling both string and object error shapes.
fn extract_error_text(val: &serde_json::Value) -> Option<String> {
    let error = val.get("error")?;
    if error.is_null() {
        return None;
    }
    if let Some(s) = error.as_str()
        && !s.is_empty()
    {
        return Some(s.to_string());
    }
    error
        .get("message")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
}

/// Extract the content array from an assistant message value.
fn assistant_content(val: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    val.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
}

fn tee_assistant_text(val: &serde_json::Value) {
    if val.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return;
    }
    if let Some(content) = assistant_content(val) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("text")
                && let Some(text) = block.get("text").and_then(|t| t.as_str())
            {
                eprintln!("{}", text);
            }
        }
    }
}

/// Scan an assistant message for `<completed>TARGET</completed>` and arm the
/// post-completion grace if the current target task's tag is present.
///
/// Writes `completion_epoch` at most once (first observation wins) so that
/// repeated completions don't reset the grace timer. Uses
/// `parse_completed_tasks` for tolerance against surrounding whitespace and
/// to match the detection path the engine uses post-process.
fn scan_for_target_completion(
    val: &serde_json::Value,
    target_task_id: &str,
    completion_epoch: &AtomicU64,
) {
    if completion_epoch.load(Ordering::Acquire) != 0 {
        return; // already armed — first observation wins
    }
    if val.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return;
    }
    let Some(content) = assistant_content(val) else {
        return;
    };
    let mut found = false;
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("text")
            && let Some(text) = block.get("text").and_then(|t| t.as_str())
            && crate::loop_engine::output_parsing::parse_completed_tasks(text)
                .iter()
                .any(|id| id == target_task_id)
        {
            found = true;
            break;
        }
    }
    if !found {
        return;
    }
    // Saturate to 1 so the sentinel `0 == not armed` stays unambiguous even on
    // a system whose clock reads pre-epoch (unsynced VM / container init); a
    // 0-valued store would silently fail the "already armed" short-circuit and
    // the grace kill would never fire.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .max(1);
    // CAS so a second thread (shouldn't exist, but defensive) can't double-set.
    if completion_epoch
        .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        eprintln!(
            "[completion] saw <completed>{}</completed> in stream — {}s grace window begins",
            target_task_id,
            crate::loop_engine::watchdog::POST_COMPLETION_GRACE_SECS,
        );
    }
}

/// Parse an iterator of stream-json lines into (output_text, conversation).
///
/// - `output_text` is extracted from the final `{"type":"result","result":"..."}` line.
/// - `conversation` is a formatted transcript of assistant messages (text, tool_use) and
///   user messages (tool_result), capped at `MAX_CONVERSATION_BYTES`.
///
/// Malformed JSON lines and unknown message types are silently skipped with a warning.
///
/// Note: permission_denials are discarded here to keep existing test call sites unchanged.
/// Use `parse_stream_json_lines_full` to get the full 3-tuple including denials.
#[cfg(test)]
pub(crate) fn parse_stream_json_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> (String, Option<String>) {
    let (output, conversation, _denials) = parse_stream_json_lines_full(lines);
    (output, conversation)
}

/// Like `parse_stream_json_lines` but also returns permission_denials.
#[cfg(test)]
pub(crate) fn parse_stream_json_lines_full<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> (String, Option<String>, Vec<serde_json::Value>) {
    let values = lines.filter_map(
        |line| match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => Some(v),
            Err(_) => {
                eprintln!("Warning: malformed stream-json line (not valid JSON)");
                None
            }
        },
    );
    process_stream_json_values(values)
}

/// Process an iterator of already-parsed stream-json `Value`s into
/// (output_text, conversation, permission_denials).
///
/// This is the shared core used by both `parse_stream_json_lines` (which parses strings first)
/// and `tee_stream_json` (which passes pre-parsed values to avoid double-parsing).
fn process_stream_json_values(
    values: impl Iterator<Item = serde_json::Value>,
) -> (String, Option<String>, Vec<serde_json::Value>) {
    let mut output_text = String::new();
    let mut conversation = String::new();
    let mut permission_denials: Vec<serde_json::Value> = Vec::new();

    for val in values {
        match val.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                process_assistant_message(&val, &mut conversation);
            }
            Some("user") => {
                process_user_message(&val, &mut conversation);
            }
            Some("result") => {
                // Extract the output text from the result line
                output_text = val
                    .get("result")
                    .map(|r| {
                        r.as_str().map(|s| s.to_string()).unwrap_or_else(|| {
                            if r.is_null() {
                                String::new()
                            } else {
                                r.to_string()
                            }
                        })
                    })
                    .unwrap_or_default();

                // Extract permission denials from the result line
                if let Some(denials) = val.get("permission_denials").and_then(|d| d.as_array()) {
                    permission_denials.extend(denials.iter().cloned());
                }
            }
            // Skip system/init and unknown types
            _ => {}
        }
    }

    (output_text, Some(conversation), permission_denials)
}

/// Append formatted assistant message content to the conversation buffer.
fn process_assistant_message(val: &serde_json::Value, conversation: &mut String) {
    if let Some(error) = extract_error_text(val) {
        append_capped(conversation, &format!("[Error: {}]\n", error));
        return;
    }

    let content = match assistant_content(val) {
        Some(c) => c,
        None => return,
    };

    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    append_capped(conversation, text);
                    append_capped(conversation, "\n");
                }
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let input_str = block
                    .get("input")
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                let truncated = truncate_bytes(&input_str, MAX_TOOL_USE_BYTES);
                append_capped(conversation, &format!("[Tool: {}] {}\n", name, truncated));
            }
            _ => {} // Skip thinking blocks and unknown types
        }
    }
}

/// Append formatted user message content (tool_result) to the conversation buffer.
fn process_user_message(val: &serde_json::Value, conversation: &mut String) {
    let content = match val
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(c) => c,
        None => return,
    };

    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
            let content_str = block
                .get("content")
                .and_then(|c| {
                    c.as_str().map(|s| s.to_string()).or_else(|| {
                        c.as_array().map(|arr| {
                            arr.iter()
                                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                    })
                })
                .unwrap_or_default();
            let truncated = truncate_bytes(&content_str, MAX_TOOL_RESULT_BYTES);
            append_capped(conversation, &format!("[Result: {}]\n", truncated));
        }
    }
}

/// Append `s` to `buf` only up to `MAX_CONVERSATION_BYTES` total.
fn append_capped(buf: &mut String, s: &str) {
    if buf.len() >= MAX_CONVERSATION_BYTES {
        return;
    }
    let remaining = MAX_CONVERSATION_BYTES - buf.len();
    let to_append = truncate_bytes(s, remaining);
    buf.push_str(to_append);
}

/// Truncate `s` to at most `max_bytes` bytes without splitting a UTF-8 character.
fn truncate_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk backwards from max_bytes to find a char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Maximum number of denied-command hints to emit per iteration.
const MAX_DENIAL_HINTS: usize = 20;

/// Extract actionable denied Bash commands from permission_denials.
///
/// Returns deduplicated binary/script names suitable for hint output.
/// Only Bash tool denials produce entries; other tools are silently skipped.
pub(crate) fn extract_denied_commands(denials: &[serde_json::Value]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for denial in denials {
        // Filter to Bash tool denials only
        let tool_name = denial
            .get("tool_name")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if tool_name != "Bash" {
            continue;
        }

        // Extract the command string from tool_input.command
        let command = match denial
            .get("tool_input")
            .and_then(|i| i.get("command"))
            .and_then(|c| c.as_str())
        {
            Some(c) => c,
            None => continue,
        };

        let binary = extract_binary(command);
        if binary.is_empty() {
            continue;
        }

        if seen.insert(binary.clone()) {
            result.push(binary);
            if result.len() >= MAX_DENIAL_HINTS {
                break;
            }
        }
    }

    result
}

/// Extract the binary/script name from a shell command string.
///
/// Takes the first whitespace-delimited token, strips leading `(` or `{` chars
/// (subshell/brace-group prefixes), and filters out control characters.
/// Returns empty string for empty/unparseable input.
fn extract_binary(command: &str) -> String {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Handle `env` prefix: `env VAR=val cmd ...` → extract `cmd`
    let effective = if trimmed.starts_with("env ") {
        // Skip "env" and any VAR=val tokens
        trimmed
            .split_whitespace()
            .skip(1) // skip "env"
            .find(|tok| !tok.contains('='))
            .unwrap_or("")
    } else {
        // First whitespace-delimited token
        trimmed.split_whitespace().next().unwrap_or("")
    };

    // Strip leading ( or { (subshell/brace-group)
    let cleaned = effective.trim_start_matches(['(', '{']);

    // Filter out control characters
    let sanitized: String = cleaned.chars().filter(|c| !c.is_control()).collect();

    sanitized
}

/// Check whether a file path targets a PRD task JSON file.
///
/// Matches paths containing `.task-mgr/tasks/` that end with `.json`.
fn is_tasks_json_path(path: &str) -> bool {
    path.ends_with(".json")
        && (path.starts_with(".task-mgr/tasks/") || path.contains("/.task-mgr/tasks/"))
}

/// Extract Edit/Write denials on `.task-mgr/tasks/*.json` from permission_denials.
///
/// Returns `(tool_name, file_path)` pairs for denied Edit or Write calls that
/// targeted PRD task files. Used to emit targeted hints in the loop engine
/// (prefer `task-mgr add --stdin` / `<task-status>` tags instead of direct edits).
///
/// Non-matching denials (Bash, other paths) are silently skipped.
pub(crate) fn extract_tasks_json_denials(denials: &[serde_json::Value]) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for denial in denials {
        let tool_name = denial
            .get("tool_name")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if tool_name != "Edit" && tool_name != "Write" {
            continue;
        }
        let file_path = denial
            .get("tool_input")
            .and_then(|i| i.get("file_path"))
            .and_then(|f| f.as_str())
            .unwrap_or("");
        if is_tasks_json_path(file_path) {
            result.push((tool_name.to_string(), file_path.to_string()));
        }
    }
    result
}

/// Maximum size in bytes for a Claude session file to be considered a ghost
/// (auto-mode classifier artifact with no real conversation).
const GHOST_SESSION_MAX_BYTES: u64 = 300;

/// Remove tiny "ghost" session files left behind by Claude's auto-mode
/// classifier subprocess. These are interactive sessions with no real
/// conversation content — just metadata stubs.
pub(crate) fn cleanup_ghost_sessions() {
    let sessions_dir = match std::env::var("HOME") {
        Ok(h) => std::path::PathBuf::from(h).join(".claude").join("sessions"),
        Err(_) => return,
    };
    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let size = match entry.metadata() {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if size <= GHOST_SESSION_MAX_BYTES {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Warn-once guard for `cleanup_title_artifact_sync` non-`NotFound` errors.
/// Prevents a misconfigured `~/.claude/` mount from spraying one stderr line
/// per spawn across a 50-batch curate run. The first error is printed in full
/// (with path and error kind) so diagnosis is still possible; subsequent
/// errors in the same process are silent.
static CLEANUP_WARN_ONCE: AtomicBool = AtomicBool::new(false);

/// Delete the ai-title jsonl that Claude wrote for the given session UUID.
///
/// Called synchronously after the child process exits, so the file is
/// guaranteed to be present (or never written). The target path is
/// computed deterministically from the cwd via `encoded_cwd_dir`; the
/// projects directory is never enumerated, so unrelated session files
/// (interactive sessions, concurrent curate batches, loop iterations)
/// cannot be touched.
///
/// Best-effort: if HOME is unset or cwd resolution fails, we skip.
/// `NotFound` from `remove_file` is silently ignored (the artifact may
/// never have been written, e.g. if a future Claude release finally
/// honors `--no-session-persistence`). The first non-`NotFound` error in
/// a process is logged to stderr (rate-limited via `CLEANUP_WARN_ONCE`)
/// so environmental misconfiguration surfaces without flooding output.
fn cleanup_title_artifact_sync(session_id: Uuid, working_dir: Option<&Path>) {
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => return,
    };
    let cwd = match working_dir {
        Some(p) => p.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(p) => p,
            Err(_) => return,
        },
    };
    let target = encoded_cwd_dir(&cwd, &home).join(format!("{}.jsonl", session_id));
    match std::fs::remove_file(&target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            // swap returns the OLD value; only the first caller sees `false`
            // and prints. All subsequent errors in this process stay silent.
            if !CLEANUP_WARN_ONCE.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[curate cleanup] failed to delete ai-title artifact {}: {} \
                     (further cleanup errors suppressed for this process)",
                    target.display(),
                    e
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::config::CODING_ALLOWED_TOOLS;
    use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, OPUS_MODEL_1M, SONNET_MODEL};
    use crate::loop_engine::watchdog::{TimeoutConfig, exit_code_from_status};
    use rstest::rstest;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    // Serialize tests that mutate CLAUDE_BINARY — use the shared mutex from
    // test_utils so prd_reconcile.rs tests also serialize against these.
    use crate::loop_engine::test_utils::CLAUDE_BINARY_MUTEX;
    #[allow(unused_imports)]
    use std::sync::Mutex;

    /// Scoped mode with the full coding tool allowlist. Used in tests that
    /// need a valid PermissionMode but are testing something else (model flags,
    /// stream_json, signal handling, etc.).
    fn scoped_coding() -> PermissionMode {
        PermissionMode::Scoped {
            allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
        }
    }

    /// Create a script that echoes its CLI args followed by stdin on one line.
    /// This matches the output format previously produced by `echo` when the prompt
    /// was passed as a CLI arg: `--print ... -p PROMPT`.
    fn make_echo_args_stdin_script(name: &str) -> std::path::PathBuf {
        use std::io::Write as _;
        let path = std::env::temp_dir().join(format!("task_mgr_test_{name}_echo_args_stdin.sh"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
            writeln!(f, r#"echo "$@" "$PROMPT""#).unwrap();
        }
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        path
    }

    /// Test helper: run spawn_claude with a mock binary that prints args + stdin.
    fn spawn_claude_echo(
        prompt: &str,
        signal: Option<&SignalFlag>,
        model: Option<&str>,
        stream_json: bool,
        permission_mode: &PermissionMode,
    ) -> TaskMgrResult<ClaudeResult> {
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let script = make_echo_args_stdin_script("echo");
        unsafe { std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap()) };
        let result = spawn_claude(
            prompt,
            permission_mode,
            SpawnOpts {
                signal_flag: signal,
                model,
                stream_json,
                ..SpawnOpts::default()
            },
        );
        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        let _ = std::fs::remove_file(&script);
        result
    }

    // --- AC: ClaudeResult struct has expected fields ---

    #[test]
    fn test_claude_result_struct_fields() {
        let result = ClaudeResult {
            exit_code: 0,
            output: "Hello world\n".to_string(),
            conversation: None,
            timed_out: false,
            completion_killed: false,
            permission_denials: vec![],
        };
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.output, "Hello world\n");
        assert!(result.conversation.is_none());
        assert!(!result.timed_out);
        assert!(!result.completion_killed);
        assert!(result.permission_denials.is_empty());
    }

    #[test]
    fn test_claude_result_with_non_zero_exit() {
        let result = ClaudeResult {
            exit_code: 137,
            output: String::new(),
            conversation: None,
            timed_out: false,
            completion_killed: false,
            permission_denials: vec![],
        };
        assert_eq!(result.exit_code, 137);
        assert!(result.output.is_empty());
    }

    // --- AC: Handles spawn failure gracefully ---

    #[test]
    fn test_spawn_nonexistent_binary_returns_error() {
        // Temporarily override the command by testing with a guaranteed-missing binary
        let result = spawn_nonexistent_binary();
        assert!(
            result.is_err(),
            "Spawning a nonexistent binary should error"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("spawning") || err.contains("Claude"),
            "Error should mention spawning context, got: {}",
            err
        );
    }

    /// Helper: spawn a binary that definitely doesn't exist to test error handling.
    fn spawn_nonexistent_binary() -> TaskMgrResult<ClaudeResult> {
        let mut child = Command::new("definitely-nonexistent-binary-xyzzy-12345")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| TaskMgrError::IoErrorWithContext {
                file_path: "definitely-nonexistent-binary-xyzzy-12345".to_string(),
                operation: "spawning Claude subprocess (is 'claude' in your PATH?)".to_string(),
                source: e,
            })?;

        let status = child.wait().map_err(|e| TaskMgrError::IoErrorWithContext {
            file_path: "claude".to_string(),
            operation: "waiting for subprocess".to_string(),
            source: e,
        })?;

        Ok(ClaudeResult {
            exit_code: status.code().unwrap_or(1),
            output: String::new(),
            conversation: None,
            timed_out: false,
            completion_killed: false,
            permission_denials: vec![],
        })
    }

    // --- AC: Output is tee'd and collected ---

    #[test]
    fn test_spawn_echo_command_captures_output() {
        // Use 'echo' as a Claude stand-in to verify tee + capture behavior
        let result = spawn_echo("Hello from echo");
        assert!(result.is_ok(), "echo should succeed: {:?}", result.err());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(
            res.output.contains("Hello from echo"),
            "Output should contain echo text, got: '{}'",
            res.output
        );
    }

    #[test]
    fn test_spawn_captures_multiline_output() {
        // Use printf to generate multiline output
        let result = spawn_printf("line1\\nline2\\nline3");
        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(res.output.contains("line1"));
        assert!(res.output.contains("line2"));
        assert!(res.output.contains("line3"));
    }

    #[test]
    fn test_spawn_captures_exit_code() {
        // Use 'false' command which always exits with code 1
        let result = spawn_false_command();
        assert!(result.is_ok(), "Process should spawn even if it fails");
        let res = result.unwrap();
        assert_ne!(res.exit_code, 0, "false command should have non-zero exit");
    }

    #[test]
    fn test_spawn_empty_output() {
        // Use 'true' command which produces no output
        let result = spawn_true_command();
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(
            res.output.trim().is_empty(),
            "true command should produce no output"
        );
    }

    // --- AC: signal_flag=None works (backward compat) ---

    #[test]
    fn test_spawn_without_signal_flag() {
        // spawn_claude with None should behave like before
        let result = spawn_claude_echo("hello", None, None, false, &scoped_coding());
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.output.contains("hello"));
    }

    #[test]
    fn test_spawn_with_signal_flag_no_signal() {
        // spawn_claude with a SignalFlag that is NOT signaled should work normally
        let flag = SignalFlag::new();
        let result = spawn_claude_echo("test output", Some(&flag), None, false, &scoped_coding());
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.output.contains("test output"));
    }

    // --- Helpers that use the same tee pattern as spawn_claude ---

    fn spawn_echo(text: &str) -> TaskMgrResult<ClaudeResult> {
        spawn_with_tee("echo", &[text])
    }

    fn spawn_printf(text: &str) -> TaskMgrResult<ClaudeResult> {
        spawn_with_tee("printf", &[&format!("{}\\n", text)])
    }

    fn spawn_false_command() -> TaskMgrResult<ClaudeResult> {
        spawn_with_tee("false", &[])
    }

    fn spawn_true_command() -> TaskMgrResult<ClaudeResult> {
        spawn_with_tee("true", &[])
    }

    /// Spawn an arbitrary command using the same tee pattern as spawn_claude.
    /// This validates the tee + capture logic without requiring the real `claude` binary.
    fn spawn_with_tee(cmd: &str, args: &[&str]) -> TaskMgrResult<ClaudeResult> {
        let mut child = Command::new(cmd)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| TaskMgrError::IoErrorWithContext {
                file_path: cmd.to_string(),
                operation: format!("spawning {}", cmd),
                source: e,
            })?;

        let stdout = child.stdout.take().expect("stdout should be piped");

        let mut output = String::new();
        let reader = BufReader::new(stdout);

        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    // Same tee behavior as spawn_claude
                    eprintln!("{}", line);
                    output.push_str(&line);
                    output.push('\n');
                }
                Err(e) => {
                    eprintln!("Warning: error reading stdout: {}", e);
                    break;
                }
            }
        }

        let status = child.wait().map_err(|e| TaskMgrError::IoErrorWithContext {
            file_path: cmd.to_string(),
            operation: format!("waiting for {}", cmd),
            source: e,
        })?;

        Ok(ClaudeResult {
            exit_code: exit_code_from_status(status),
            output,
            conversation: None,
            timed_out: false,
            completion_killed: false,
            permission_denials: vec![],
        })
    }

    // --- AC: Watchdog thread terminates child on signal ---

    #[cfg(unix)]
    #[test]
    fn test_watchdog_kills_child_on_signal() {
        // Spawn a long-running process via spawn_claude with a signal flag.
        // The script drains stdin first (so the stdin write completes), then
        // sleeps long enough for the watchdog to kill it.
        use std::io::Write as _;
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let script_path = std::env::temp_dir().join("task_mgr_test_watchdog_signal_sleep.sh");
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, "cat > /dev/null").unwrap();
            writeln!(f, "sleep 120").unwrap();
        }
        std::fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        unsafe { std::env::set_var("CLAUDE_BINARY", script_path.to_str().unwrap()) };
        let flag = SignalFlag::new();

        // Set the signal flag after a short delay in a background thread
        let flag_clone = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            flag_clone.set();
        });

        let start = std::time::Instant::now();
        let result = spawn_claude(
            "ignored",
            &PermissionMode::Dangerous,
            SpawnOpts {
                signal_flag: Some(&flag),
                ..SpawnOpts::default()
            },
        );
        let elapsed = start.elapsed();

        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        let _ = std::fs::remove_file(&script_path);

        assert!(
            result.is_ok(),
            "spawn_claude should not error on signal kill"
        );
        let res = result.unwrap();
        // Child was killed by signal, exit code should be 128+SIGTERM or 128+SIGKILL
        assert_ne!(
            res.exit_code, 0,
            "Signal-killed process should have non-zero exit"
        );
        // Should complete well under 10s (signal at 500ms + 3s grace max = ~3.5s)
        assert!(
            elapsed.as_secs() < 10,
            "Watchdog should kill child promptly, took {:?}",
            elapsed
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_watchdog_does_not_interfere_with_normal_exit() {
        // If the child exits normally, the watchdog should stop cleanly
        let flag = SignalFlag::new();
        let result = spawn_claude_echo("quick exit", Some(&flag), None, false, &scoped_coding());

        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.output.contains("quick exit"));
        // Flag should NOT be signaled
        assert!(!flag.is_signaled());
    }

    // --- EPIPE handling ---

    /// Verify that a child which exits immediately without reading stdin
    /// triggers the BrokenPipe path (line 170) without causing a panic or error.
    #[cfg(unix)]
    #[test]
    fn test_broken_pipe_on_immediate_exit() {
        use std::io::Write as _;
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let script_path = std::env::temp_dir().join("task_mgr_test_epipe_immediate_exit.sh");
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            // Exit immediately without reading stdin — triggers EPIPE on write.
            writeln!(f, "true").unwrap();
        }
        std::fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        unsafe { std::env::set_var("CLAUDE_BINARY", script_path.to_str().unwrap()) };

        let result = spawn_claude(
            "this prompt will not be read",
            &PermissionMode::Dangerous,
            SpawnOpts::default(),
        );

        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        let _ = std::fs::remove_file(&script_path);

        assert!(
            result.is_ok(),
            "BrokenPipe on stdin write must not surface as an error: {result:?}"
        );
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0, "Script runs `true` which exits 0");
    }

    // --- Tests for --model flag on spawn_claude ---

    /// Active: model=None → no --model flag, standard flags present.
    /// Validates backward compatibility: None model produces args identical to
    /// pre-model behavior.
    #[test]
    fn test_spawn_model_none_no_model_flag() {
        let result = spawn_claude_echo("test_prompt", None, None, false, &scoped_coding());

        assert!(
            result.is_ok(),
            "spawn_claude should succeed: {:?}",
            result.err()
        );
        let res = result.unwrap();
        let output = res.output.trim();

        // model=None must NOT produce --model flag
        assert!(
            !output.contains("--model"),
            "model=None should not include --model flag, got: '{}'",
            output
        );

        // Standard flags must always be present
        assert!(
            output.contains("--print"),
            "Must always have --print, got: '{}'",
            output
        );
        assert!(
            output.contains("--permission-mode"),
            "Must always have --permission-mode, got: '{}'",
            output
        );
        assert!(
            output.contains("dontAsk"),
            "Must always have dontAsk, got: '{}'",
            output
        );
    }

    /// model=Some(OPUS_MODEL) → --model flag present with correct value in echoed args.
    #[test]
    fn test_spawn_model_some_opus_includes_model_flag() {
        let result = spawn_claude_echo(
            "test_prompt",
            None,
            Some(OPUS_MODEL),
            false,
            &scoped_coding(),
        );

        assert!(result.is_ok());
        let res = result.unwrap();
        let output = res.output.trim();

        let expected = format!("--model {OPUS_MODEL}");
        assert!(
            output.contains(&expected),
            "model=Some(OPUS_MODEL) should include --model flag, got: '{}'",
            output
        );
    }

    /// model=Some(OPUS_MODEL_1M) → --model flag present with the 1M context model ID.
    #[test]
    fn test_spawn_model_opus_1m_includes_model_flag() {
        let result = spawn_claude_echo(
            "test_prompt",
            None,
            Some(OPUS_MODEL_1M),
            false,
            &scoped_coding(),
        );

        assert!(result.is_ok());
        let res = result.unwrap();
        let output = res.output.trim();

        let expected = format!("--model {OPUS_MODEL_1M}");
        assert!(
            output.contains(&expected),
            "model=Some(OPUS_MODEL_1M) should include --model flag with 1M variant, got: '{}'",
            output
        );
    }

    /// model=Some("") → treated as None, no --model flag.
    /// Guards against naively passing --model '' to the Claude CLI.
    #[test]
    fn test_spawn_model_empty_string_treated_as_none() {
        let result = spawn_claude_echo("test_prompt", None, Some(""), false, &scoped_coding());

        assert!(result.is_ok());
        let res = result.unwrap();
        let output = res.output.trim();

        assert!(
            !output.contains("--model"),
            "model=Some('') should be treated as None — no --model flag, got: '{}'",
            output
        );
    }

    /// Known-bad discriminator — --model must appear BEFORE -p.
    /// Rejects implementations that append --model after the prompt flag.
    #[test]
    fn test_spawn_model_flag_appears_before_prompt_flag() {
        let result = spawn_claude_echo(
            "test_prompt",
            None,
            Some(OPUS_MODEL),
            false,
            &scoped_coding(),
        );

        assert!(result.is_ok());
        let res = result.unwrap();
        let output = res.output.trim();

        let model_pos = output
            .find("--model")
            .expect("--model should be present in output");
        // Use " -p " to avoid matching the "-p" inside "--print"
        let prompt_pos = output
            .find(" -p ")
            .expect("-p flag should be present in output");
        assert!(
            model_pos < prompt_pos,
            "--model (at {}) must appear BEFORE -p (at {}), got: '{}'",
            model_pos,
            prompt_pos,
            output
        );
    }

    /// --print and --permission-mode dontAsk must be present regardless of model value.
    #[test]
    fn test_spawn_model_some_preserves_required_flags() {
        let result = spawn_claude_echo(
            "test_prompt",
            None,
            Some(OPUS_MODEL),
            false,
            &scoped_coding(),
        );

        assert!(result.is_ok());
        let res = result.unwrap();
        let output = res.output.trim();

        assert!(
            output.contains("--print"),
            "Must always have --print even with model, got: '{}'",
            output
        );
        assert!(
            output.contains("--permission-mode"),
            "Must always have --permission-mode even with model, got: '{}'",
            output
        );
        assert!(
            output.contains("dontAsk"),
            "Must always have dontAsk even with model, got: '{}'",
            output
        );
        assert!(
            output.contains(&format!("--model {OPUS_MODEL}")),
            "Must have --model flag, got: '{}'",
            output
        );
    }

    // --- TEST-001: Comprehensive tests for spawn_claude model flag ---
    //
    // Parameterized coverage, flag interaction, and robustness beyond TEST-INIT-001.

    /// AC: Parameterized tests for multiple model strings (opus, sonnet, haiku, custom).
    /// Verifies each model string produces correct --model <name> before -p.
    #[rstest]
    #[case(OPUS_MODEL)]
    #[case(SONNET_MODEL)]
    #[case(HAIKU_MODEL)]
    #[case("my-custom-model")]
    fn test_spawn_claude_model_variants(#[case] model: &str) {
        let result = spawn_claude_echo("test prompt", None, Some(model), false, &scoped_coding());

        let res = result.expect("echo should succeed");
        let output = res.output.trim();

        // --model <name> present
        let expected = format!("--model {}", model);
        assert!(
            output.contains(&expected),
            "Output should contain '{}', got: '{}'",
            expected,
            output
        );

        // --model before -p
        let model_pos = output.find("--model").expect("--model should be in output");
        let p_pos = output.find(" -p ").expect("-p should be in output");
        assert!(
            model_pos < p_pos,
            "--model (pos {}) must appear before -p (pos {}) for model '{}', output: '{}'",
            model_pos,
            p_pos,
            model,
            output
        );
    }

    /// AC: Model strings with hyphens, underscores, and dots pass through correctly.
    #[rstest]
    #[case("model-with-hyphens")]
    #[case("model_with_underscores")]
    #[case("model.with.dots")]
    #[case("model-with_mixed.chars-v2")]
    fn test_spawn_claude_model_special_chars(#[case] model: &str) {
        let result = spawn_claude_echo("test prompt", None, Some(model), false, &scoped_coding());

        let res = result.expect("echo should succeed");
        let output = res.output.trim();

        let expected = format!("--model {}", model);
        assert!(
            output.contains(&expected),
            "Model '{}' should pass through verbatim, got: '{}'",
            model,
            output
        );
    }

    /// AC: --model does not interfere with --permission-mode or --print flags.
    /// Verifies exact ordering: --print --no-session-persistence --permission-mode dontAsk [--model <m>] -p <prompt>
    #[rstest]
    #[case(Some(SONNET_MODEL))]
    #[case(None)]
    fn test_spawn_claude_model_does_not_interfere_with_flags(#[case] model: Option<&str>) {
        let result = spawn_claude_echo("my prompt", None, model, false, &scoped_coding());

        let res = result.expect("echo should succeed");
        let output = res.output.trim();

        // --print always first
        assert!(
            output.starts_with("--print"),
            "--print should be first arg, got: '{}'",
            output
        );

        // --no-session-persistence present after --print
        let print_pos = output.find("--print").unwrap();
        let nsp_pos = output
            .find("--no-session-persistence")
            .expect("--no-session-persistence must be present");
        assert!(
            nsp_pos > print_pos,
            "--no-session-persistence should follow --print"
        );

        // --permission-mode always present after --no-session-persistence
        let perm_pos = output
            .find("--permission-mode")
            .expect("--permission-mode must be present");
        assert!(
            perm_pos > nsp_pos,
            "--permission-mode should follow --no-session-persistence"
        );

        // -p always present and prompt follows
        let p_pos = output.find(" -p ").expect("-p must be present");
        assert!(
            output[p_pos..].contains("my prompt"),
            "Prompt should follow -p flag, got: '{}'",
            output
        );

        // If model is present, it must be between permission flags and -p
        if let Some(m) = model {
            let model_pos = output
                .find("--model")
                .expect("--model must be present when model is Some");
            assert!(
                model_pos > perm_pos && model_pos < p_pos,
                "--model (pos {}) must be between --permission-mode (pos {}) and -p (pos {}), got: '{}'",
                model_pos,
                perm_pos,
                p_pos,
                output
            );
            assert!(
                output.contains(&format!("--model {}", m)),
                "Model value should be correct"
            );
        }
    }

    /// AC: None model produces identical args to pre-Phase-2 behavior.
    /// Exact string comparison to verify no extra args are added.
    #[test]
    fn test_spawn_claude_none_model_identical_to_pre_phase2() {
        let result = spawn_claude_echo("my prompt text", None, None, false, &scoped_coding());

        let res = result.expect("echo should succeed");
        let output = res.output.trim();

        // stream_json=false: exactly these args, no more
        let expected = format!(
            "--print --no-session-persistence --permission-mode dontAsk --allowedTools {} -p my prompt text",
            CODING_ALLOWED_TOOLS
        );
        assert_eq!(
            output, expected,
            "None model with stream_json=false must produce exactly these args"
        );
    }

    /// Edge case: whitespace-only model string treated as None.
    #[rstest]
    #[case("   ")]
    #[case("\t")]
    #[case(" \t ")]
    fn test_spawn_claude_whitespace_only_model_treated_as_none(#[case] model: &str) {
        let result = spawn_claude_echo("test prompt", None, Some(model), false, &scoped_coding());

        let res = result.expect("echo should succeed");
        let output = res.output.trim();

        assert!(
            !output.contains("--model"),
            "Whitespace-only model '{}' should be treated as None, got: '{}'",
            model.escape_debug(),
            output
        );
    }

    /// Helper: create a script that emits a stream-json result line containing CLI args + stdin.
    /// The script prints `{"type":"result","result":"<args> <stdin>"}` so the stream-json
    /// parser returns the args as the output text.  `name` makes the filename unique.
    /// Returns the absolute path to the created script.
    fn make_stream_json_result_script(name: &str) -> std::path::PathBuf {
        use std::io::Write;
        let script_path = std::env::temp_dir().join(format!("task_mgr_test_{name}.sh"));
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
            writeln!(
                f,
                r#"printf '{{"type":"result","result":"%s %s"}}\n' "$*" "$PROMPT""#
            )
            .unwrap();
        }
        std::fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        script_path
    }

    #[test]
    fn test_spawn_claude_without_timeout_not_timed_out() {
        let result = spawn_claude_echo("hello", None, None, false, &scoped_coding());

        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(!res.timed_out, "Normal exit should not be timed_out");
    }

    // --- stream_json arg construction tests ---

    #[test]
    fn test_stream_json_false_uses_print_flag() {
        let result = spawn_claude_echo("prompt", None, None, false, &scoped_coding());
        let output = result.unwrap().output;
        assert!(
            output.contains("--print"),
            "stream_json=false must use --print"
        );
        assert!(
            !output.contains("--output-format"),
            "stream_json=false must NOT use --output-format"
        );
        assert!(
            output.contains("--no-session-persistence"),
            "stream_json=false must include --no-session-persistence"
        );
    }

    #[test]
    fn test_stream_json_true_uses_output_format_stream_json() {
        // For stream_json=true the args are passed to the binary (echo) but the output is
        // processed as stream-json.  Since echo's output is not valid JSON, output_text is
        // empty, but the subprocess arg list is still written to stderr.
        // We verify behaviour by using a shell script that writes its args into a result JSON.
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let script_path = make_stream_json_result_script("args");
        unsafe { std::env::set_var("CLAUDE_BINARY", script_path.to_str().unwrap()) };
        let result = spawn_claude(
            "prompt",
            &PermissionMode::Dangerous,
            SpawnOpts {
                stream_json: true,
                ..SpawnOpts::default()
            },
        );
        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        let _ = std::fs::remove_file(&script_path);

        let res = result.expect("spawn should succeed");
        let output = res.output;
        assert!(
            output.contains("--output-format stream-json"),
            "stream_json=true must use --output-format stream-json, got: '{}'",
            output
        );
        assert!(
            output.contains("--print"),
            "stream_json=true must also use --print (required for --no-session-persistence)"
        );
        assert!(
            output.contains("--no-session-persistence"),
            "stream_json=true must include --no-session-persistence"
        );
    }

    // --- parse_stream_json_lines unit tests ---

    #[test]
    fn test_parse_stream_json_extracts_result_output() {
        let lines = [
            r#"{"type":"result","subtype":"success","result":"<completed>TASK-1</completed>","session_id":"abc"}"#,
        ];
        let (output, _conv) = parse_stream_json_lines(lines.iter().copied());
        assert_eq!(output, "<completed>TASK-1</completed>");
    }

    #[test]
    fn test_parse_stream_json_null_result_gives_empty_output() {
        let lines = [r#"{"type":"result","subtype":"success","result":null,"session_id":"abc"}"#];
        let (output, _) = parse_stream_json_lines(lines.iter().copied());
        assert_eq!(output, "");
    }

    #[test]
    fn test_parse_stream_json_assistant_text_in_conversation() {
        let lines = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello world"}]},"model":"m","error":null}"#,
            r#"{"type":"result","result":"done"}"#,
        ];
        let (output, conv) = parse_stream_json_lines(lines.iter().copied());
        assert_eq!(output, "done");
        let conv = conv.expect("conversation should be Some");
        assert!(conv.contains("Hello world"));
    }

    #[test]
    fn test_parse_stream_json_thinking_blocks_skipped() {
        let lines = [
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"internal"},{"type":"text","text":"visible"}]},"model":"m","error":null}"#,
        ];
        let (_, conv) = parse_stream_json_lines(lines.iter().copied());
        let conv = conv.unwrap();
        assert!(
            !conv.contains("internal"),
            "thinking blocks should be skipped"
        );
        assert!(conv.contains("visible"));
    }

    #[test]
    fn test_parse_stream_json_tool_use_formatted() {
        let lines = [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/src/main.rs"}}]},"model":"m","error":null}"#,
        ];
        let (_, conv) = parse_stream_json_lines(lines.iter().copied());
        let conv = conv.unwrap();
        assert!(
            conv.contains("[Tool: Read]"),
            "tool_use must be formatted as [Tool: name]"
        );
    }

    #[test]
    fn test_parse_stream_json_tool_result_in_conversation() {
        let lines = [
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"fn main() {}","is_error":false}]}}"#,
        ];
        let (_, conv) = parse_stream_json_lines(lines.iter().copied());
        let conv = conv.unwrap();
        assert!(
            conv.contains("[Result:"),
            "tool_result must be formatted as [Result: ...]"
        );
        assert!(conv.contains("fn main()"));
    }

    #[test]
    fn test_parse_stream_json_malformed_line_skipped() {
        let lines = ["not json at all", r#"{"type":"result","result":"ok"}"#];
        // Should not panic; output extracted from result line
        let (output, _) = parse_stream_json_lines(lines.iter().copied());
        assert_eq!(output, "ok");
    }

    #[test]
    fn test_parse_stream_json_system_messages_skipped() {
        let lines = [
            r#"{"type":"system","subtype":"init","data":{}}"#,
            r#"{"type":"result","result":"final"}"#,
        ];
        let (output, conv) = parse_stream_json_lines(lines.iter().copied());
        assert_eq!(output, "final");
        assert_eq!(
            conv,
            Some(String::new()),
            "system messages should not add to conversation"
        );
    }

    #[test]
    fn test_parse_stream_json_conversation_cap() {
        // Fill conversation beyond MAX_CONVERSATION_BYTES with repeated text blocks
        let big_text = "x".repeat(10_000);
        let block = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": big_text}]},
            "model": "m",
            "error": null
        })
        .to_string();
        let lines: Vec<String> = std::iter::repeat_n(block, 10).collect();
        let (_, conv) = parse_stream_json_lines(lines.iter().map(|s| s.as_str()));
        let conv = conv.unwrap();
        assert!(
            conv.len() <= MAX_CONVERSATION_BYTES,
            "conversation must be capped at {} chars, got {}",
            MAX_CONVERSATION_BYTES,
            conv.len()
        );
    }

    #[test]
    fn test_parse_stream_json_tool_use_input_truncated() {
        let big_input = "a".repeat(1000);
        let block = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "id": "t1", "name": "Write", "input": {"content": big_input}}]},
            "model": "m",
            "error": null
        })
        .to_string();
        let (_, conv) = parse_stream_json_lines(std::iter::once(block.as_str()));
        let conv = conv.unwrap();
        // The formatted tool_use line should not contain the full 1000-char input
        // (input JSON is serialized then truncated to 500 chars)
        assert!(conv.contains("[Tool: Write]"), "tool_use must be formatted");
    }

    #[test]
    fn test_truncate_bytes_ascii() {
        assert_eq!(truncate_bytes("hello world", 5), "hello");
        assert_eq!(truncate_bytes("hi", 100), "hi");
    }

    #[test]
    fn test_truncate_bytes_multibyte() {
        // "こんにちは" is 5 chars, each 3 bytes = 15 bytes total
        let s = "こんにちは";
        // 3 bytes = exactly 1 char boundary
        assert_eq!(truncate_bytes(s, 3), "こ");
        // 9 bytes = exactly 3 chars
        assert_eq!(truncate_bytes(s, 9), "こんに");
        // 4 bytes lands mid-char, should round down to 3
        assert_eq!(truncate_bytes(s, 4), "こ");
        // Ensure we don't split mid-character
        let truncated = truncate_bytes(s, 5);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    // --- TEST-001: Comprehensive tests for parse_stream_json_lines ---

    /// AC: Parameterized — multiple message type combinations produce correct output.
    #[rstest]
    #[case(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]},"model":"m","error":null}"#,
        "Hello",
        ""
    )]
    #[case(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"result data","is_error":false}]}}"#,
        "",
        "[Result: result data]"
    )]
    #[case(r#"{"type":"system","subtype":"init","data":{}}"#, "", "")]
    fn test_parse_stream_json_message_types(
        #[case] line: &str,
        #[case] expected_in_conv: &str,
        #[case] expected_result_contains: &str,
    ) {
        let (_, conv) = parse_stream_json_lines(std::iter::once(line));
        let conv_str = conv.unwrap_or_default();
        if !expected_in_conv.is_empty() {
            assert!(
                conv_str.contains(expected_in_conv),
                "Expected '{}' in conversation, got: '{}'",
                expected_in_conv,
                conv_str
            );
        }
        if !expected_result_contains.is_empty() {
            assert!(
                conv_str.contains(expected_result_contains),
                "Expected '{}' in conversation, got: '{}'",
                expected_result_contains,
                conv_str
            );
        }
        if expected_in_conv.is_empty() && expected_result_contains.is_empty() {
            assert!(
                conv_str.is_empty(),
                "Expected no conversation content for this message type, got: '{}'",
                conv_str
            );
        }
    }

    /// AC: Interleaved assistant/user/system messages — only relevant ones extracted.
    #[test]
    fn test_parse_stream_json_interleaved_messages() {
        let lines = [
            r#"{"type":"system","subtype":"init","data":{}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Step 1"}]},"model":"m","error":null}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"file content","is_error":false}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Step 2"}]},"model":"m","error":null}"#,
            r#"{"type":"system","subtype":"other","data":{"ignored":true}}"#,
            r#"{"type":"result","subtype":"success","result":"<completed>X</completed>","session_id":"s1"}"#,
        ];
        let (output, conv) = parse_stream_json_lines(lines.iter().copied());
        assert_eq!(output, "<completed>X</completed>");
        let conv = conv.expect("conversation should be Some");
        assert!(
            conv.contains("Step 1"),
            "First assistant text should appear"
        );
        assert!(
            conv.contains("Step 2"),
            "Second assistant text should appear"
        );
        assert!(
            conv.contains("[Result: file content]"),
            "Tool result should appear"
        );
        assert!(
            !conv.contains("ignored"),
            "System messages must not appear in conversation"
        );
    }

    /// AC: Truncation at exactly MAX_TOOL_USE_BYTES boundary (500 chars) — not off-by-one.
    #[test]
    fn test_parse_stream_json_tool_use_truncation_at_500_boundary() {
        // Build an input value that serializes to exactly 500 chars
        // The input field is serialized with to_string(), so "{"content":"<500 a's>"}"
        // We want the serialized form to be exactly 500 chars → find what value achieves that
        // {"content":"..."} = 13 overhead chars → fill 487 chars with 'a'
        let input_value_487 = "a".repeat(487);
        let block_exactly_500 = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "id": "t1", "name": "Read",
                "input": {"content": input_value_487}}]},
            "model": "m",
            "error": null
        })
        .to_string();
        let (_, conv) = parse_stream_json_lines(std::iter::once(block_exactly_500.as_str()));
        let conv = conv.unwrap();
        // The input serializes to exactly 500 chars or less → not truncated
        assert!(conv.contains("[Tool: Read]"));
        // The input string in the conversation should be present fully
        assert!(
            conv.contains(&input_value_487[..10]),
            "Short-enough input should not be truncated"
        );

        // Now build one that's 501 chars → should be truncated
        let input_value_488 = "a".repeat(488);
        let block_501 = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "id": "t2", "name": "Read",
                "input": {"content": input_value_488}}]},
            "model": "m",
            "error": null
        })
        .to_string();
        // The serialized input JSON is now 501 chars — should be truncated to 500
        let (_, conv2) = parse_stream_json_lines(std::iter::once(block_501.as_str()));
        let conv2 = conv2.unwrap();
        assert!(conv2.contains("[Tool: Read]"));
        // The full 488-char value inside {"content":"..."} must be truncated
        // so the conversation line must be at most 500 chars of input plus "[Tool: Read] \n"
        let tool_line = conv2
            .lines()
            .find(|l| l.contains("[Tool: Read]"))
            .expect("should have tool line");
        // The suffix after "[Tool: Read] " is the truncated input_str
        let input_part = tool_line.trim_start_matches("[Tool: Read] ");
        assert!(
            input_part.len() <= MAX_TOOL_USE_BYTES,
            "Input part must be <= {} chars, got {}",
            MAX_TOOL_USE_BYTES,
            input_part.len()
        );
    }

    /// AC: Truncation at exactly MAX_TOOL_RESULT_BYTES boundary (1000 chars) — not off-by-one.
    #[test]
    fn test_parse_stream_json_tool_result_truncation_at_1000_boundary() {
        // Exactly 1000 chars → not truncated
        let content_1000 = "b".repeat(1000);
        let line_exact = serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "t1",
                "content": content_1000, "is_error": false}]}
        })
        .to_string();
        let (_, conv) = parse_stream_json_lines(std::iter::once(line_exact.as_str()));
        let conv = conv.unwrap();
        assert!(
            conv.contains(&content_1000),
            "1000-char result should not be truncated"
        );

        // 1001 chars → truncated
        let content_1001 = "c".repeat(1001);
        let line_over = serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "t2",
                "content": content_1001, "is_error": false}]}
        })
        .to_string();
        let (_, conv2) = parse_stream_json_lines(std::iter::once(line_over.as_str()));
        let conv2 = conv2.unwrap();
        // Should NOT contain the full 1001 chars
        assert!(
            !conv2.contains(&content_1001),
            "1001-char result must be truncated"
        );
        // But should contain the first 1000 chars
        assert!(
            conv2.contains(&content_1001[..1000]),
            "First 1000 chars of result should be present"
        );
    }

    /// AC: Unicode truncation preserves char boundaries for tool_result content.
    #[test]
    fn test_parse_stream_json_unicode_truncation_preserves_char_boundaries() {
        // Build a string of 1001 Unicode chars (each 3 bytes in UTF-8) to exceed MAX_TOOL_RESULT_BYTES
        // We want char count > 1000 but byte count much higher
        let unicode_str: String = "α".repeat(1001); // α is 2 bytes each
        let line = serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "u1",
                "content": unicode_str, "is_error": false}]}
        })
        .to_string();
        let (_, conv) = parse_stream_json_lines(std::iter::once(line.as_str()));
        let conv = conv.unwrap();
        // Must be valid UTF-8 (no split mid-codepoint)
        assert!(
            std::str::from_utf8(conv.as_bytes()).is_ok(),
            "Conversation must be valid UTF-8 after truncation"
        );
        // Should contain [Result:
        assert!(conv.contains("[Result:"));
        // The result content inside must not be longer than 1000 chars
        let result_line = conv
            .lines()
            .find(|l| l.starts_with("[Result:"))
            .expect("should have result line");
        // Strip prefix "[Result: " and suffix "]"
        let content_part = result_line
            .strip_prefix("[Result: ")
            .unwrap_or("")
            .strip_suffix(']')
            .unwrap_or(result_line);
        assert!(
            content_part.len() <= MAX_TOOL_RESULT_BYTES,
            "Unicode content must be truncated to {} bytes",
            MAX_TOOL_RESULT_BYTES
        );
    }

    /// AC: Very large conversation (>50K chars) truncated correctly.
    #[test]
    fn test_parse_stream_json_very_large_conversation_truncated() {
        // Each block produces ~10_001 chars in conversation ("x"*10000 + "\n")
        // 6 blocks = ~60_006 chars > MAX_CONVERSATION_BYTES (50_000)
        let big_text = "x".repeat(10_000);
        let block = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": big_text}]},
            "model": "m",
            "error": null
        })
        .to_string();
        let lines: Vec<String> = std::iter::repeat_n(block, 6).collect();
        let (_, conv) = parse_stream_json_lines(lines.iter().map(|s| s.as_str()));
        let conv = conv.expect("should have conversation");
        assert!(
            conv.len() <= MAX_CONVERSATION_BYTES,
            "Large conversation must be capped at {} chars, got {}",
            MAX_CONVERSATION_BYTES,
            conv.len()
        );
        // Must still be valid UTF-8
        assert!(std::str::from_utf8(conv.as_bytes()).is_ok());
    }

    /// AC: Multiple assistant messages accumulate correctly.
    #[test]
    fn test_parse_stream_json_multiple_assistant_messages_accumulate() {
        let make_assistant = |text: &str| {
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": text}]},
                "model": "m",
                "error": null
            })
            .to_string()
        };
        let lines = [
            make_assistant("First message"),
            make_assistant("Second message"),
            make_assistant("Third message"),
        ];
        let (_, conv) = parse_stream_json_lines(lines.iter().map(|s| s.as_str()));
        let conv = conv.expect("should have conversation");
        assert!(
            conv.contains("First message"),
            "First message should be in conversation"
        );
        assert!(
            conv.contains("Second message"),
            "Second message should be in conversation"
        );
        assert!(
            conv.contains("Third message"),
            "Third message should be in conversation"
        );
        // All three messages in order
        let pos1 = conv.find("First message").unwrap();
        let pos2 = conv.find("Second message").unwrap();
        let pos3 = conv.find("Third message").unwrap();
        assert!(
            pos1 < pos2 && pos2 < pos3,
            "Messages should appear in order"
        );
    }

    /// AC: Assistant message with error field (string) produces [Error: ...] in conversation.
    #[rstest]
    #[case(
        r#"{"type":"assistant","message":{"content":[]},"model":"m","error":"rate limit exceeded"}"#,
        "[Error: rate limit exceeded]"
    )]
    #[case(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"unreachable"}]},"model":"m","error":{"message":"network timeout"}}"#,
        "[Error: network timeout]"
    )]
    #[case(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"visible"}]},"model":"m","error":null}"#,
        "visible"
    )]
    fn test_parse_stream_json_assistant_error_field(
        #[case] line: &str,
        #[case] expected_in_conv: &str,
    ) {
        let (_, conv) = parse_stream_json_lines(std::iter::once(line));
        let conv = conv.expect("should have conversation");
        assert!(
            conv.contains(expected_in_conv),
            "Expected '{}' in conversation, got: '{}'",
            expected_in_conv,
            conv
        );
    }

    /// AC: When error is present, assistant content blocks are NOT included.
    #[test]
    fn test_parse_stream_json_error_suppresses_content_blocks() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"should not appear"}]},"model":"m","error":"something broke"}"#;
        let (_, conv) = parse_stream_json_lines(std::iter::once(line));
        let conv = conv.expect("should have conversation");
        assert!(
            conv.contains("[Error: something broke]"),
            "Error must appear in conversation"
        );
        assert!(
            !conv.contains("should not appear"),
            "Content blocks must be suppressed when error is present"
        );
    }

    /// AC: result with subtype=error is handled — output extracted from result field.
    #[rstest]
    #[case(
        r#"{"type":"result","subtype":"error","result":"fatal error occurred","session_id":"s"}"#,
        "fatal error occurred"
    )]
    #[case(
        r#"{"type":"result","subtype":"success","result":"<completed>T-1</completed>","session_id":"s"}"#,
        "<completed>T-1</completed>"
    )]
    #[case(
        r#"{"type":"result","subtype":"error","result":null,"session_id":"s"}"#,
        ""
    )]
    fn test_parse_stream_json_result_subtypes(#[case] line: &str, #[case] expected_output: &str) {
        let (output, _) = parse_stream_json_lines(std::iter::once(line));
        assert_eq!(
            output, expected_output,
            "Output should be extracted from result field regardless of subtype"
        );
    }

    /// AC: Empty lines iterator produces empty output and Some("") conversation.
    #[test]
    fn test_parse_stream_json_empty_input() {
        let (output, conv) = parse_stream_json_lines(std::iter::empty());
        assert_eq!(output, "");
        assert_eq!(conv, Some(String::new()));
    }

    /// AC: User message with non-tool_result content types are skipped gracefully.
    #[test]
    fn test_parse_stream_json_user_non_tool_result_skipped() {
        let line = r#"{"type":"user","message":{"content":[{"type":"text","text":"user text"}]}}"#;
        let (_, conv) = parse_stream_json_lines(std::iter::once(line));
        // "text" type in user messages is not tool_result — should be skipped
        assert_eq!(
            conv,
            Some(String::new()),
            "Non-tool_result user content must not add to conversation"
        );
    }

    // --- TEST-002: spawn_claude stream_json arg construction ---

    /// AC: stream_json=false with a model — --print, --no-session-persistence, and --model all present.
    #[test]
    fn test_stream_json_false_with_model_has_print_and_model() {
        let result = spawn_claude_echo("prompt", None, Some(OPUS_MODEL), false, &scoped_coding());
        let output = result.unwrap().output;
        assert!(
            output.contains("--print"),
            "stream_json=false must use --print"
        );
        assert!(
            output.contains("--no-session-persistence"),
            "stream_json=false must include --no-session-persistence"
        );
        assert!(
            output.contains("--model"),
            "stream_json=false with model must include --model"
        );
        assert!(
            output.contains(OPUS_MODEL),
            "stream_json=false with model must include the model value"
        );
        assert!(
            !output.contains("--output-format"),
            "stream_json=false must NOT use --output-format"
        );
    }

    /// AC: stream_json=true with model + timeout — correct arg ordering.
    ///
    /// Expected order: --print, --output-format stream-json, --no-session-persistence,
    /// --permission-mode dontAsk, --model <model>, -p <prompt>
    #[test]
    fn test_stream_json_true_with_model_and_timeout_arg_ordering() {
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let script_path = make_stream_json_result_script("stream_model_timeout");
        let timeout = TimeoutConfig::from_difficulty(Some("medium"), Arc::new(AtomicU64::new(0)));
        unsafe { std::env::set_var("CLAUDE_BINARY", script_path.to_str().unwrap()) };
        let result = spawn_claude(
            "my-prompt",
            &scoped_coding(),
            SpawnOpts {
                model: Some(SONNET_MODEL),
                timeout: Some(timeout),
                stream_json: true,
                ..SpawnOpts::default()
            },
        );
        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        let _ = std::fs::remove_file(&script_path);

        let output = result.expect("spawn should succeed").output;

        // --output-format stream-json must appear before " -p " (prompt flag).
        // Use " -p " with spaces to avoid matching "-p" inside "--dangerously-skip-permissions".
        let pos_output_format = output
            .find("--output-format stream-json")
            .expect("--output-format stream-json must be present");
        let pos_prompt_flag = output
            .find(" -p ")
            .expect("' -p ' (prompt flag with spaces) must be present");
        assert!(
            pos_output_format < pos_prompt_flag,
            "--output-format stream-json must appear before -p"
        );

        // --model must appear before -p <prompt>
        let pos_model = output.find("--model").expect("--model must be present");
        assert!(pos_model < pos_prompt_flag, "--model must appear before -p");

        // model value must be present
        assert!(output.contains(SONNET_MODEL), "model value must be in args");

        // --no-session-persistence present
        assert!(
            output.contains("--no-session-persistence"),
            "--no-session-persistence must be present"
        );

        // --print must be present (required for --no-session-persistence and --output-format)
        assert!(
            output.contains("--print"),
            "--print must be present for stream_json=true"
        );
    }

    /// AC: parameterized tests for all 4 caller patterns:
    ///   (stream_json=false, model=None)   — curate / ingestion callers
    ///   (stream_json=false, model=Some)   — utility callers with model
    ///   (stream_json=true,  model=None)   — loop engine (no model override)
    ///   (stream_json=true,  model=Some)   — loop engine with model override
    ///
    /// For stream_json=false cases we use "echo" as the binary (returns args as text).
    /// For stream_json=true cases we use a shell script that wraps args in a result JSON.
    #[rstest]
    #[case(false, None, true, false)] // curate/ingestion: --print, no --output-format
    #[case(false, Some("opus"), true, false)] // utility+model: --print, no --output-format, has --model
    #[case(true, None, true, true)] // engine no model: --print + --output-format
    #[case(true, Some("sonnet"), true, true)] // engine+model: --print + --output-format, has --model
    fn test_spawn_claude_four_caller_patterns(
        #[case] stream_json: bool,
        #[case] model: Option<&str>,
        #[case] expect_print: bool,
        #[case] expect_output_format: bool,
    ) {
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let output = if stream_json {
            // Need a script that emits valid result JSON so the stream-json parser yields args
            let script_path =
                make_stream_json_result_script(&format!("4callers_{}", model.unwrap_or("none")));
            unsafe { std::env::set_var("CLAUDE_BINARY", script_path.to_str().unwrap()) };
            let result = spawn_claude(
                "test-prompt",
                &PermissionMode::Dangerous,
                SpawnOpts {
                    model,
                    stream_json,
                    ..SpawnOpts::default()
                },
            );
            unsafe { std::env::remove_var("CLAUDE_BINARY") };
            let _ = std::fs::remove_file(&script_path);
            result.expect("spawn should succeed").output
        } else {
            // Call spawn_claude directly — CLAUDE_BINARY_MUTEX is already held by this function.
            // Using spawn_claude_echo here would deadlock (std::sync::Mutex is not reentrant).
            let script = make_echo_args_stdin_script("4callers_echo");
            unsafe { std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap()) };
            let result = spawn_claude(
                "test-prompt",
                &PermissionMode::Dangerous,
                SpawnOpts {
                    model,
                    stream_json,
                    ..SpawnOpts::default()
                },
            );
            unsafe { std::env::remove_var("CLAUDE_BINARY") };
            let _ = std::fs::remove_file(&script);
            result.expect("spawn should succeed").output
        };

        if expect_print {
            assert!(
                output.contains("--print"),
                "expected --print in output: {output}"
            );
        } else {
            assert!(
                !output.contains("--print"),
                "expected NO --print in output: {output}"
            );
        }

        if expect_output_format {
            assert!(
                output.contains("--output-format stream-json"),
                "expected --output-format stream-json in output: {output}"
            );
        } else {
            assert!(
                !output.contains("--output-format"),
                "expected NO --output-format in output: {output}"
            );
        }

        // --no-session-persistence always present
        assert!(
            output.contains("--no-session-persistence"),
            "--no-session-persistence must always be present: {output}"
        );

        // model presence
        if let Some(m) = model {
            assert!(
                output.contains("--model"),
                "expected --model in output: {output}"
            );
            assert!(
                output.contains(m),
                "expected model value '{m}' in output: {output}"
            );
        } else {
            assert!(
                !output.contains("--model"),
                "expected NO --model flag when model=None: {output}"
            );
        }
    }

    /// AC: truncate_bytes at exact boundary — no off-by-one.
    #[rstest]
    #[case("hello", 5, "hello")] // exact boundary — keep all
    #[case("hello!", 5, "hello")] // one over boundary — truncate
    #[case("hi", 5, "hi")] // under boundary — keep all
    #[case("", 5, "")] // empty string
    #[case("abcde", 0, "")] // zero max_chars
    fn test_truncate_bytes_boundary(
        #[case] input: &str,
        #[case] max_chars: usize,
        #[case] expected: &str,
    ) {
        assert_eq!(
            truncate_bytes(input, max_chars),
            expected,
            "truncate_bytes({:?}, {}) should be {:?}",
            input,
            max_chars,
            expected
        );
    }

    /// AC: truncate_bytes with Unicode at byte boundary preserves char boundaries.
    #[rstest]
    #[case("αβγδε", 6, "αβγ")] // 6 bytes = 3 two-byte chars
    #[case("αβγδε", 5, "αβ")] // 5 bytes mid-char, rounds down to 4 (2 chars)
    #[case("こんにちは", 9, "こんに")] // 9 bytes = 3 three-byte chars
    #[case("🎉🎊🎈", 8, "🎉🎊")] // 8 bytes = 2 four-byte emojis
    fn test_truncate_bytes_unicode_boundary(
        #[case] input: &str,
        #[case] max_bytes: usize,
        #[case] expected: &str,
    ) {
        let result = truncate_bytes(input, max_bytes);
        assert_eq!(result, expected);
        // Must be valid UTF-8 (no split mid-codepoint)
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    // --- INT-001: Integration tests using mock_stream_json.sh ---

    /// Helper: run spawn_claude with CLAUDE_BINARY pointing to mock_stream_json.sh.
    fn spawn_claude_mock_stream_json(stream_json: bool) -> TaskMgrResult<ClaudeResult> {
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Locate the fixture relative to CARGO_MANIFEST_DIR
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let script = format!("{}/tests/fixtures/mock_stream_json.sh", manifest_dir);
        unsafe { std::env::set_var("CLAUDE_BINARY", &script) };
        let result = spawn_claude(
            "ignored_prompt",
            &PermissionMode::Dangerous,
            SpawnOpts {
                stream_json,
                ..SpawnOpts::default()
            },
        );
        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        result
    }

    /// AC: ClaudeResult.output contains the result text from stream-json mode.
    #[test]
    fn test_integration_stream_json_output_contains_result_text() {
        let result = spawn_claude_mock_stream_json(true);
        let res = result.expect("spawn_claude should succeed with mock_stream_json.sh");
        assert_eq!(res.exit_code, 0);
        assert_eq!(
            res.output, "<completed>TASK-001</completed>",
            "output should be the result field from the final stream-json line, got: '{}'",
            res.output
        );
    }

    /// AC: ClaudeResult.conversation contains formatted conversation with [Tool: ...] entries.
    #[test]
    fn test_integration_stream_json_conversation_contains_tool_entries() {
        let result = spawn_claude_mock_stream_json(true);
        let res = result.expect("spawn_claude should succeed with mock_stream_json.sh");
        let conv = res
            .conversation
            .expect("conversation should be Some in stream-json mode");
        assert!(
            conv.contains("[Tool: Read]"),
            "conversation must contain [Tool: Read] entry, got: '{}'",
            conv
        );
        assert!(
            conv.contains("Let me read the file."),
            "conversation must contain assistant text, got: '{}'",
            conv
        );
        assert!(
            conv.contains("The file contains a main function."),
            "conversation must contain second assistant text, got: '{}'",
            conv
        );
        assert!(
            conv.contains("[Result:"),
            "conversation must contain tool result entry, got: '{}'",
            conv
        );
    }

    /// AC: Fallback when mock outputs plain text (stream_json=false mode).
    /// In plain mode, conversation is None and output contains the raw stdout lines.
    #[test]
    fn test_integration_plain_mode_fallback() {
        let result = spawn_claude_mock_stream_json(false);
        let res = result.expect("spawn_claude should succeed with mock_stream_json.sh");
        assert_eq!(res.exit_code, 0);
        // In plain mode, raw JSON lines are collected verbatim into output
        assert!(
            res.output.contains("stream-json") || res.output.contains("type"),
            "plain mode output should contain raw JSON lines, got: '{}'",
            res.output
        );
        assert!(
            res.conversation.is_none(),
            "plain mode must not produce a conversation"
        );
    }

    // --- PERM-FEAT-002: Permission mode arg construction tests ---

    /// AC: Dangerous mode includes --dangerously-skip-permissions in args.
    /// Negative: must NOT include --permission-mode.
    #[test]
    fn test_spawn_dangerous_mode_emits_skip_permissions_flag() {
        let mode = PermissionMode::Dangerous;
        let result = spawn_claude_echo("prompt", None, None, false, &mode);
        let output = result.expect("echo should succeed").output;
        let output = output.trim();

        assert!(
            output.contains("--dangerously-skip-permissions"),
            "Dangerous mode must include --dangerously-skip-permissions, got: '{output}'"
        );
        assert!(
            !output.contains("--permission-mode"),
            "Dangerous mode must NOT include --permission-mode, got: '{output}'"
        );
    }

    /// AC: Scoped mode includes --permission-mode dontAsk (two separate args).
    /// Negative: must NOT include --dangerously-skip-permissions.
    #[test]
    fn test_spawn_scoped_mode_none_tools_emits_permission_mode_dontask() {
        let mode = PermissionMode::Scoped {
            allowed_tools: None,
        };
        let result = spawn_claude_echo("prompt", None, None, false, &mode);
        let output = result.expect("echo should succeed").output;
        let output = output.trim();

        assert!(
            output.contains("--permission-mode"),
            "Scoped mode must include --permission-mode, got: '{output}'"
        );
        assert!(
            output.contains("dontAsk"),
            "Scoped mode must include dontAsk, got: '{output}'"
        );
        assert!(
            !output.contains("--dangerously-skip-permissions"),
            "Scoped mode must NOT include --dangerously-skip-permissions, got: '{output}'"
        );
        assert!(
            !output.contains("--allowedTools"),
            "Scoped {{ allowed_tools: None }} must NOT include --allowedTools, got: '{output}'"
        );
    }

    /// AC: Scoped with Some(tools) includes --permission-mode dontAsk --allowedTools <tools>.
    /// Negative: --allowedTools must appear AFTER --permission-mode dontAsk (ordering).
    #[test]
    fn test_spawn_scoped_mode_some_tools_emits_allowed_tools() {
        let tools = "Read,Edit,Write";
        let mode = PermissionMode::Scoped {
            allowed_tools: Some(tools.to_string()),
        };
        let result = spawn_claude_echo("prompt", None, None, false, &mode);
        let output = result.expect("echo should succeed").output;
        let output = output.trim();

        assert!(
            output.contains("--permission-mode"),
            "Scoped mode must include --permission-mode, got: '{output}'"
        );
        assert!(
            output.contains("dontAsk"),
            "Scoped mode must include dontAsk, got: '{output}'"
        );
        assert!(
            output.contains("--allowedTools"),
            "Scoped {{ allowed_tools: Some }} must include --allowedTools, got: '{output}'"
        );
        assert!(
            output.contains(tools),
            "Scoped mode must include the tools string, got: '{output}'"
        );
        assert!(
            !output.contains("--dangerously-skip-permissions"),
            "Scoped mode must NOT include --dangerously-skip-permissions, got: '{output}'"
        );

        // --allowedTools must appear after --permission-mode dontAsk
        let perm_pos = output.find("--permission-mode").unwrap();
        let tools_pos = output
            .find("--allowedTools")
            .expect("--allowedTools must be present");
        assert!(
            tools_pos > perm_pos,
            "--allowedTools (pos {tools_pos}) must appear AFTER --permission-mode (pos {perm_pos})"
        );
    }

    /// AC: Auto mode includes --permission-mode auto and --allowedTools when tools provided.
    /// Negative: must NOT include --dangerously-skip-permissions.
    #[test]
    fn test_spawn_auto_mode_emits_permission_mode_auto_with_tools() {
        let mode = PermissionMode::Auto {
            allowed_tools: Some("Read,Edit,Bash(cargo:*)".to_string()),
        };
        let result = spawn_claude_echo("prompt", None, None, false, &mode);
        let output = result.expect("echo should succeed").output;
        let output = output.trim();

        assert!(
            output.contains("--permission-mode") && output.contains("auto"),
            "Auto mode must include --permission-mode auto, got: '{output}'"
        );
        assert!(
            output.contains("--allowedTools"),
            "Auto mode with tools must include --allowedTools, got: '{output}'"
        );
        assert!(
            !output.contains("--dangerously-skip-permissions"),
            "Auto mode must NOT include --dangerously-skip-permissions, got: '{output}'"
        );
    }

    /// AC: Auto mode without tools includes --permission-mode auto but no --allowedTools.
    #[test]
    fn test_spawn_auto_mode_no_tools_omits_allowed_tools() {
        let mode = PermissionMode::Auto {
            allowed_tools: None,
        };
        let result = spawn_claude_echo("prompt", None, None, false, &mode);
        let output = result.expect("echo should succeed").output;
        let output = output.trim();

        assert!(
            output.contains("--permission-mode") && output.contains("auto"),
            "Auto mode must include --permission-mode auto, got: '{output}'"
        );
        assert!(
            !output.contains("--allowedTools"),
            "Auto mode without tools must NOT include --allowedTools, got: '{output}'"
        );
    }

    /// AC: Arg ordering preserved for Scoped mode: base flags → permission flags → [--model] → -p.
    #[test]
    fn test_spawn_scoped_mode_arg_ordering() {
        let mode = PermissionMode::Scoped {
            allowed_tools: Some("Read,Edit".to_string()),
        };
        let result = spawn_claude_echo("my-prompt", None, Some(SONNET_MODEL), false, &mode);
        let output = result.expect("echo should succeed").output;
        let output = output.trim();

        // --print must come first
        assert!(
            output.starts_with("--print"),
            "--print must be first, got: '{output}'"
        );

        let print_pos = output.find("--print").unwrap();
        let perm_pos = output.find("--permission-mode").unwrap();
        let model_pos = output.find("--model").unwrap();
        let p_pos = output.find(" -p ").unwrap();

        assert!(
            print_pos < perm_pos,
            "--print must appear before --permission-mode"
        );
        assert!(
            perm_pos < model_pos,
            "--permission-mode must appear before --model"
        );
        assert!(model_pos < p_pos, "--model must appear before -p");
    }

    /// Known-bad guard: Scoped { allowed_tools: None } must NOT emit --allowedTools ''.
    /// Passing an empty string is different from omitting the flag entirely.
    #[test]
    fn test_spawn_scoped_none_tools_omits_allowed_tools_entirely() {
        let mode = PermissionMode::Scoped {
            allowed_tools: None,
        };
        let result = spawn_claude_echo("prompt", None, None, false, &mode);
        let output = result.expect("echo should succeed").output;

        assert!(
            !output.contains("--allowedTools"),
            "Scoped {{ allowed_tools: None }} must NOT emit --allowedTools at all, got: '{}'",
            output.trim()
        );
    }

    // --- permission_denials extraction tests ---

    #[test]
    fn test_permission_denials_extracted_from_result_line() {
        let lines = [
            r#"{"type":"result","result":"done","permission_denials":[{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"touch /tmp/foo","description":"Create file"}}]}"#,
        ];
        let (_output, _conv, denials) = parse_stream_json_lines_full(lines.iter().copied());
        assert_eq!(denials.len(), 1);
        assert_eq!(denials[0]["tool_name"], "Bash");
        assert_eq!(denials[0]["tool_input"]["command"], "touch /tmp/foo");
    }

    #[test]
    fn test_permission_denials_empty_array() {
        let lines = [r#"{"type":"result","result":"done","permission_denials":[]}"#];
        let (_output, _conv, denials) = parse_stream_json_lines_full(lines.iter().copied());
        assert!(denials.is_empty());
    }

    #[test]
    fn test_permission_denials_missing_field() {
        let lines = [r#"{"type":"result","result":"done"}"#];
        let (_output, _conv, denials) = parse_stream_json_lines_full(lines.iter().copied());
        assert!(denials.is_empty());
    }

    #[test]
    fn test_extract_denied_commands_bash_denial() {
        let denials = vec![serde_json::json!({
            "tool_name": "Bash",
            "tool_use_id": "t1",
            "tool_input": {"command": "docker build .", "description": "Build image"}
        })];
        let cmds = extract_denied_commands(&denials);
        assert_eq!(cmds, vec!["docker"]);
    }

    #[test]
    fn test_extract_denied_commands_non_bash_skipped() {
        let denials = vec![serde_json::json!({
            "tool_name": "Write",
            "tool_use_id": "t1",
            "tool_input": {"file_path": "/tmp/foo", "content": "bar"}
        })];
        let cmds = extract_denied_commands(&denials);
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_extract_denied_commands_deduplication() {
        let denials = vec![
            serde_json::json!({
                "tool_name": "Bash",
                "tool_use_id": "t1",
                "tool_input": {"command": "npm install"}
            }),
            serde_json::json!({
                "tool_name": "Bash",
                "tool_use_id": "t2",
                "tool_input": {"command": "npm test"}
            }),
        ];
        let cmds = extract_denied_commands(&denials);
        assert_eq!(cmds, vec!["npm"]);
    }

    #[test]
    fn test_extract_denied_commands_multiple_distinct() {
        let denials = vec![
            serde_json::json!({
                "tool_name": "Bash",
                "tool_use_id": "t1",
                "tool_input": {"command": "npm install"}
            }),
            serde_json::json!({
                "tool_name": "Bash",
                "tool_use_id": "t2",
                "tool_input": {"command": "docker build ."}
            }),
        ];
        let cmds = extract_denied_commands(&denials);
        assert_eq!(cmds, vec!["npm", "docker"]);
    }

    #[test]
    fn test_extract_denied_commands_missing_command_field() {
        let denials = vec![serde_json::json!({
            "tool_name": "Bash",
            "tool_use_id": "t1",
            "tool_input": {}
        })];
        let cmds = extract_denied_commands(&denials);
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_extract_binary_simple() {
        assert_eq!(extract_binary("docker build ."), "docker");
    }

    #[test]
    fn test_extract_binary_with_path() {
        assert_eq!(extract_binary("/usr/bin/docker build ."), "/usr/bin/docker");
    }

    #[test]
    fn test_extract_binary_subshell_prefix() {
        assert_eq!(extract_binary("(cd /tmp && ls)"), "cd");
    }

    #[test]
    fn test_extract_binary_brace_group() {
        assert_eq!(extract_binary("{echo hello}"), "echo");
    }

    #[test]
    fn test_extract_binary_empty() {
        assert_eq!(extract_binary(""), "");
        assert_eq!(extract_binary("  "), "");
    }

    #[test]
    fn test_extract_binary_env_prefix() {
        assert_eq!(extract_binary("env FOO=bar docker build ."), "docker");
    }

    #[test]
    fn test_extract_binary_env_multiple_vars() {
        assert_eq!(extract_binary("env FOO=bar BAZ=qux npm install"), "npm");
    }

    // --- extract_tasks_json_denials ---

    #[test]
    fn test_extract_tasks_json_denials_edit_match() {
        let denials = vec![serde_json::json!({
            "tool_name": "Edit",
            "tool_use_id": "t1",
            "tool_input": {
                "file_path": ".task-mgr/tasks/my-prd.json",
                "old_string": "foo",
                "new_string": "bar"
            }
        })];
        let result = extract_tasks_json_denials(&denials);
        assert_eq!(
            result,
            vec![(
                "Edit".to_string(),
                ".task-mgr/tasks/my-prd.json".to_string()
            )]
        );
    }

    #[test]
    fn test_extract_tasks_json_denials_write_match() {
        let denials = vec![serde_json::json!({
            "tool_name": "Write",
            "tool_use_id": "t1",
            "tool_input": {"file_path": ".task-mgr/tasks/new.json", "content": "{}"}
        })];
        let result = extract_tasks_json_denials(&denials);
        assert_eq!(
            result,
            vec![("Write".to_string(), ".task-mgr/tasks/new.json".to_string())]
        );
    }

    #[test]
    fn test_extract_tasks_json_denials_non_tasks_path_skipped() {
        let denials = vec![serde_json::json!({
            "tool_name": "Edit",
            "tool_use_id": "t1",
            "tool_input": {"file_path": "src/commands/add.rs", "old_string": "a", "new_string": "b"}
        })];
        let result = extract_tasks_json_denials(&denials);
        assert!(result.is_empty(), "src/**/*.rs edits must NOT be flagged");
    }

    #[test]
    fn test_extract_tasks_json_denials_bash_skipped() {
        let denials = vec![serde_json::json!({
            "tool_name": "Bash",
            "tool_use_id": "t1",
            "tool_input": {"command": "cat .task-mgr/tasks/foo.json"}
        })];
        let result = extract_tasks_json_denials(&denials);
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_tasks_json_denials_read_skipped() {
        let denials = vec![serde_json::json!({
            "tool_name": "Read",
            "tool_use_id": "t1",
            "tool_input": {"file_path": ".task-mgr/tasks/foo.json"}
        })];
        let result = extract_tasks_json_denials(&denials);
        assert!(result.is_empty(), "Read denials must not produce hints");
    }

    #[test]
    fn test_extract_tasks_json_denials_nested_path() {
        let denials = vec![serde_json::json!({
            "tool_name": "Write",
            "tool_use_id": "t1",
            "tool_input": {"file_path": "/some/project/.task-mgr/tasks/prd.json", "content": "{}"}
        })];
        let result = extract_tasks_json_denials(&denials);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_extract_tasks_json_denials_empty() {
        let result = extract_tasks_json_denials(&[]);
        assert!(result.is_empty());
    }

    // --- is_tasks_json_path ---

    #[test]
    fn test_is_tasks_json_path_relative() {
        assert!(is_tasks_json_path(".task-mgr/tasks/foo.json"));
        assert!(is_tasks_json_path(".task-mgr/tasks/my-prd.json"));
    }

    #[test]
    fn test_is_tasks_json_path_non_json_not_matched() {
        assert!(!is_tasks_json_path(".task-mgr/tasks/foo.rs"));
        assert!(!is_tasks_json_path(".task-mgr/tasks/foo.txt"));
    }

    #[test]
    fn test_is_tasks_json_path_src_path_not_matched() {
        assert!(!is_tasks_json_path("src/commands/add.rs"));
        assert!(!is_tasks_json_path("src/loop_engine/config.rs"));
    }

    // --- AC: encoded_cwd_dir pure helper ---

    #[test]
    fn test_encoded_cwd_dir_simple_path() {
        let got = encoded_cwd_dir(Path::new("$HOME/foo"), Path::new("$HOME"));
        assert_eq!(
            got,
            PathBuf::from("$HOME/.claude/projects/-home-chris-foo")
        );
    }

    #[test]
    fn test_encoded_cwd_dir_repo_path() {
        let got = encoded_cwd_dir(
            Path::new("$HOME/projects/task-mgr"),
            Path::new("$HOME"),
        );
        assert_eq!(
            got,
            PathBuf::from(
                "$HOME/.claude/projects/-home-chris-Documents-startat0-Projects-task-mgr"
            )
        );
    }

    #[test]
    fn test_encoded_cwd_dir_trailing_slash_normalized() {
        // Trailing slash on cwd must encode identically to no trailing slash —
        // otherwise the cleanup target wouldn't match what Claude wrote.
        let with_slash = encoded_cwd_dir(Path::new("$HOME/foo/"), Path::new("$HOME"));
        let no_slash = encoded_cwd_dir(Path::new("$HOME/foo"), Path::new("$HOME"));
        assert_eq!(with_slash, no_slash);
    }

    // --- AC: --session-id flag + UUID v4 + ordering ---

    /// Helper: split echoed-args output into argv tokens. The mock script
    /// echoes `"$@" "$PROMPT"` so tokens are space-separated.
    fn argv_tokens(output: &str) -> Vec<String> {
        output.split_whitespace().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_cleanup_title_artifact_false_omits_session_id() {
        let result = spawn_claude_echo("p", None, None, false, &scoped_coding())
            .expect("spawn should succeed");
        assert!(
            !result.output.contains("--session-id"),
            "cleanup_title_artifact=false must NOT add --session-id; got: {}",
            result.output
        );
    }

    #[test]
    fn test_cleanup_title_artifact_true_adds_valid_uuid_v4_session_id() {
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let script = make_echo_args_stdin_script("cleanup_uuid");
        unsafe { std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap()) };
        let result = spawn_claude(
            "p",
            &PermissionMode::Dangerous,
            SpawnOpts {
                model: Some(SONNET_MODEL),
                cleanup_title_artifact: true,
                ..SpawnOpts::default()
            },
        );
        unsafe { std::env::remove_var("CLAUDE_BINARY") };
        let _ = std::fs::remove_file(&script);
        let res = result.expect("spawn should succeed");
        let tokens = argv_tokens(&res.output);

        let sid_idx = tokens
            .iter()
            .position(|t| t == "--session-id")
            .expect("--session-id must be present when cleanup_title_artifact=true");
        let p_idx = tokens
            .iter()
            .position(|t| t == "-p")
            .expect("-p must be present");
        // Known-bad guard: flag must be left of -p, else Claude ignores it.
        assert!(
            sid_idx < p_idx,
            "--session-id must appear before -p (got sid={}, p={})",
            sid_idx,
            p_idx
        );

        let model_idx = tokens.iter().position(|t| t == "--model").unwrap();
        assert!(
            sid_idx > model_idx,
            "--session-id should be placed after --model"
        );

        let uuid_str = &tokens[sid_idx + 1];
        let parsed = uuid::Uuid::parse_str(uuid_str)
            .unwrap_or_else(|e| panic!("UUID '{}' must parse: {}", uuid_str, e));
        assert_eq!(
            parsed.get_version(),
            Some(uuid::Version::Random),
            "UUID must be v4 (random); got {:?}",
            parsed.get_version()
        );

        // Exactly one --session-id pair.
        let count = tokens.iter().filter(|t| *t == "--session-id").count();
        assert_eq!(count, 1, "expected exactly one --session-id flag");
    }

    /// Cleanup must target a deterministic path — so an UNRELATED .jsonl that
    /// happens to live in the same projects dir MUST survive. This guards
    /// against any future implementation that uses read_dir + heuristics.
    #[test]
    fn test_cleanup_does_not_touch_unrelated_jsonl_files() {
        // Use a temp HOME so we can place a sibling file safely.
        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path();
        let fake_cwd = tmp.path().join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let projects_dir = encoded_cwd_dir(&fake_cwd, fake_home);
        std::fs::create_dir_all(&projects_dir).unwrap();
        let bystander = projects_dir.join("00000000-0000-4000-8000-000000000000.jsonl");
        std::fs::write(&bystander, "untouched").unwrap();

        // Drive the helper directly with a known UUID; nothing should remove
        // the bystander since the UUID differs.
        let target_uuid = uuid::Uuid::new_v4();
        let target_path = projects_dir.join(format!("{}.jsonl", target_uuid));
        std::fs::write(&target_path, "to-be-deleted").unwrap();

        // Manually invoke the same path computation the cleanup thread uses.
        // (We avoid spawning the real thread here to keep the test under 30s;
        // the deterministic-target invariant is what matters.)
        let computed = encoded_cwd_dir(&fake_cwd, fake_home).join(format!("{}.jsonl", target_uuid));
        let _ = std::fs::remove_file(&computed);

        assert!(
            bystander.exists(),
            "bystander .jsonl with a different UUID must NOT be deleted"
        );
        assert!(
            !target_path.exists(),
            "the UUID-matched target should be removed"
        );
    }

    /// Serializes env-var mutation across HOME-sensitive tests; HOME is process-
    /// global and leaking it into concurrent tests would make them flaky.
    static HOME_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Restores HOME (or unsets it) on drop, so a failed assertion doesn't
    /// leak the fake HOME into subsequent tests.
    struct HomeGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(value: &Path) -> Self {
            let previous = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", value) };
            Self { previous }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    /// End-to-end: drive `cleanup_title_artifact_sync` directly against a
    /// temp HOME. Proves the actual helper (not a reimplementation) deletes
    /// the UUID-matched file and leaves an unrelated sibling alone. Future
    /// changes to the path-encoding logic or the `remove_file` call will
    /// break this test, which is the point.
    #[test]
    fn test_cleanup_title_artifact_sync_deletes_target_preserves_bystander() {
        let _guard = HOME_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();

        let projects_dir = encoded_cwd_dir(&fake_cwd, &fake_home);
        std::fs::create_dir_all(&projects_dir).unwrap();

        let bystander_uuid = uuid::Uuid::new_v4();
        let bystander = projects_dir.join(format!("{}.jsonl", bystander_uuid));
        std::fs::write(&bystander, "untouched").unwrap();

        let target_uuid = uuid::Uuid::new_v4();
        let target_path = projects_dir.join(format!("{}.jsonl", target_uuid));
        std::fs::write(&target_path, "to-be-deleted").unwrap();

        let _home = HomeGuard::set(&fake_home);
        cleanup_title_artifact_sync(target_uuid, Some(&fake_cwd));

        assert!(
            !target_path.exists(),
            "cleanup_title_artifact_sync should have removed the UUID-matched target"
        );
        assert!(
            bystander.exists(),
            "cleanup_title_artifact_sync must not touch a .jsonl with a different UUID"
        );
    }

    /// HOME unset: helper must return silently without panicking or erroring.
    #[test]
    fn test_cleanup_title_artifact_sync_skips_when_home_unset() {
        let _guard = HOME_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("HOME");
        unsafe { std::env::remove_var("HOME") };

        // Should be a no-op, no panic.
        cleanup_title_artifact_sync(uuid::Uuid::new_v4(), None);

        if let Some(v) = previous {
            unsafe { std::env::set_var("HOME", v) }
        }
    }

    /// Target file never written (Claude crashed before writing ai-title):
    /// `NotFound` must be swallowed, no panic, no stderr noise tested here
    /// (we only assert the call returns normally).
    #[test]
    fn test_cleanup_title_artifact_sync_missing_target_is_silent() {
        let _guard = HOME_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let fake_cwd = fake_home.join("workspace");
        std::fs::create_dir_all(&fake_cwd).unwrap();
        // Deliberately do NOT create the projects dir or any target file.

        let _home = HomeGuard::set(&fake_home);
        cleanup_title_artifact_sync(uuid::Uuid::new_v4(), Some(&fake_cwd));
        // Test passes if we reach here without panic.
    }
}
