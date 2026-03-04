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
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::signals::SignalFlag;
use crate::loop_engine::watchdog::{exit_code_from_status, watchdog_loop, TimeoutConfig};

/// Maximum bytes for the formatted conversation in stream-json mode.
/// Byte-based for O(1) checking; limits are approximate and mostly-ASCII content means bytes ≈ chars.
const MAX_CONVERSATION_BYTES: usize = 50_000;
/// Maximum bytes for a single tool_use input block.
const MAX_TOOL_USE_BYTES: usize = 500;
/// Maximum bytes for a single tool_result content block.
const MAX_TOOL_RESULT_BYTES: usize = 1_000;

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
            "--verbose",
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
///   `MAX_CONVERSATION_BYTES`.
///
/// Each JSON line is parsed exactly once; the parsed `Value` is passed to both
/// `tee_assistant_text` (for live display) and `process_stream_json_values` (for
/// conversation building).
fn tee_stream_json(reader: BufReader<impl std::io::Read>) -> (String, Option<String>) {
    let mut parsed: Vec<serde_json::Value> = Vec::new();

    for line_result in reader.lines() {
        match line_result {
            Ok(line) => {
                match serde_json::from_str::<serde_json::Value>(&line) {
                    Ok(val) => {
                        // Tee assistant text live before collecting for conversation building
                        tee_assistant_text(&val);
                        parsed.push(val);
                    }
                    Err(_) => {
                        eprintln!("Warning: malformed stream-json line (not valid JSON)");
                    }
                }
            }
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
    if let Some(s) = error.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
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
///   user messages (tool_result), capped at `MAX_CONVERSATION_BYTES`.
///
/// Malformed JSON lines and unknown message types are silently skipped with a warning.
#[cfg(test)]
pub(crate) fn parse_stream_json_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> (String, Option<String>) {
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

/// Process an iterator of already-parsed stream-json `Value`s into (output_text, conversation).
///
/// This is the shared core used by both `parse_stream_json_lines` (which parses strings first)
/// and `tee_stream_json` (which passes pre-parsed values to avoid double-parsing).
fn process_stream_json_values(
    values: impl Iterator<Item = serde_json::Value>,
) -> (String, Option<String>) {
    let mut output_text = String::new();
    let mut conversation = String::new();

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
            }
            // Skip system/init and unknown types
            _ => {}
        }
    }

    (output_text, Some(conversation))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::watchdog::{exit_code_from_status, TimeoutConfig};
    use rstest::rstest;
    use std::sync::atomic::AtomicU64;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    // Serialize tests that mutate CLAUDE_BINARY to avoid race conditions
    // when cargo test runs threads in parallel.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Test helper: run spawn_claude with CLAUDE_BINARY=echo under ENV_MUTEX.
    fn spawn_claude_echo(
        prompt: &str,
        signal: Option<&SignalFlag>,
        model: Option<&str>,
        stream_json: bool,
    ) -> TaskMgrResult<ClaudeResult> {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude(prompt, signal, None, model, None, stream_json);
        std::env::remove_var("CLAUDE_BINARY");
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
        let result = spawn_claude_echo("hello", None, None, false);
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.output.contains("hello"));
    }

    #[test]
    fn test_spawn_with_signal_flag_no_signal() {
        // spawn_claude with a SignalFlag that is NOT signaled should work normally
        let flag = SignalFlag::new();
        let result = spawn_claude_echo("test output", Some(&flag), None, false);
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
        let flag = SignalFlag::new();
        let result = spawn_claude_echo("quick exit", Some(&flag), None, false);

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
        let result = spawn_claude_echo("test_prompt", None, None, false);

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
        let result = spawn_claude_echo("test_prompt", None, Some("claude-opus-4-6"), false);

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
        let result = spawn_claude_echo("test_prompt", None, Some(""), false);

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
        let result = spawn_claude_echo("test_prompt", None, Some("claude-opus-4-6"), false);

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

    /// --print and --dangerously-skip-permissions must be present regardless of model value.
    #[test]
    fn test_spawn_model_some_preserves_required_flags() {
        let result = spawn_claude_echo("test_prompt", None, Some("claude-opus-4-6"), false);

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
        let result = spawn_claude_echo("test prompt", None, Some(model), false);

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
        let result = spawn_claude_echo("test prompt", None, Some(model), false);

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
        let result = spawn_claude_echo("my prompt", None, model, false);

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
        let result = spawn_claude_echo("my prompt text", None, None, false);

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
        let result = spawn_claude_echo("test prompt", None, Some(model), false);

        let res = result.expect("echo should succeed");
        let output = res.output.trim();

        assert!(
            !output.contains("--model"),
            "Whitespace-only model '{}' should be treated as None, got: '{}'",
            model.escape_debug(),
            output
        );
    }

    /// Helper: create a script that emits a stream-json result line containing all CLI args.
    /// The script prints `{"type":"result","result":"<args>"}` so the stream-json parser
    /// returns the args as the output text.  `name` is used to make the filename unique.
    /// Returns the absolute path to the created script.
    fn make_stream_json_result_script(name: &str) -> std::path::PathBuf {
        use std::io::Write;
        let script_path = std::env::temp_dir().join(format!("task_mgr_test_{name}.sh"));
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, r#"printf '{{"type":"result","result":"%s"}}\n' "$*""#).unwrap();
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
        let result = spawn_claude_echo("hello", None, None, false);

        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(!res.timed_out, "Normal exit should not be timed_out");
    }

    // --- stream_json arg construction tests ---

    #[test]
    fn test_stream_json_false_uses_print_flag() {
        let result = spawn_claude_echo("prompt", None, None, false);
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
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let script_path = make_stream_json_result_script("args");
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
        let lines: Vec<String> = std::iter::repeat(block).take(10).collect();
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
        let lines: Vec<String> = std::iter::repeat(block).take(6).collect();
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
        let result = spawn_claude_echo("prompt", None, Some("claude-opus-4-6"), false);
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
            output.contains("claude-opus-4-6"),
            "stream_json=false with model must include the model value"
        );
        assert!(
            !output.contains("--output-format"),
            "stream_json=false must NOT use --output-format"
        );
    }

    /// AC: stream_json=true with model + timeout — correct arg ordering.
    ///
    /// Expected order: --output-format stream-json, --no-session-persistence,
    /// --dangerously-skip-permissions, --model <model>, -p <prompt>
    #[test]
    fn test_stream_json_true_with_model_and_timeout_arg_ordering() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let script_path = make_stream_json_result_script("stream_model_timeout");
        let timeout = TimeoutConfig::from_difficulty(Some("medium"), Arc::new(AtomicU64::new(0)));
        std::env::set_var("CLAUDE_BINARY", script_path.to_str().unwrap());
        let result = spawn_claude(
            "my-prompt",
            None,
            None,
            Some("claude-sonnet-4-6"),
            Some(timeout),
            true,
        );
        std::env::remove_var("CLAUDE_BINARY");
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
        assert!(
            output.contains("claude-sonnet-4-6"),
            "model value must be in args"
        );

        // --no-session-persistence present
        assert!(
            output.contains("--no-session-persistence"),
            "--no-session-persistence must be present"
        );

        // --print must NOT be present
        assert!(
            !output.contains("--print"),
            "--print must NOT be present for stream_json=true"
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
    #[case(true, None, false, true)] // engine no model: --output-format, no --print
    #[case(true, Some("sonnet"), false, true)] // engine+model: --output-format, no --print, has --model
    fn test_spawn_claude_four_caller_patterns(
        #[case] stream_json: bool,
        #[case] model: Option<&str>,
        #[case] expect_print: bool,
        #[case] expect_output_format: bool,
    ) {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let output = if stream_json {
            // Need a script that emits valid result JSON so the stream-json parser yields args
            let script_path =
                make_stream_json_result_script(&format!("4callers_{}", model.unwrap_or("none")));
            std::env::set_var("CLAUDE_BINARY", script_path.to_str().unwrap());
            let result = spawn_claude("test-prompt", None, None, model, None, stream_json);
            std::env::remove_var("CLAUDE_BINARY");
            let _ = std::fs::remove_file(&script_path);
            result.expect("spawn should succeed").output
        } else {
            // Call spawn_claude directly — ENV_MUTEX is already held by this function.
            // Using spawn_claude_echo here would deadlock (std::sync::Mutex is not reentrant).
            std::env::set_var("CLAUDE_BINARY", "echo");
            let result = spawn_claude("test-prompt", None, None, model, None, stream_json);
            std::env::remove_var("CLAUDE_BINARY");
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
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Locate the fixture relative to CARGO_MANIFEST_DIR
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let script = format!("{}/tests/fixtures/mock_stream_json.sh", manifest_dir);
        std::env::set_var("CLAUDE_BINARY", &script);
        let result = spawn_claude("ignored_prompt", None, None, None, None, stream_json);
        std::env::remove_var("CLAUDE_BINARY");
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
}
