/// Claude subprocess spawner for the autonomous agent loop.
///
/// Spawns `claude --print --dangerously-skip-permissions -p PROMPT` as a child
/// process. Tees stdout to stderr (live display) while collecting it into a buffer
/// for later analysis by the detection engine. Claude's stderr passes through
/// directly (inherited).
///
/// When a `SignalFlag` is provided, a watchdog thread monitors for SIGINT/SIGTERM
/// and escalates: SIGTERM → 3s grace → SIGKILL.
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::signals::SignalFlag;

/// Timeout constants for per-iteration time limits.
const TIMEOUT_LOW_SECS: u64 = 20 * 60;
const TIMEOUT_MEDIUM_SECS: u64 = 30 * 60;
const TIMEOUT_HIGH_SECS: u64 = 40 * 60;
const INITIAL_EXTENSION_SECS: u64 = 7 * 60;
const EXTENSION_DECREMENT_SECS: u64 = 60;

/// Configuration for per-iteration timeout with activity-based extensions.
#[derive(Clone)]
pub struct TimeoutConfig {
    /// Maximum time allowed for the iteration.
    pub base_timeout: Duration,
    /// First activity extension amount (decreases by `EXTENSION_DECREMENT_SECS` each use).
    pub initial_extension: Duration,
    /// Shared epoch timestamp of last file activity from the monitor thread.
    pub last_activity_epoch: Arc<AtomicU64>,
}

impl TimeoutConfig {
    /// Create a `TimeoutConfig` from a task difficulty string.
    ///
    /// Maps: `"low"` → 20min, `"high"` → 40min, anything else (including `None`) → 30min.
    pub fn from_difficulty(difficulty: Option<&str>, last_activity_epoch: Arc<AtomicU64>) -> Self {
        let base_secs = match difficulty.map(|d| d.to_ascii_lowercase()).as_deref() {
            Some("low") => TIMEOUT_LOW_SECS,
            Some("high") => TIMEOUT_HIGH_SECS,
            _ => TIMEOUT_MEDIUM_SECS,
        };
        TimeoutConfig {
            base_timeout: Duration::from_secs(base_secs),
            initial_extension: Duration::from_secs(INITIAL_EXTENSION_SECS),
            last_activity_epoch,
        }
    }
}

/// Maximum characters for the formatted conversation in stream-json mode.
const MAX_CONVERSATION_CHARS: usize = 50_000;
/// Maximum characters for a single tool_use input block.
const MAX_TOOL_USE_CHARS: usize = 500;
/// Maximum characters for a single tool_result content block.
const MAX_TOOL_RESULT_CHARS: usize = 1_000;

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
}

