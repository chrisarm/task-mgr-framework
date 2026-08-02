//! Auto-launch of `/review-loop` after a successful loop or batch run.
//!
//! # Design notes
//!
//! **Env-var inheritance is intentional.** [`ProcessLauncher`] builds a
//! [`std::process::Command`] without calling `.env_clear()`, so `ANTHROPIC_API_KEY`,
//! Grok credentials, and other ambient variables the user has set are inherited
//! by the spawned process. Stripping the environment would silently break auth.
//!
//! **Dual launch modes.** Interactive mode (TTY parent) inherits stdin/stdout/
//! stderr and blocks so the operator lands in a live session. Headless mode
//! (non-TTY parent — babysit / redirected logs) detaches a single-turn review
//! that writes to a log file and a findings path; it never auto-chains
//! `/compound`. See [`LaunchMode`] and [`resolve_launch_mode`].
//!
//! **Provider host.** The launcher follows `models.primaryProvider` (claude or
//! grok). Codex and unknown providers skip launch with a recovery hint.
//! Grok loads global Claude skills (`~/.claude/commands/`), so interactive
//! `/review-loop` works for both hosts without a separate Grok skill stage.
//!
//! **Worktree-suppression rationale.** When `LoopResult::worktree_path` is `None`
//! (or the path no longer exists on disk), `maybe_fire` prints a hint and returns
//! without launching. It does NOT fall back to `project_root`. Running `/review-loop`
//! from the main worktree would check out the feature branch there, which is
//! push-protected in most CI setups and risks dirty-state collisions with other
//! in-flight loops.

use std::fs;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::loop_engine::engine::LoopResult;
use crate::loop_engine::model::{Provider, parse_config_provider};
use crate::loop_engine::project_config::{AutoReviewMode, ProjectConfig};

// ---------------------------------------------------------------------------
// Decision struct and resolution
// ---------------------------------------------------------------------------

/// Resolved auto-review policy for a single loop/batch run.
#[derive(Debug, Clone, Copy)]
pub struct Decision {
    /// Whether auto-review is enabled for this run.
    pub enabled: bool,
    /// Minimum number of tasks that must have been completed for the review to fire.
    pub min_tasks: u32,
}

/// Resolve the final auto-review [`Decision`] from config + CLI overrides.
///
/// Priority (highest to lowest):
/// 1. `cli_force_off` — disables unconditionally, sets `min_tasks = u32::MAX`
/// 2. `cli_force_on`  — enables with `min_tasks = 1`
/// 3. Project config (`auto_review` / `auto_review_min_tasks`)
pub fn resolve_decision(
    config: &ProjectConfig,
    cli_force_on: bool,
    cli_force_off: bool,
) -> Decision {
    if cli_force_off {
        return Decision {
            enabled: false,
            min_tasks: u32::MAX,
        };
    }
    if cli_force_on {
        return Decision {
            enabled: true,
            min_tasks: 1,
        };
    }
    Decision {
        enabled: config.auto_review,
        min_tasks: config.auto_review_min_tasks,
    }
}

// ---------------------------------------------------------------------------
// Gate logic
// ---------------------------------------------------------------------------

/// Returns `true` when all conditions are met and the review should fire.
///
/// All four conditions must hold:
/// - `d.enabled`
/// - `exit_code == 0` (clean exit)
/// - `!was_stopped` (not a mid-run stop signal)
/// - `tasks_completed >= d.min_tasks`
pub fn should_fire(d: &Decision, exit_code: i32, was_stopped: bool, tasks_completed: u32) -> bool {
    d.enabled && exit_code == 0 && !was_stopped && tasks_completed >= d.min_tasks
}

// ---------------------------------------------------------------------------
// Review host + launch mode
// ---------------------------------------------------------------------------

/// Interactive CLI host that can run `/review-loop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewHost {
    Claude,
    Grok,
}

impl ReviewHost {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewHost::Claude => "claude",
            ReviewHost::Grok => "grok",
        }
    }

    /// Binary path: `$CLAUDE_BINARY` / `$GROK_BINARY` else bare name on PATH.
    pub fn resolve_binary(self) -> String {
        match self {
            ReviewHost::Claude => std::env::var("CLAUDE_BINARY")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "claude".to_string()),
            ReviewHost::Grok => std::env::var("GROK_BINARY")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "grok".to_string()),
        }
    }
}

