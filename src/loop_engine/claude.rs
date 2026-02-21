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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::signals::SignalFlag;

/// Result of a Claude subprocess invocation.
#[derive(Debug)]
pub struct ClaudeResult {
    /// Process exit code (0 = success, non-zero = error/crash)
    pub exit_code: i32,
    /// Complete stdout output collected from the process
    pub output: String,
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
) -> TaskMgrResult<ClaudeResult> {
    let binary = std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string());
    let mut args: Vec<&str> = vec!["--print", "--dangerously-skip-permissions"];
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

    // Start watchdog thread if signal handling is requested
    let stop_watchdog = Arc::new(AtomicBool::new(false));
    let watchdog_handle = signal_flag.map(|flag| {
        let stop = Arc::clone(&stop_watchdog);
        let flag = flag.clone();
        std::thread::spawn(move || {
            watchdog_loop(child_pid, &flag, &stop);
        })
    });

    // Take ownership of stdout for line-by-line reading
    let stdout = child
        .stdout
        .take()
        .expect("stdout should be piped (Stdio::piped() was set on spawn)");

    let mut output = String::new();
    let reader = BufReader::new(stdout);

    for line_result in reader.lines() {
        match line_result {
            Ok(line) => {
                // Tee: echo to stderr (live display) and collect in buffer
                eprintln!("{}", line);
                output.push_str(&line);
                output.push('\n');
            }
            Err(e) => {
                eprintln!("Warning: error reading Claude stdout: {}", e);
                break;
            }
        }
    }

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

    Ok(ClaudeResult { exit_code, output })
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

/// Watchdog loop: polls signal flag and terminates child process on signal.
///
/// Runs on a dedicated OS thread (not tokio) so it works even when the
/// main thread is blocked in synchronous I/O.
///
/// Escalation: SIGTERM → 3s grace period → SIGKILL.
#[cfg(unix)]
fn watchdog_loop(child_pid: u32, signal_flag: &SignalFlag, stop: &AtomicBool) {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);
    const GRACE_PERIOD: Duration = Duration::from_secs(3);
    const GRACE_POLL: Duration = Duration::from_millis(100);

    // Kill the process group (negative PID) so that Claude AND all its child
    // processes are terminated. This prevents orphaned children from holding
    // the stdout pipe open and blocking the main thread in reader.lines().
    let pgid = -(child_pid as i32);

    // Poll until signaled or told to stop
    while !stop.load(Ordering::Acquire) {
        if signal_flag.is_signaled() {
            eprintln!(
                "\nSignal received, terminating Claude process group (pgid {})...",
                child_pid
            );

            // Send SIGTERM to the entire process group
            let ret = unsafe { libc::kill(pgid, libc::SIGTERM) };
            if ret == -1 {
                // ESRCH: process group already exited — nothing to do
                return;
            }

            // Wait up to 3s for graceful exit
            let start = std::time::Instant::now();
            while start.elapsed() < GRACE_PERIOD {
                std::thread::sleep(GRACE_POLL);
                if stop.load(Ordering::Acquire) {
                    // Main thread reaped the child
                    return;
                }
                // Check if process group leader still exists
                let ret = unsafe { libc::kill(pgid, 0) };
                if ret == -1 {
                    // Process group exited
                    return;
                }
            }

            // Grace period expired — force kill the entire group
            eprintln!(
                "Grace period expired, sending SIGKILL to process group {}...",
                child_pid
            );
            unsafe {
                libc::kill(pgid, libc::SIGKILL);
            }
            return;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(not(unix))]
fn watchdog_loop(_child_pid: u32, signal_flag: &SignalFlag, stop: &AtomicBool) {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    // On non-Unix, we can only poll — no SIGTERM/SIGKILL.
    // The child will be terminated when the parent exits (default behavior).
    while !stop.load(Ordering::Acquire) {
        if signal_flag.is_signaled() {
            eprintln!("\nSignal received, Claude subprocess will be terminated...");
            return;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- AC: ClaudeResult struct has expected fields ---

    #[test]
    fn test_claude_result_struct_fields() {
        let result = ClaudeResult {
            exit_code: 0,
            output: "Hello world\n".to_string(),
        };
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.output, "Hello world\n");
    }

    #[test]
    fn test_claude_result_with_non_zero_exit() {
        let result = ClaudeResult {
            exit_code: 137,
            output: String::new(),
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
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("hello", None, None, None);
        std::env::remove_var("CLAUDE_BINARY");
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.output.contains("hello"));
    }

    #[test]
    fn test_spawn_with_signal_flag_no_signal() {
        // spawn_claude with a SignalFlag that is NOT signaled should work normally
        std::env::set_var("CLAUDE_BINARY", "echo");
        let flag = SignalFlag::new();
        let result = spawn_claude("test output", Some(&flag), None, None);
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
        let result = spawn_claude("60", Some(&flag), None, None);
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
        std::env::set_var("CLAUDE_BINARY", "echo");
        let flag = SignalFlag::new();
        let result = spawn_claude("quick exit", Some(&flag), None, None);
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
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test_prompt", None, None, None);
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
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test_prompt", None, None, Some("claude-opus-4-6"));
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
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test_prompt", None, None, Some(""));
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
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test_prompt", None, None, Some("claude-opus-4-6"));
        std::env::remove_var("CLAUDE_BINARY");

        assert!(result.is_ok());
        let res = result.unwrap();
        let output = res.output.trim();

        let model_pos = output
            .find("--model")
            .expect("--model should be present in output");
        // Use " -p " to avoid matching the "-p" inside "--print"
        let prompt_pos = output.find(" -p ").expect("-p flag should be present in output");
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
        std::env::set_var("CLAUDE_BINARY", "echo");
        let result = spawn_claude("test_prompt", None, None, Some("claude-opus-4-6"));
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
}