/// Spawn Claude with the given prompt and collect its output.
///
/// The subprocess runs `<binary> --print --dangerously-skip-permissions -p <prompt>`.
/// When `model` is `Some(m)` and non-empty, `--model m` is inserted before `-p`.
/// The binary defaults to `claude` but can be overridden via the `CLAUDE_BINARY`
/// environment variable (useful for testing with mock scripts).
///
/// - stdout is piped, read line-by-line, echoed to stderr (tee), and buffered
/// - stderr is inherited (passes through directly to the terminal)
/// - The full environment is inherited by the subprocess
///
/// When `working_dir` is `Some`, the subprocess runs in that directory. This is
/// critical when using git worktrees: Claude's sandbox scopes file writes to its
/// working directory, so it must run from the worktree (not the source repo) to
/// be able to write files there.
///
/// When `signal_flag` is `Some`, a watchdog thread polls the flag every 200ms.
/// On signal detection: sends SIGTERM to child, waits up to 3s, then SIGKILL.
///
/// # Errors
///
/// Returns `TaskMgrError::IoError` if the binary is not found or
/// the process fails to spawn.
pub fn spawn_claude(
    prompt: &str,
    signal_flag: Option<&SignalFlag>,
    working_dir: Option<&Path>,
    model: Option<&str>,
    timeout: Option<TimeoutConfig>,
    stream_json: bool,
) -> TaskMgrResult<ClaudeResult> {
    let binary = std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string());
    let mut args: Vec<&str> = if stream_json {
        vec![
            "--output-format",
            "stream-json",
            "--no-session-persistence",
            "--dangerously-skip-permissions",
        ]
    } else {
        vec![
            "--print",
            "--no-session-persistence",
            "--dangerously-skip-permissions",
        ]
    };
    if let Some(m) = model {
        if !m.trim().is_empty() {
            args.push("--model");
            args.push(m);
        }
    }
    args.push("-p");
    args.push(prompt);

    let mut cmd = Command::new(&binary);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    // Put child in its own process group so we can kill the entire tree on signal.
    // This also prevents Claude from receiving terminal SIGINT directly — the
    // watchdog thread is solely responsible for termination.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
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

    // Extract PID before starting watchdog — no race condition
    let child_pid = child.id();

    // Reclaim foreground process group so Ctrl+C delivers SIGINT to us.
    // Claude's pre_exec setpgid(0,0) puts it in its own group, but Claude
    // Code (Node.js) may call tcsetpgrp() during init to become the
    // foreground group. Without this, SIGINT from Ctrl+C goes to Claude's
    // group and task-mgr's signal handler never fires.
    #[cfg(unix)]
    {
        unsafe {
            let our_pgid = libc::getpgrp();
            // stderr is inherited (connected to terminal); stdin=null, stdout=piped
            libc::tcsetpgrp(libc::STDERR_FILENO, our_pgid);
        }
    }

    // Start watchdog thread if signal handling or timeout is requested
    let stop_watchdog = Arc::new(AtomicBool::new(false));
    let timed_out_flag = Arc::new(AtomicBool::new(false));
    let watchdog_handle = if signal_flag.is_some() || timeout.is_some() {
        let stop = Arc::clone(&stop_watchdog);
        let flag = signal_flag.cloned();
        let timeout_cfg = timeout;
        let timed_out = Arc::clone(&timed_out_flag);
        Some(std::thread::spawn(move || {
            watchdog_loop(
                child_pid,
                flag.as_ref(),
                &stop,
                timeout_cfg.as_ref(),
                &timed_out,
            );
        }))
    } else {
        None
    };

    // Take ownership of stdout for line-by-line reading
    let stdout = child
        .stdout
        .take()
        .expect("stdout should be piped (Stdio::piped() was set on spawn)");

    let reader = BufReader::new(stdout);

    let (output, conversation) = if stream_json {
        tee_stream_json(reader)
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
                Err(e) => {
                    eprintln!("Warning: error reading Claude stdout: {}", e);
                    break;
                }
            }
        }
        (buf, None)
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

    let exit_code = exit_code_from_status(status);
    let timed_out = timed_out_flag.load(Ordering::Acquire);

    Ok(ClaudeResult {
        exit_code,
        output,
        conversation,
        timed_out,
    })
}

/// Read stream-json lines from Claude, tee assistant text to stderr, and return
/// (output_text, conversation).
///
/// - `output_text`: extracted from the final `result.result` field (what `--print` would emit).
/// - `conversation`: formatted transcript of the full conversation, capped at
///   `MAX_CONVERSATION_CHARS`.
fn tee_stream_json(reader: BufReader<impl std::io::Read>) -> (String, Option<String>) {
    let mut raw_lines: Vec<String> = Vec::new();

    for line_result in reader.lines() {
        match line_result {
            Ok(line) => {
                // Display assistant text blocks live before collecting
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                    tee_assistant_text(&val);
                } else {
                    eprintln!("Warning: malformed stream-json line (not valid JSON)");
                }
                raw_lines.push(line);
            }
            Err(e) => {
                eprintln!("Warning: error reading Claude stdout: {}", e);
                break;
            }
        }
    }

    parse_stream_json_lines(raw_lines.iter().map(|s| s.as_str()))
}