/// How the review process is launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Inherit stdio, block until the interactive session ends.
    Interactive,
    /// Single-turn headless run; stdout/stderr to a log file; detach (non-blocking).
    Headless,
}

/// Resolve the interactive review host from `models.primaryProvider`.
///
/// Returns `None` for codex / unparseable providers (auto-review only supports
/// interactive claude/grok hosts).
pub fn resolve_review_host(config: &ProjectConfig) -> Option<ReviewHost> {
    match parse_config_provider(&config.models.primary_provider) {
        Ok(Provider::Claude) => Some(ReviewHost::Claude),
        Ok(Provider::Grok) => Some(ReviewHost::Grok),
        Ok(Provider::Codex) | Err(_) => None,
    }
}

/// Resolve launch mode from config + whether the parent has a TTY.
///
/// Returns `None` when the configured mode refuses to launch (e.g.
/// `interactive` config with a non-TTY parent).
pub fn resolve_launch_mode(mode: AutoReviewMode, is_tty: bool) -> Option<LaunchMode> {
    match mode {
        AutoReviewMode::Off => None,
        AutoReviewMode::Interactive => {
            if is_tty {
                Some(LaunchMode::Interactive)
            } else {
                None
            }
        }
        AutoReviewMode::Headless => Some(LaunchMode::Headless),
        AutoReviewMode::Auto => {
            if is_tty {
                Some(LaunchMode::Interactive)
            } else {
                Some(LaunchMode::Headless)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PRD markdown path resolution
// ---------------------------------------------------------------------------

/// Resolve the PRD markdown path from a PRD JSON path.
///
/// Tries, in order (first existing path wins):
/// 1. JSON `prdFile` field (relative to the JSON's parent directory)
/// 2. `{stem}.md` (same stem, extension swapped)
/// 3. `prd-{stem}.md` (prefixed form in the same directory)
/// 4. `{stem}-prompt.md` (plan-tasks legacy fallback)
///
/// Returns `None` when none exist on disk.
///
/// **Trust boundary**: `prd_json` is an operator-controlled CLI argument (the
/// PRD JSON path passed to `task-mgr loop`). No path-escape detection is
/// performed — the caller is trusted. Do not forward user-supplied, untrusted
/// paths here without validation.
pub fn prd_md_path(prd_json: &Path) -> Option<PathBuf> {
    let parent = prd_json.parent().unwrap_or_else(|| Path::new("."));

    // 1. prdFile from JSON (best-effort)
    if let Ok(content) = fs::read_to_string(prd_json)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(prd_file) = json.get("prdFile").and_then(|v| v.as_str())
    {
        let candidate = if Path::new(prd_file).is_absolute() {
            PathBuf::from(prd_file)
        } else {
            parent.join(prd_file)
        };
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 2. bare {stem}.md
    let bare = prd_json.with_extension("md");
    if bare.exists() {
        return Some(bare);
    }

    let stem = prd_json.file_stem()?.to_str()?;

    // 3. prd-{stem}.md
    let prefixed = parent.join(format!("prd-{stem}.md"));
    if prefixed.exists() {
        return Some(prefixed);
    }

    // 4. {stem}-prompt.md (plan-tasks legacy)
    let prompt = parent.join(format!("{stem}-prompt.md"));
    if prompt.exists() {
        return Some(prompt);
    }

    None
}

// ---------------------------------------------------------------------------
// Pending-review receipt
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingReviewReceipt {
    prd_json: String,
    prd_md: String,
    worktree: String,
    host: String,
    mode: String,
    interactive_command: String,
    log_path: Option<String>,
    findings_path: Option<String>,
    created_at: String,
    status: String,
}

fn interactive_command(host: ReviewHost, md: &Path, worktree: &Path) -> String {
    format!(
        "cd {} && {} \"/review-loop {}\"",
        worktree.display(),
        host.as_str(),
        md.display()
    )
}

struct ReceiptInput<'a> {
    worktree: &'a Path,
    prd_json: &'a Path,
    md: &'a Path,
    host: ReviewHost,
    mode: Option<LaunchMode>,
    log_path: Option<&'a Path>,
    findings_path: Option<&'a Path>,
    status: &'a str,
}

fn write_pending_receipt(input: ReceiptInput<'_>) {
    let dir = input.worktree.join(".task-mgr").join("pending-reviews");
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!(
            "[auto-review] could not create pending-reviews dir ({}): {e}",
            dir.display()
        );
        return;
    }
    let stem = input
        .prd_json
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("prd");
    let path = dir.join(format!("{stem}.json"));
    let mode_str = match input.mode {
        Some(LaunchMode::Interactive) => "interactive",
        Some(LaunchMode::Headless) => "headless",
        None => "none",
    };
    let receipt = PendingReviewReceipt {
        prd_json: input.prd_json.display().to_string(),
        prd_md: input.md.display().to_string(),
        worktree: input.worktree.display().to_string(),
        host: input.host.as_str().to_string(),
        mode: mode_str.to_string(),
        interactive_command: interactive_command(input.host, input.md, input.worktree),
        log_path: input.log_path.map(|p| p.display().to_string()),
        findings_path: input.findings_path.map(|p| p.display().to_string()),
        created_at: chrono_like_now(),
        status: input.status.to_string(),
    };
    match serde_json::to_string_pretty(&receipt) {
        Ok(body) => {
            if let Err(e) = fs::write(&path, body) {
                eprintln!(
                    "[auto-review] could not write receipt {}: {e}",
                    path.display()
                );
            }
        }
        Err(e) => {
            eprintln!("[auto-review] could not serialize receipt: {e}");
        }
    }
}

fn chrono_like_now() -> String {
    // Avoid pulling chrono just for a receipt timestamp — RFC3339-ish from SystemTime.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

fn headless_log_path(worktree: &Path, stem: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    worktree
        .join(".task-mgr")
        .join("logs")
        .join(format!("auto-review-{stem}-{ts}.log"))
}

fn headless_findings_path(worktree: &Path, stem: &str) -> PathBuf {
    worktree
        .join(".task-mgr")
        .join("reviews")
        .join(format!("{stem}.md"))
}

fn slim_headless_prompt(md: &Path, worktree: &Path, findings: &Path) -> String {
    format!(
        r#"You are running a HEADLESS post-loop code review (non-interactive; no clarifying questions).

PRD / review brief: {md}
Worktree (cd here; all edits and git ops must stay in this worktree): {worktree}
Write findings to: {findings}

Instructions (adapted from /review-loop; do NOT auto-chain /compound):
1. Read the PRD/brief at the path above to understand what was supposed to be built.
2. Derive the task JSON path (same basename, .json) if useful for branchName only — prefer `jq -r .branchName` over loading the full JSON.
3. Confirm you are in the worktree. Review the branch vs main (`git log --oneline main..HEAD`, `git diff main...HEAD`) and any uncommitted changes.
4. Assess correctness, security, and PRD coherence. Prefer a code-review subagent when available (rust-python-code-reviewer / standard-code-reviewer).
5. Write a structured findings report to the findings path above:
   - Summary (2–4 sentences)
   - Critical / High / Medium / Low findings with file:line when possible
   - Explicit "CLEAN" or "NEEDS WORK" verdict
6. Do NOT spawn fixup tasks unless findings are Critical and the fix is trivial and safe; prefer documenting them for a human.
7. Do NOT run /compound. Do NOT ask the user questions. Exit when the findings file is written.

If the worktree path is wrong or the branch has no commits beyond main, write that fact to the findings file and stop.
"#,
        md = md.display(),
        worktree = worktree.display(),
        findings = findings.display(),
    )
}

// ---------------------------------------------------------------------------
// Launcher abstraction
// ---------------------------------------------------------------------------

/// Abstraction for launching the `/review-loop` session.
///
/// Production code uses [`ProcessLauncher`]; tests use [`CapturingLauncher`].
pub trait ReviewLauncher {
    fn launch(
        &self,
        md: &Path,
        worktree: Option<&Path>,
        mode: LaunchMode,
        host: ReviewHost,
        log_path: Option<&Path>,
        findings_path: Option<&Path>,
    ) -> std::io::Result<()>;
}

/// Production launcher — spawns claude or grok (interactive or headless).
#[derive(Debug, Default)]
pub struct ProcessLauncher;

impl ReviewLauncher for ProcessLauncher {
    fn launch(
        &self,
        md: &Path,
        worktree: Option<&Path>,
        mode: LaunchMode,
        host: ReviewHost,
        log_path: Option<&Path>,
        findings_path: Option<&Path>,
    ) -> std::io::Result<()> {
        let binary = host.resolve_binary();
        match mode {
            LaunchMode::Interactive => launch_interactive(&binary, host, md, worktree),
            LaunchMode::Headless => {
                let wt = worktree.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "headless auto-review requires a worktree",
                    )
                })?;
                let log = log_path.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "headless auto-review requires a log path",
                    )
                })?;
                let findings = findings_path.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "headless auto-review requires a findings path",
                    )
                })?;
                launch_headless(&binary, host, md, wt, log, findings)
            }
        }
    }
}