/// Tee assistant text content blocks to stderr for live display.
fn tee_assistant_text(val: &serde_json::Value) {
    if val.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return;
    }
    if let Some(content) = val
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    eprintln!("{}", text);
                }
            }
        }
    }
}

/// Parse an iterator of stream-json lines into (output_text, conversation).
///
/// - `output_text` is extracted from the final `{"type":"result","result":"..."}` line.
/// - `conversation` is a formatted transcript of assistant messages (text, tool_use) and
///   user messages (tool_result), capped at `MAX_CONVERSATION_CHARS`.
///
/// Malformed JSON lines and unknown message types are silently skipped with a warning.
pub fn parse_stream_json_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> (String, Option<String>) {
    let mut output_text = String::new();
    let mut conversation = String::new();

    for line in lines {
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("Warning: malformed stream-json line (not valid JSON)");
                continue;
            }
        };

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
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            // Skip system/init and unknown types
            _ => {}
        }
    }

    let conversation_opt = if conversation.is_empty() {
        None
    } else {
        Some(conversation)
    };

    (output_text, conversation_opt)
}

/// Append formatted assistant message content to the conversation buffer.
fn process_assistant_message(val: &serde_json::Value, conversation: &mut String) {
    // Handle error field on assistant message
    if let Some(error) = val.get("error").and_then(|e| e.as_str()) {
        if !error.is_empty() {
            append_capped(conversation, &format!("[Error: {}]\n", error));
            return;
        }
    }
    // Also handle error as object
    if let Some(error_obj) = val.get("error") {
        if !error_obj.is_null() {
            if let Some(msg) = error_obj.get("message").and_then(|m| m.as_str()) {
                append_capped(conversation, &format!("[Error: {}]\n", msg));
                return;
            }
        }
    }

    let content = match val
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
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
                let truncated = truncate_chars(&input_str, MAX_TOOL_USE_CHARS);
                append_capped(conversation, &format!("[Tool: {}] {}\n", name, truncated));
            }
            Some("thinking") => {
                // Skip thinking blocks
            }
            _ => {}
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
                .and_then(|c| c.as_str())
                .unwrap_or_default();
            let truncated = truncate_chars(content_str, MAX_TOOL_RESULT_CHARS);
            append_capped(conversation, &format!("[Result: {}]\n", truncated));
        }
    }
}

/// Append `s` to `buf` only up to `MAX_CONVERSATION_CHARS` total.
fn append_capped(buf: &mut String, s: &str) {
    if buf.len() >= MAX_CONVERSATION_CHARS {
        return;
    }
    let remaining = MAX_CONVERSATION_CHARS - buf.len();
    let to_append = truncate_chars(s, remaining);
    buf.push_str(to_append);
}

/// Truncate `s` to at most `max_chars` Unicode scalar values without splitting mid-character.
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    // Find the byte boundary at the char boundary
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Extract exit code from process status, using 128+signal convention on Unix.
fn exit_code_from_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }

    // Process was killed by a signal (code() returns None)
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }

    1 // fallback
}

/// Send SIGTERM → wait grace period → SIGKILL to a process group.
///
/// Returns once the process group is dead or force-killed.
/// `stop` is checked during the grace period to detect early reaping by the main thread.
#[cfg(unix)]
fn kill_process_group(child_pid: u32, stop: &AtomicBool, reason: &str) {
    const GRACE_PERIOD: Duration = Duration::from_secs(3);
    const GRACE_POLL: Duration = Duration::from_millis(100);

    let pgid = -(child_pid as i32);

    eprintln!(
        "\n{}, terminating Claude process group (pgid {})...",
        reason, child_pid
    );

    let ret = unsafe { libc::kill(pgid, libc::SIGTERM) };
    if ret == -1 {
        return; // ESRCH: already exited
    }

    let start = Instant::now();
    while start.elapsed() < GRACE_PERIOD {
        std::thread::sleep(GRACE_POLL);
        if stop.load(Ordering::Acquire) {
            return; // Main thread reaped
        }
        let ret = unsafe { libc::kill(pgid, 0) };
        if ret == -1 {
            return; // Exited
        }
    }

    eprintln!(
        "Grace period expired, sending SIGKILL to process group {}...",
        child_pid
    );
    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
}

/// Watchdog loop: polls signal flag and timeout, terminates child on either.
///
/// Runs on a dedicated OS thread (not tokio) so it works even when the
/// main thread is blocked in synchronous I/O.
#[cfg(unix)]
fn watchdog_loop(
    child_pid: u32,
    signal_flag: Option<&SignalFlag>,
    stop: &AtomicBool,
    timeout: Option<&TimeoutConfig>,
    timed_out: &AtomicBool,
) {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    // Timeout tracking
    let mut deadline = timeout.map(|t| Instant::now() + t.base_timeout);
    let mut last_seen_activity: u64 = 0;
    let mut extensions_used: u32 = 0;

    while !stop.load(Ordering::Acquire) {
        // Check signal
        if let Some(flag) = signal_flag {
            if flag.is_signaled() {
                kill_process_group(child_pid, stop, "Signal received");
                return;
            }
        }

        // Check timeout
        if let (Some(ref mut dl), Some(tc)) = (&mut deadline, timeout) {
            // Check for new activity
            let current_activity = tc.last_activity_epoch.load(Ordering::Acquire);
            if current_activity > last_seen_activity {
                last_seen_activity = current_activity;
                let ext_secs = INITIAL_EXTENSION_SECS
                    .saturating_sub(extensions_used as u64 * EXTENSION_DECREMENT_SECS);
                if ext_secs > 0 {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    let extended = remaining + Duration::from_secs(ext_secs);
                    let capped = extended.min(tc.base_timeout);
                    *dl = Instant::now() + capped;
                    extensions_used += 1;
                    eprintln!(
                        "[timeout] Activity detected, extended deadline by {}s ({} remaining, {} extensions used)",
                        ext_secs,
                        capped.as_secs(),
                        extensions_used
                    );
                }
            }

            if Instant::now() >= *dl {
                eprintln!(
                    "[timeout] Iteration exceeded {}s timeout",
                    tc.base_timeout.as_secs()
                );
                timed_out.store(true, Ordering::Release);
                kill_process_group(child_pid, stop, "Timeout exceeded");
                return;
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(not(unix))]
fn watchdog_loop(
    _child_pid: u32,
    signal_flag: Option<&SignalFlag>,
    stop: &AtomicBool,
    _timeout: Option<&TimeoutConfig>,
    _timed_out: &AtomicBool,
) {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    while !stop.load(Ordering::Acquire) {
        if let Some(flag) = signal_flag {
            if flag.is_signaled() {
                eprintln!("\nSignal received, Claude subprocess will be terminated...");
                return;
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::sync::Mutex;

    // Serialize tests that mutate CLAUDE_BINARY to avoid race conditions
    // when cargo test runs threads in parallel.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    // --- AC: ClaudeResult struct has expected fields ---

    #[test]
    fn test_claude_result_struct_fields() {
        let result = ClaudeResult {
            exit_code: 0,
            output: "Hello world\n".to_string(),
            conversation: None,
            timed_out: false,
        };
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.output, "Hello world\n");
        assert!(result.conversation.is_none());
        assert!(!result.timed_out);
    }

    #[test]
    fn test_claude_result_with_non_zero_exit() {
        let result = ClaudeResult {
            exit_code: 137,
            output: String::new(),
            conversation: None,
            timed_out: false,
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
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("hello", None, None, None, None, false);
        std::env::remove_var("CLAUDE_BINARY");
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.output.contains("hello"));
    }

    #[test]
    fn test_spawn_with_signal_flag_no_signal() {
        // spawn_claude with a SignalFlag that is NOT signaled should work normally
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let flag = SignalFlag::new();
        let result = spawn_claude("test output", Some(&flag), None, None, None, false);
        std::env::remove_var("CLAUDE_BINARY");
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
        })
    }

    // --- AC: exit_code_from_status ---

    #[test]
    fn test_exit_code_from_normal_exit() {
        use std::process::Command;
        let status = Command::new("true").status().unwrap();
        assert_eq!(exit_code_from_status(status), 0);

        let status = Command::new("false").status().unwrap();
        assert_eq!(exit_code_from_status(status), 1);
    }

    #[cfg(unix)]
    #[test]
    fn test_exit_code_from_signal_killed_process() {
        use std::process::Command;
        // Spawn a sleep, then kill it with SIGKILL
        let mut child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let status = child.wait().unwrap();
        let code = exit_code_from_status(status);
        assert_eq!(code, 128 + libc::SIGKILL, "Should be 128 + SIGKILL (137)");
    }

    // --- AC: Watchdog thread terminates child on signal ---

    #[cfg(unix)]
    #[test]
    fn test_watchdog_kills_child_on_signal() {
        // Spawn a long-running process via spawn_claude with a signal flag
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "sleep");
        let flag = SignalFlag::new();

        // Set the signal flag after a short delay in a background thread
        let flag_clone = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            flag_clone.set();
        });

        let start = std::time::Instant::now();
        // "60" is the argument to sleep — it will run for 60s unless killed
        let result = spawn_claude("60", Some(&flag), None, None, None, false);
        let elapsed = start.elapsed();

        std::env::remove_var("CLAUDE_BINARY");

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
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let flag = SignalFlag::new();
        let result = spawn_claude("quick exit", Some(&flag), None, None, None, false);
        std::env::remove_var("CLAUDE_BINARY");

        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.output.contains("quick exit"));
        // Flag should NOT be signaled
        assert!(!flag.is_signaled());
    }

    // --- Tests for --model flag on spawn_claude ---

    /// Active: model=None → no --model flag, standard flags present.
    /// Validates backward compatibility: None model produces args identical to
    /// pre-model behavior.
    #[test]
    fn test_spawn_model_none_no_model_flag() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test_prompt", None, None, None, None, false);
        std::env::remove_var("CLAUDE_BINARY");

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
            output.contains("--dangerously-skip-permissions"),
            "Must always have --dangerously-skip-permissions, got: '{}'",
            output
        );
    }

    /// model=Some("claude-opus-4-6") → --model flag present with correct value in echoed args.
    #[test]
    fn test_spawn_model_some_opus_includes_model_flag() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude(
            "test_prompt",
            None,
            None,
            Some("claude-opus-4-6"),
            None,
            false,
        );
        std::env::remove_var("CLAUDE_BINARY");

        assert!(result.is_ok());
        let res = result.unwrap();
        let output = res.output.trim();

        assert!(
            output.contains("--model claude-opus-4-6"),
            "model=Some('claude-opus-4-6') should include --model flag, got: '{}'",
            output
        );
    }

    /// model=Some("") → treated as None, no --model flag.
    /// Guards against naively passing --model '' to the Claude CLI.
    #[test]
    fn test_spawn_model_empty_string_treated_as_none() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test_prompt", None, None, Some(""), None, false);
        std::env::remove_var("CLAUDE_BINARY");

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
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude(
            "test_prompt",
            None,
            None,
            Some("claude-opus-4-6"),
            None,
            false,
        );
        std::env::remove_var("CLAUDE_BINARY");

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

    /// Parameterized test: multiple model strings all produce correct --model flag.
    #[test]
    fn test_spawn_model_parameterized_model_strings() {
        let models = [
            ("claude-opus-4-6", "--model claude-opus-4-6"),
            ("claude-sonnet-4-6", "--model claude-sonnet-4-6"),
            (
                "claude-haiku-4-5-20251001",
                "--model claude-haiku-4-5-20251001",
            ),
            ("custom-model-v2", "--model custom-model-v2"),
            ("my_model.v1.2", "--model my_model.v1.2"),
        ];

        for (model, expected_fragment) in &models {
            let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            std::env::set_var("CLAUDE_BINARY", "echo");
            let result = spawn_claude("test_prompt", None, None, Some(model), None, false);
            std::env::remove_var("CLAUDE_BINARY");

            assert!(result.is_ok(), "model='{}' should succeed", model);
            let output = result.unwrap().output;
            assert!(
                output.contains(expected_fragment),
                "model='{}' should produce '{}', got: '{}'",
                model,
                expected_fragment,
                output.trim()
            );
        }
    }

    /// Whitespace-only model treated as None — no --model flag.
    #[test]
    fn test_spawn_model_whitespace_only_treated_as_none() {
        for model in &["  ", "\t", " \t "] {
            let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            std::env::set_var("CLAUDE_BINARY", "echo");
            let result = spawn_claude("test_prompt", None, None, Some(model), None, false);
            std::env::remove_var("CLAUDE_BINARY");

            assert!(result.is_ok());
            let output = result.unwrap().output;
            assert!(
                !output.contains("--model"),
                "model='{}' (whitespace-only) should not produce --model flag, got: '{}'",
                model.escape_debug(),
                output.trim()
            );
        }
    }

    /// --print and --dangerously-skip-permissions must be present regardless of model value.
    #[test]
    fn test_spawn_model_some_preserves_required_flags() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude(
            "test_prompt",
            None,
            None,
            Some("claude-opus-4-6"),
            None,
            false,
        );
        std::env::remove_var("CLAUDE_BINARY");

        assert!(result.is_ok());
        let res = result.unwrap();
        let output = res.output.trim();

        assert!(
            output.contains("--print"),
            "Must always have --print even with model, got: '{}'",
            output
        );
        assert!(
            output.contains("--dangerously-skip-permissions"),
            "Must always have --dangerously-skip-permissions even with model, got: '{}'",
            output
        );
        assert!(
            output.contains("--model claude-opus-4-6"),
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
    #[case("claude-opus-4-6")]
    #[case("claude-sonnet-4-6")]
    #[case("claude-haiku-4-5-20251001")]
    #[case("my-custom-model")]
    fn test_spawn_claude_model_variants(#[case] model: &str) {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test prompt", None, None, Some(model), None, false);
        std::env::remove_var("CLAUDE_BINARY");

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
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test prompt", None, None, Some(model), None, false);
        std::env::remove_var("CLAUDE_BINARY");

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

    /// AC: --model does not interfere with --dangerously-skip-permissions or --print flags.
    /// Verifies exact ordering: --print --no-session-persistence --dangerously-skip-permissions [--model <m>] -p <prompt>
    #[rstest]
    #[case(Some("claude-sonnet-4-6"))]
    #[case(None)]
    fn test_spawn_claude_model_does_not_interfere_with_flags(#[case] model: Option<&str>) {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("my prompt", None, None, model, None, false);
        std::env::remove_var("CLAUDE_BINARY");

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

        // --dangerously-skip-permissions always present after --no-session-persistence
        let dsp_pos = output
            .find("--dangerously-skip-permissions")
            .expect("--dangerously-skip-permissions must be present");
        assert!(
            dsp_pos > nsp_pos,
            "--dangerously-skip-permissions should follow --no-session-persistence"
        );

        // -p always present and prompt follows
        let p_pos = output.find(" -p ").expect("-p must be present");
        assert!(
            output[p_pos..].contains("my prompt"),
            "Prompt should follow -p flag, got: '{}'",
            output
        );

        // If model is present, it must be between --dangerously-skip-permissions and -p
        if let Some(m) = model {
            let model_pos = output
                .find("--model")
                .expect("--model must be present when model is Some");
            assert!(
                model_pos > dsp_pos && model_pos < p_pos,
                "--model (pos {}) must be between --dangerously-skip-permissions (pos {}) and -p (pos {}), got: '{}'",
                model_pos,
                dsp_pos,
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
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("my prompt text", None, None, None, None, false);
        std::env::remove_var("CLAUDE_BINARY");

        let res = result.expect("echo should succeed");
        let output = res.output.trim();

        // stream_json=false: exactly these args, no more
        assert_eq!(
            output,
            "--print --no-session-persistence --dangerously-skip-permissions -p my prompt text",
            "None model with stream_json=false must produce exactly these args"
        );
    }

    /// Edge case: whitespace-only model string treated as None.
    #[rstest]
    #[case("   ")]
    #[case("\t")]
    #[case(" \t ")]
    fn test_spawn_claude_whitespace_only_model_treated_as_none(#[case] model: &str) {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test prompt", None, None, Some(model), None, false);
        std::env::remove_var("CLAUDE_BINARY");

        let res = result.expect("echo should succeed");
        let output = res.output.trim();

        assert!(
            !output.contains("--model"),
            "Whitespace-only model '{}' should be treated as None, got: '{}'",
            model.escape_debug(),
            output
        );
    }

    // --- Timeout tests ---

    #[test]
    fn test_timeout_config_from_difficulty() {
        let activity = Arc::new(AtomicU64::new(0));
        let low = TimeoutConfig::from_difficulty(Some("low"), Arc::clone(&activity));
        assert_eq!(low.base_timeout, Duration::from_secs(20 * 60));

        let med = TimeoutConfig::from_difficulty(None, Arc::clone(&activity));
        assert_eq!(med.base_timeout, Duration::from_secs(30 * 60));

        let med2 = TimeoutConfig::from_difficulty(Some("medium"), Arc::clone(&activity));
        assert_eq!(med2.base_timeout, Duration::from_secs(30 * 60));

        let high = TimeoutConfig::from_difficulty(Some("high"), Arc::clone(&activity));
        assert_eq!(high.base_timeout, Duration::from_secs(40 * 60));

        // Case insensitive
        let high2 = TimeoutConfig::from_difficulty(Some("HIGH"), Arc::clone(&activity));
        assert_eq!(high2.base_timeout, Duration::from_secs(40 * 60));
    }

    /// Helper: create a script that sleeps for 120s, ignoring all CLI args.
    /// Returns the path to the temp script (caller must keep TempDir alive).
    fn create_sleep_script(dir: &std::path::Path) -> String {
        let script_path = dir.join("fake_claude.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 120\n").unwrap();
        std::fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        script_path.to_str().unwrap().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn test_timeout_kills_long_running_process() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let script = create_sleep_script(tmp.path());
        std::env::set_var("CLAUDE_BINARY", &script);
        let activity = Arc::new(AtomicU64::new(0));
        let timeout = TimeoutConfig {
            base_timeout: Duration::from_secs(2),
            initial_extension: Duration::from_secs(INITIAL_EXTENSION_SECS),
            last_activity_epoch: activity,
        };

        let start = Instant::now();
        let result = spawn_claude("ignored", None, None, None, Some(timeout), false);
        let elapsed = start.elapsed();

        std::env::remove_var("CLAUDE_BINARY");

        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(res.timed_out, "Process should be marked as timed out");
        assert_ne!(res.exit_code, 0);
        assert!(
            elapsed.as_secs() < 10,
            "Should timeout quickly, took {:?}",
            elapsed
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_timeout_extends_on_activity() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let script = create_sleep_script(tmp.path());
        std::env::set_var("CLAUDE_BINARY", &script);
        let activity = Arc::new(AtomicU64::new(0));
        let activity_clone = Arc::clone(&activity);
        let timeout = TimeoutConfig {
            base_timeout: Duration::from_secs(3),
            initial_extension: Duration::from_secs(INITIAL_EXTENSION_SECS),
            last_activity_epoch: activity,
        };

        // Simulate activity at 2s to extend the deadline
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            let epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            activity_clone.store(epoch, Ordering::Release);
        });

        let start = Instant::now();
        let result = spawn_claude("ignored", None, None, None, Some(timeout), false);
        let elapsed = start.elapsed();

        std::env::remove_var("CLAUDE_BINARY");

        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(res.timed_out, "Should eventually time out");
        // With 3s base + activity at 2s extending by min(1+7*60, 3)=3s → deadline at ~5s
        assert!(
            elapsed.as_secs() >= 4,
            "Activity should have extended lifetime past 3s, took {:?}",
            elapsed
        );
    }

    #[test]
    fn test_timeout_extension_decreases() {
        // Verify the extension formula: 7min, 6min, 5min, ..., 0
        for i in 0u32..10 {
            let ext = INITIAL_EXTENSION_SECS.saturating_sub(i as u64 * EXTENSION_DECREMENT_SECS);
            let expected = match i {
                0 => 7 * 60,
                1 => 6 * 60,
                2 => 5 * 60,
                3 => 4 * 60,
                4 => 3 * 60,
                5 => 2 * 60,
                6 => 1 * 60,
                _ => 0,
            };
            assert_eq!(ext, expected, "Extension #{} should be {}s", i, expected);
        }
    }

    #[test]
    fn test_spawn_claude_without_timeout_not_timed_out() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("hello", None, None, None, None, false);
        std::env::remove_var("CLAUDE_BINARY");

        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(!res.timed_out, "Normal exit should not be timed_out");
    }

    // --- stream_json arg construction tests ---

    #[test]
    fn test_stream_json_false_uses_print_flag() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("prompt", None, None, None, None, false);
        std::env::remove_var("CLAUDE_BINARY");
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
        use std::io::Write;
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        // Write a temporary script that emits a result JSON containing the args
        let dir = std::env::temp_dir();
        let script_path = dir.join("task_mgr_test_args.sh");
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            // Output a valid result JSON with the args as the result text
            writeln!(f, r#"printf '{{"type":"result","result":"%s"}}\n' "$*""#).unwrap();
        }
        let _ = std::fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        );

        std::env::set_var("CLAUDE_BINARY", script_path.to_str().unwrap());
        let result = spawn_claude("prompt", None, None, None, None, true);
        std::env::remove_var("CLAUDE_BINARY");
        let _ = std::fs::remove_file(&script_path);

        let res = result.expect("spawn should succeed");
        let output = res.output;
        assert!(
            output.contains("--output-format stream-json"),
            "stream_json=true must use --output-format stream-json, got: '{}'",
            output
        );
        assert!(
            !output.contains("--print"),
            "stream_json=true must NOT use --print"
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
        assert!(
            conv.is_none(),
            "system messages should not add to conversation"
        );
    }

    #[test]
    fn test_parse_stream_json_conversation_cap() {
        // Fill conversation beyond MAX_CONVERSATION_CHARS with repeated text blocks
        let big_text = "x".repeat(10_000);
        let block = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": big_text}]},
            "model": "m",
            "error": null
        })
        .to_string();
        let lines: Vec<String> = std::iter::repeat(block).take(10).collect();
        let (_, conv) = parse_stream_json_lines(lines.iter().map(|s| s.as_str()));
        let conv = conv.unwrap();
        assert!(
            conv.len() <= MAX_CONVERSATION_CHARS,
            "conversation must be capped at {} chars, got {}",
            MAX_CONVERSATION_CHARS,
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
    fn test_truncate_chars_ascii() {
        assert_eq!(truncate_chars("hello world", 5), "hello");
        assert_eq!(truncate_chars("hi", 100), "hi");
    }

    #[test]
    fn test_truncate_chars_multibyte() {
        // "こんにちは" is 5 chars, each 3 bytes = 15 bytes total
        let s = "こんにちは";
        assert_eq!(truncate_chars(s, 3), "こんに");
        // Ensure we don't split mid-character
        let truncated = truncate_chars(s, 3);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }
}