fn launch_interactive(
    binary: &str,
    host: ReviewHost,
    md: &Path,
    worktree: Option<&Path>,
) -> std::io::Result<()> {
    let prompt_arg = format!("/review-loop {}", md.display());
    let mut cmd = Command::new(binary);
    // Set cwd first so both hosts resolve relative paths from the worktree.
    // Grok also accepts --cwd; pass it before the positional prompt so clap
    // does not treat the prompt as a flag value.
    if let Some(dir) = worktree {
        cmd.current_dir(dir);
        if host == ReviewHost::Grok {
            cmd.arg("--cwd").arg(dir);
        }
    }
    cmd.arg(&prompt_arg);

    match cmd.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => {
            eprintln!(
                "[auto-review] {host} exited with status {status}; \
                 re-run `{cmd}` manually if needed",
                host = host.as_str(),
                cmd = interactive_command(
                    host,
                    md,
                    worktree.unwrap_or_else(|| Path::new("."))
                ),
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn launch_headless(
    binary: &str,
    host: ReviewHost,
    md: &Path,
    worktree: &Path,
    log_path: &Path,
    findings_path: &Path,
) -> std::io::Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = findings_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let prompt = slim_headless_prompt(md, worktree, findings_path);
    let prompt_file = worktree.join(".task-mgr").join(format!(
        "auto-review-prompt-{}.md",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    if let Some(parent) = prompt_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&prompt_file, &prompt)?;

    let log = fs::File::create(log_path)?;
    let log_err = log.try_clone()?;

    let mut cmd = Command::new(binary);
    match host {
        ReviewHost::Claude => {
            cmd.arg("--print")
                .arg("--dangerously-skip-permissions")
                .arg("--no-session-persistence")
                .arg("--output-format")
                .arg("text")
                .arg("-p")
                .arg(&prompt);
        }
        ReviewHost::Grok => {
            cmd.arg("--cwd")
                .arg(worktree)
                .arg("--always-approve")
                .arg("--prompt-file")
                .arg(&prompt_file)
                .arg("--output-format")
                .arg("text");
        }
    }
    cmd.current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    // Detach from the parent process group on Unix so a babysitter exit /
    // SIGHUP does not kill the review mid-flight.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                // setsid() fails if already a session leader; ignore error.
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd.spawn()?;
    eprintln!(
        "[auto-review] headless {host} review launched (pid {}); log: {}; findings: {}",
        child.id(),
        log_path.display(),
        findings_path.display(),
        host = host.as_str(),
    );
    // Detach: do not wait. Dropping Child does not kill on Unix once setsid'd.
    std::mem::forget(child);
    Ok(())
}

/// Test-only launcher that records calls instead of spawning processes.
#[cfg(test)]
pub(crate) struct CapturingLauncher {
    pub calls: std::sync::Mutex<
        Vec<(
            PathBuf,
            Option<PathBuf>,
            LaunchMode,
            ReviewHost,
            Option<PathBuf>,
            Option<PathBuf>,
        )>,
    >,
}

#[cfg(test)]
impl CapturingLauncher {
    pub fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl ReviewLauncher for CapturingLauncher {
    fn launch(
        &self,
        md: &Path,
        worktree: Option<&Path>,
        mode: LaunchMode,
        host: ReviewHost,
        log_path: Option<&Path>,
        findings_path: Option<&Path>,
    ) -> std::io::Result<()> {
        self.calls.lock().unwrap().push((
            md.to_path_buf(),
            worktree.map(Path::to_path_buf),
            mode,
            host,
            log_path.map(Path::to_path_buf),
            findings_path.map(Path::to_path_buf),
        ));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Maybe fire the auto-review launcher after a loop run.
///
/// Detects whether stdout is a TTY, then delegates to [`maybe_fire_with`].
/// Launcher errors are logged but never propagated — a review launch failure
/// must never change the loop's exit code.
pub fn maybe_fire(
    config: &ProjectConfig,
    cli_force_on: bool,
    cli_force_off: bool,
    result: &LoopResult,
    prd_json: &Path,
    launcher: &dyn ReviewLauncher,
) {
    let is_tty = std::io::stdout().is_terminal();
    maybe_fire_with(
        config,
        cli_force_on,
        cli_force_off,
        result,
        prd_json,
        launcher,
        is_tty,
    );
}

/// Gate logic for auto-review with an injected TTY flag (test seam).
///
/// Production callers must go through [`maybe_fire`] (which probes the real
/// TTY). This function is `pub(crate)` so unit tests can exercise interactive
/// vs headless branches under `cargo test` (non-TTY env).
pub(crate) fn maybe_fire_with(
    config: &ProjectConfig,
    cli_force_on: bool,
    cli_force_off: bool,
    result: &LoopResult,
    prd_json: &Path,
    launcher: &dyn ReviewLauncher,
    is_tty: bool,
) {
    let decision = resolve_decision(config, cli_force_on, cli_force_off);

    if !should_fire(
        &decision,
        result.exit_code,
        result.was_stopped,
        result.tasks_completed,
    ) {
        return;
    }

    // autoReviewMode: "off" is an extra suppress even when autoReview is true
    // and CLI force-on is set? Plan says off ≡ disabled. CLI force-on should
    // still enable decision but mode=off means no launch. Force-on with mode
    // off is rare; respect mode.
    if config.auto_review_mode == AutoReviewMode::Off && !cli_force_on {
        return;
    }

    let worktree = result.worktree_path.as_deref();
    match worktree {
        None => {
            eprintln!(
                "[auto-review] no worktree path available; \
                 run `/review-loop` manually in your feature worktree \
                 (PRD JSON: {})",
                prd_json.display()
            );
            return;
        }
        Some(wt) if !wt.exists() => {
            eprintln!(
                "[auto-review] worktree `{wt}` does not exist; \
                 run `/review-loop` manually in your feature worktree \
                 (PRD JSON: {path})",
                wt = wt.display(),
                path = prd_json.display()
            );
            return;
        }
        _ => {}
    }
    let worktree = worktree.expect("worktree Some after match");

    let md = match prd_md_path(prd_json) {
        Some(p) => p,
        None => {
            eprintln!(
                "[auto-review] could not find a markdown PRD for `{}`; \
                 tried prdFile field, {{stem}}.md, prd-{{stem}}.md, {{stem}}-prompt.md — \
                 run `/review-loop` manually after adding a brief",
                prd_json.display()
            );
            return;
        }
    };

    if md.to_string_lossy().chars().any(char::is_whitespace) {
        eprintln!(
            "[auto-review] PRD path `{}` contains whitespace; slash-command \
             parsers fragment it. Rename the file to remove spaces/tabs, then \
             re-run `/review-loop` manually.",
            md.display()
        );
        return;
    }

    let host = match resolve_review_host(config) {
        Some(h) => h,
        None => {
            eprintln!(
                "[auto-review] models.primaryProvider={:?} is not an interactive \
                 review host (need claude or grok); run `/review-loop {}` manually \
                 with an enabled host",
                config.models.primary_provider,
                md.display()
            );
            return;
        }
    };

    // When CLI force-on is set, treat mode as Auto if it was Off so force
    // still launches.
    let mode_cfg = if cli_force_on && config.auto_review_mode == AutoReviewMode::Off {
        AutoReviewMode::Auto
    } else {
        config.auto_review_mode
    };

    let launch_mode = resolve_launch_mode(mode_cfg, is_tty);

    let stem = prd_json
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("prd");

    let (log_path, findings_path) = match launch_mode {
        Some(LaunchMode::Headless) => (
            Some(headless_log_path(worktree, stem)),
            Some(headless_findings_path(worktree, stem)),
        ),
        _ => (None, None),
    };

    let status = match launch_mode {
        Some(LaunchMode::Interactive) => "waiting_interactive",
        Some(LaunchMode::Headless) => "launched",
        None => "waiting_interactive", // receipt only
    };

    write_pending_receipt(ReceiptInput {
        worktree,
        prd_json,
        md: &md,
        host,
        mode: launch_mode,
        log_path: log_path.as_deref(),
        findings_path: findings_path.as_deref(),
        status,
    });

    let Some(mode) = launch_mode else {
        eprintln!(
            "[auto-review] stdout is not a TTY and autoReviewMode=interactive; \
             review not launched. Re-run:\n  {}",
            interactive_command(host, &md, worktree)
        );
        return;
    };

    if mode == LaunchMode::Headless {
        eprintln!(
            "[auto-review] non-TTY parent → headless review via {}; \
             interactive re-run: {}",
            host.as_str(),
            interactive_command(host, &md, worktree)
        );
    }

    if let Err(e) = launcher.launch(
        &md,
        Some(worktree),
        mode,
        host,
        log_path.as_deref(),
        findings_path.as_deref(),
    ) {
        eprintln!(
            "[auto-review] failed to launch {} ({}); \
             re-run:\n  {}",
            host.as_str(),
            e,
            interactive_command(host, &md, worktree)
        );
        write_pending_receipt(ReceiptInput {
            worktree,
            prd_json,
            md: &md,
            host,
            mode: Some(mode),
            log_path: log_path.as_deref(),
            findings_path: findings_path.as_deref(),
            status: "failed_to_spawn",
        });
    }
}

/// Back-compat test seam name used throughout existing unit tests.
///
/// Calls [`maybe_fire_with`] with `is_tty = true` so inner-gate tests still
/// exercise interactive launch under `cargo test` (which is non-TTY).
#[cfg(test)]
pub(crate) fn maybe_fire_inner(
    config: &ProjectConfig,
    cli_force_on: bool,
    cli_force_off: bool,
    result: &LoopResult,
    prd_json: &Path,
    launcher: &dyn ReviewLauncher,
) {
    maybe_fire_with(
        config,
        cli_force_on,
        cli_force_off,
        result,
        prd_json,
        launcher,
        true, // pretend TTY so interactive path is reachable in tests
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn default_config() -> ProjectConfig {
        ProjectConfig::default()
    }

    fn grok_primary_config() -> ProjectConfig {
        let mut config = default_config();
        config.models.primary_provider = "grok".to_string();
        config
    }

    // --- resolve_decision ---

    #[test]
    fn resolve_cli_off_wins() {
        let d = resolve_decision(&default_config(), false, true);
        assert!(!d.enabled);
        assert_eq!(d.min_tasks, u32::MAX);
    }

    #[test]
    fn resolve_cli_on_overrides_config_false() {
        let mut config = default_config();
        config.auto_review = false;
        let d = resolve_decision(&config, true, false);
        assert!(d.enabled);
        assert_eq!(d.min_tasks, 1);
    }

    #[test]
    fn resolve_defaults_from_config() {
        // default config has auto_review=true, auto_review_min_tasks=3
        let d = resolve_decision(&default_config(), false, false);
        assert!(d.enabled);
        assert_eq!(d.min_tasks, 3);
    }

    #[test]
    fn resolve_config_disabled() {
        let mut config = default_config();
        config.auto_review = false;
        let d = resolve_decision(&config, false, false);
        assert!(!d.enabled);
    }

    #[test]
    fn resolve_cli_off_beats_cli_on() {
        // Both flags set — cli_force_off wins (clap prevents this at parse time,
        // but resolve_decision handles it defensively).
        let d = resolve_decision(&default_config(), true, true);
        assert!(!d.enabled);
        assert_eq!(d.min_tasks, u32::MAX);
    }

    // --- should_fire ---

    fn enabled_decision(min_tasks: u32) -> Decision {
        Decision {
            enabled: true,
            min_tasks,
        }
    }

    fn disabled_decision() -> Decision {
        Decision {
            enabled: false,
            min_tasks: 3,
        }
    }

    #[test]
    fn should_fire_all_clear() {
        assert!(should_fire(&enabled_decision(3), 0, false, 3));
    }

    #[test]
    fn should_fire_blocked_by_nonzero_exit() {
        assert!(!should_fire(&enabled_decision(3), 1, false, 5));
    }

    #[test]
    fn should_fire_blocked_by_was_stopped() {
        assert!(!should_fire(&enabled_decision(3), 0, true, 5));
    }

    #[test]
    fn should_fire_blocked_by_threshold() {
        assert!(!should_fire(&enabled_decision(3), 0, false, 2));
    }

    #[test]
    fn should_fire_blocked_by_disabled() {
        assert!(!should_fire(&disabled_decision(), 0, false, 5));
    }

    #[test]
    fn should_fire_boundary_equal_to_min() {
        // tasks_completed == min_tasks should fire (>= not >)
        assert!(should_fire(&enabled_decision(3), 0, false, 3));
    }

    #[test]
    fn should_fire_zero_threshold_fires_when_other_gates_pass() {
        // min_tasks=0 means the threshold is no barrier at all.
        assert!(should_fire(&enabled_decision(0), 0, false, 0));
        assert!(should_fire(&enabled_decision(0), 0, false, 1));
        // Other gates still block independently.
        assert!(!should_fire(&enabled_decision(0), 1, false, 5)); // non-zero exit
        assert!(!should_fire(&enabled_decision(0), 0, true, 5)); // was_stopped
    }

    // --- resolve_launch_mode ---

    #[test]
    fn launch_mode_auto_tty_is_interactive() {
        assert_eq!(
            resolve_launch_mode(AutoReviewMode::Auto, true),
            Some(LaunchMode::Interactive)
        );
    }

    #[test]
    fn launch_mode_auto_non_tty_is_headless() {
        assert_eq!(
            resolve_launch_mode(AutoReviewMode::Auto, false),
            Some(LaunchMode::Headless)
        );
    }

    #[test]
    fn launch_mode_interactive_non_tty_is_none() {
        assert_eq!(resolve_launch_mode(AutoReviewMode::Interactive, false), None);
    }

    #[test]
    fn launch_mode_headless_always_headless() {
        assert_eq!(
            resolve_launch_mode(AutoReviewMode::Headless, true),
            Some(LaunchMode::Headless)
        );
        assert_eq!(
            resolve_launch_mode(AutoReviewMode::Headless, false),
            Some(LaunchMode::Headless)
        );
    }

    #[test]
    fn launch_mode_off_is_none() {
        assert_eq!(resolve_launch_mode(AutoReviewMode::Off, true), None);
    }

    // --- resolve_review_host ---

    #[test]
    fn review_host_defaults_to_claude() {
        assert_eq!(
            resolve_review_host(&default_config()),
            Some(ReviewHost::Claude)
        );
    }

    #[test]
    fn review_host_follows_primary_grok() {
        assert_eq!(
            resolve_review_host(&grok_primary_config()),
            Some(ReviewHost::Grok)
        );
    }

    #[test]
    fn review_host_codex_is_none() {
        let mut config = default_config();
        config.models.primary_provider = "codex".to_string();
        assert_eq!(resolve_review_host(&config), None);
    }

    // --- prd_md_path ---

    #[test]
    fn prd_md_path_bare_exists() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        assert_eq!(prd_md_path(&json), Some(md));
    }

    #[test]
    fn prd_md_path_prefixed_exists() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("prd-foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        assert_eq!(prd_md_path(&json), Some(md));
    }

    #[test]
    fn prd_md_path_prd_file_field() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("custom-brief.md");
        fs::write(&md, "# brief").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(
            &json,
            r#"{"prdFile":"custom-brief.md","userStories":[]}"#,
        )
        .unwrap();
        assert_eq!(prd_md_path(&json), Some(md));
    }

    #[test]
    fn prd_md_path_prompt_fallback() {
        let tmp = TempDir::new().unwrap();
        let prompt = tmp.path().join("foo-prompt.md");
        fs::write(&prompt, "# prompt").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, r#"{"userStories":[]}"#).unwrap();
        assert_eq!(prd_md_path(&json), Some(prompt));
    }

    #[test]
    fn prd_md_path_neither_exists() {
        let tmp = TempDir::new().unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, r#"{"userStories":[]}"#).unwrap();
        assert_eq!(prd_md_path(&json), None);
    }

    #[test]
    fn prd_md_path_prefers_prd_file_over_prompt() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("brief.md");
        fs::write(&md, "b").unwrap();
        let prompt = tmp.path().join("foo-prompt.md");
        fs::write(&prompt, "p").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, r#"{"prdFile":"brief.md"}"#).unwrap();
        assert_eq!(prd_md_path(&json), Some(md));
    }

    // --- maybe_fire ---

    fn passing_result(tmp: &TempDir) -> LoopResult {
        LoopResult {
            exit_code: 0,
            worktree_path: Some(tmp.path().to_path_buf()),
            branch_name: None,
            was_stopped: false,
            tasks_completed: 5,
            prd_complete: true,
        }
    }

    #[test]
    fn maybe_fire_inner_fires_when_all_gates_pass() {
        // Calls `maybe_fire_inner` so the test exercises every gate with a
        // synthetic TTY (interactive path).
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        let calls = launcher.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, md);
        assert_eq!(calls[0].1, Some(tmp.path().to_path_buf()));
        assert_eq!(calls[0].2, LaunchMode::Interactive);
        assert_eq!(calls[0].3, ReviewHost::Claude);
    }

    #[test]
    fn maybe_fire_with_non_tty_launches_headless_under_auto() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_with(
            &default_config(),
            false,
            false,
            &result,
            &json,
            &launcher,
            false, // non-TTY
        );

        let calls = launcher.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].2, LaunchMode::Headless);
        assert!(calls[0].4.is_some(), "headless must pass a log path");
        assert!(calls[0].5.is_some(), "headless must pass a findings path");

        // Receipt written
        let receipt = tmp
            .path()
            .join(".task-mgr/pending-reviews/foo.json");
        assert!(receipt.exists(), "pending-review receipt must be written");
    }

    #[test]
    fn maybe_fire_with_non_tty_interactive_mode_suppresses_launch() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let mut config = default_config();
        config.auto_review_mode = AutoReviewMode::Interactive;

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_with(&config, false, false, &result, &json, &launcher, false);

        assert!(
            launcher.calls.lock().unwrap().is_empty(),
            "interactive mode must not launch on non-TTY"
        );
        // Receipt still written for manual re-run
        assert!(
            tmp.path()
                .join(".task-mgr/pending-reviews/foo.json")
                .exists()
        );
    }

    #[test]
    fn maybe_fire_uses_grok_when_primary_is_grok() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(
            &grok_primary_config(),
            false,
            false,
            &result,
            &json,
            &launcher,
        );

        let calls = launcher.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].3, ReviewHost::Grok);
    }

    #[test]
    fn maybe_fire_no_launch_on_nonzero_exit() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let mut result = passing_result(&tmp);
        result.exit_code = 1;
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_no_launch_on_was_stopped() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let mut result = passing_result(&tmp);
        result.was_stopped = true;
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_no_launch_below_threshold() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let mut result = passing_result(&tmp);
        result.tasks_completed = 2; // below default min of 3
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_no_launch_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        // cli_force_off disables regardless of config
        maybe_fire_inner(&default_config(), false, true, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_inner_no_launch_when_worktree_missing() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let mut result = passing_result(&tmp);
        result.worktree_path = Some(tmp.path().join("nonexistent-worktree"));
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_inner_suppresses_when_md_path_contains_whitespace() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("My PRD.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("My PRD.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_inner_suppresses_when_md_path_contains_tab() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("My\tPRD.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("My\tPRD.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_inner_proceeds_when_md_path_has_no_whitespace() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("my-prd.md");
        fs::write(&md, "").unwrap();
        let json = tmp.path().join("my-prd.json");
        fs::write(&json, "{}").unwrap();

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert_eq!(launcher.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn maybe_fire_prompt_fallback_allows_launch() {
        let tmp = TempDir::new().unwrap();
        let prompt = tmp.path().join("lean-prompt.md");
        fs::write(&prompt, "# lean").unwrap();
        let json = tmp.path().join("lean.json");
        fs::write(&json, r#"{"userStories":[]}"#).unwrap();

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        let calls = launcher.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, prompt);
    }
}
