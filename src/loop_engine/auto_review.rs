//! Auto-launch of `/review-loop` after a successful loop or batch run.
//!
//! # Design notes
//!
//! **Env-var inheritance is intentional.** [`ProcessLauncher`] builds a
//! [`std::process::Command`] without calling `.env_clear()`, so `ANTHROPIC_API_KEY`
//! and any other ambient variables the user has set are inherited by the spawned
//! `claude` process. Stripping the environment would silently break authentication.
//!
//! **TTY inheritance is automatic.** `Command::status()` connects the child's
//! stdin/stdout/stderr to the parent process's file descriptors. This means the
//! spawned `claude` session is fully interactive — the user lands in a live
//! terminal session. Never add `--print` or `-p` to the spawned command; those
//! flags force non-interactive output capture and defeat the purpose.
//!
//! **Worktree-suppression rationale.** When `LoopResult::worktree_path` is `None`
//! (or the path no longer exists on disk), `maybe_fire` prints a hint and returns
//! without launching. It does NOT fall back to `project_root`. Running `/review-loop`
//! from the main worktree would check out the feature branch there, which is
//! push-protected in most CI setups and risks dirty-state collisions with other
//! in-flight loops.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::loop_engine::engine::LoopResult;
use crate::loop_engine::project_config::ProjectConfig;

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
// PRD markdown path resolution
// ---------------------------------------------------------------------------

/// Resolve the PRD markdown path from a PRD JSON path.
///
/// Tries two conventions:
/// 1. `foo.md` (same stem, extension swapped)
/// 2. `prd-foo.md` (prefixed form in the same directory)
///
/// Returns `None` when neither exists on disk.
///
/// **Trust boundary**: `prd_json` is an operator-controlled CLI argument (the
/// PRD JSON path passed to `task-mgr loop`). No path-escape detection is
/// performed — the caller is trusted. Do not forward user-supplied, untrusted
/// paths here without validation.
pub fn prd_md_path(prd_json: &Path) -> Option<PathBuf> {
    let bare = prd_json.with_extension("md");
    if bare.exists() {
        return Some(bare);
    }
    let stem = prd_json.file_stem()?.to_str()?;
    let parent = prd_json.parent()?;
    let prefixed = parent.join(format!("prd-{stem}.md"));
    if prefixed.exists() {
        return Some(prefixed);
    }
    None
}

// ---------------------------------------------------------------------------
// Launcher abstraction
// ---------------------------------------------------------------------------

/// Abstraction for launching the `/review-loop` claude session.
///
/// Production code uses [`ProcessLauncher`]; tests use [`CapturingLauncher`].
pub trait ReviewLauncher {
    fn launch(&self, md: &Path, worktree: Option<&Path>) -> std::io::Result<()>;
}

/// Production launcher — spawns an interactive `claude` process.
pub struct ProcessLauncher;

impl ReviewLauncher for ProcessLauncher {
    fn launch(&self, md: &Path, worktree: Option<&Path>) -> std::io::Result<()> {
        let claude_bin = std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string());

        let prompt_arg = format!("/review-loop {}", md.display());
        let mut cmd = Command::new(&claude_bin);
        cmd.arg(prompt_arg);

        if let Some(dir) = worktree {
            cmd.current_dir(dir);
        }

        match cmd.status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => {
                eprintln!(
                    "[auto-review] claude exited with status {status}; \
                     re-run `claude \"/review-loop {path}\"` manually if needed",
                    path = md.display()
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// Test-only launcher that records calls instead of spawning processes.
#[cfg(test)]
pub(crate) struct CapturingLauncher {
    pub calls: std::sync::Mutex<Vec<(PathBuf, Option<PathBuf>)>>,
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
    fn launch(&self, md: &Path, worktree: Option<&Path>) -> std::io::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push((md.to_path_buf(), worktree.map(Path::to_path_buf)));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Maybe fire the auto-review launcher after a loop run.
///
/// Performs the TTY pre-check (interactive review requires a real terminal),
/// then delegates the remaining gate logic to `maybe_fire_inner`. Any
/// failing gate prints a hint to stderr and returns without launching;
/// launcher errors are logged but never propagated — a review launch failure
/// must never change the loop's exit code.
///
/// **Why the split**: under `cargo test` the process is non-TTY, so every
/// gate after the TTY check would otherwise be unreachable from a test
/// path — and gate tests would pass vacuously regardless of the gate's
/// correctness. Tests bypass the TTY check by calling `maybe_fire_inner`
/// directly. See the gate-ordering caveat in this project's `CLAUDE.md`.
pub fn maybe_fire(
    config: &ProjectConfig,
    cli_force_on: bool,
    cli_force_off: bool,
    result: &LoopResult,
    prd_json: &Path,
    launcher: &dyn ReviewLauncher,
) {
    if !std::io::stdout().is_terminal() {
        eprintln!(
            "[auto-review] stdout is not a TTY (CI / redirected); \
             run `claude \"/review-loop {path}\"` manually",
            path = prd_json.display()
        );
        return;
    }
    maybe_fire_inner(
        config,
        cli_force_on,
        cli_force_off,
        result,
        prd_json,
        launcher,
    );
}

/// All gate logic for [`maybe_fire`], assuming the TTY pre-check has passed.
///
/// Production callers must go through [`maybe_fire`] (which performs the TTY
/// check first). This function is `pub(crate)` solely as a **test seam**:
/// `cargo test` runs in a non-TTY env, so without this seam every gate below
/// would be unreachable from tests and would pass vacuously. Tests assert
/// against the `CapturingLauncher` after calling this function directly.
pub(crate) fn maybe_fire_inner(
    config: &ProjectConfig,
    cli_force_on: bool,
    cli_force_off: bool,
    result: &LoopResult,
    prd_json: &Path,
    launcher: &dyn ReviewLauncher,
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

    let worktree = result.worktree_path.as_deref();
    match worktree {
        None => {
            eprintln!(
                "[auto-review] no worktree path available; \
                 run `claude \"/review-loop {path}\"` manually in your feature worktree",
                path = prd_json.display()
            );
            return;
        }
        // Best-effort existence check: a concurrent `git worktree prune` can
        // remove the worktree between this check and the launcher's `current_dir`
        // call (TOCTOU). The launcher's own `Command::status()` error path
        // surfaces the racy outcome via the `Err(e)` arm below.
        Some(wt) if !wt.exists() => {
            eprintln!(
                "[auto-review] worktree `{wt}` does not exist; \
                 run `claude \"/review-loop {path}\"` manually in your feature worktree",
                wt = wt.display(),
                path = prd_json.display()
            );
            return;
        }
        _ => {}
    }

    let md = match prd_md_path(prd_json) {
        Some(p) => p,
        None => {
            eprintln!(
                "[auto-review] could not find a markdown PRD for `{}`; \
                 run `/review-loop` manually",
                prd_json.display()
            );
            return;
        }
    };

    if md.to_string_lossy().chars().any(char::is_whitespace) {
        eprintln!(
            "[auto-review] PRD path `{}` contains whitespace; Claude's slash-command \
             parser would fragment it. Rename the file to remove spaces/tabs, then \
             run `claude \"/review-loop {}\"` manually.",
            md.display(),
            md.display()
        );
        return;
    }

    if let Err(e) = launcher.launch(&md, worktree) {
        eprintln!(
            "[auto-review] failed to launch claude ({}); \
             run `claude \"/review-loop {path}\"` manually",
            e,
            path = md.display()
        );
    }
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

    // --- prd_md_path ---

    #[test]
    fn prd_md_path_bare_exists() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        assert_eq!(prd_md_path(&json), Some(md));
    }

    #[test]
    fn prd_md_path_prefixed_exists() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("prd-foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");
        assert_eq!(prd_md_path(&json), Some(md));
    }

    #[test]
    fn prd_md_path_neither_exists() {
        let tmp = TempDir::new().unwrap();
        let json = tmp.path().join("foo.json");
        assert_eq!(prd_md_path(&json), None);
    }

    // --- maybe_fire ---

    fn passing_result(tmp: &TempDir) -> LoopResult {
        LoopResult {
            exit_code: 0,
            worktree_path: Some(tmp.path().to_path_buf()),
            branch_name: None,
            was_stopped: false,
            tasks_completed: 5,
        }
    }

    #[test]
    fn maybe_fire_inner_fires_when_all_gates_pass() {
        // Calls `maybe_fire_inner` so the test exercises every gate after the
        // TTY pre-check — under `cargo test` (non-TTY) the public `maybe_fire`
        // would short-circuit before reaching the launcher.
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        let calls = launcher.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, md);
        assert_eq!(calls[0].1, Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn maybe_fire_outer_suppresses_in_non_tty() {
        // Direct check of the outer's TTY gate. `cargo test` runs in a non-TTY
        // env, so a call through the public `maybe_fire` with otherwise-passing
        // inputs must suppress the launch. This is the discriminator that
        // proves the TTY gate is doing real work (removing it would let the
        // launcher fire — `CapturingLauncher.calls.len() == 1`).
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire(&default_config(), false, false, &result, &json, &launcher);

        assert!(
            launcher.calls.lock().unwrap().is_empty(),
            "outer maybe_fire must suppress launch when stdout is not a TTY",
        );
    }

    #[test]
    fn maybe_fire_no_launch_on_nonzero_exit() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");

        let launcher = CapturingLauncher::new();
        let mut result = passing_result(&tmp);
        result.exit_code = 1;
        maybe_fire(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_no_launch_on_was_stopped() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");

        let launcher = CapturingLauncher::new();
        let mut result = passing_result(&tmp);
        result.was_stopped = true;
        maybe_fire(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_no_launch_below_threshold() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");

        let launcher = CapturingLauncher::new();
        let mut result = passing_result(&tmp);
        result.tasks_completed = 2; // below default min of 3
        maybe_fire(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_no_launch_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        // cli_force_off disables regardless of config
        maybe_fire(&default_config(), false, true, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_inner_no_launch_when_worktree_missing() {
        // Worktree-existence gate fires AFTER the TTY check, so call the inner
        // so the assertion actually exercises that gate (otherwise the test
        // would pass vacuously under `cargo test` via the TTY gate).
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("foo.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("foo.json");

        let launcher = CapturingLauncher::new();
        let mut result = passing_result(&tmp);
        result.worktree_path = Some(tmp.path().join("nonexistent-worktree"));
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_inner_suppresses_when_md_path_contains_whitespace() {
        // Whitespace guard sits AFTER the TTY check in `maybe_fire`. Call the
        // inner so the assertion is a real test of the guard — deleting the
        // guard would let the launcher fire and `calls.len()` become 1.
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("My PRD.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("My PRD.json");

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_inner_suppresses_when_md_path_contains_tab() {
        // Same rationale as the space variant — exercises the `char::is_whitespace`
        // path that an ASCII-only check would miss.
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("My\tPRD.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("My\tPRD.json");

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert!(launcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn maybe_fire_inner_proceeds_when_md_path_has_no_whitespace() {
        // Positive control: with the whitespace guard's predicate false, the
        // launcher fires. Pairs with the two suppression tests above to prove
        // the guard fires exactly when it should and not otherwise.
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("my-prd.md");
        std::fs::write(&md, "").unwrap();
        let json = tmp.path().join("my-prd.json");

        let launcher = CapturingLauncher::new();
        let result = passing_result(&tmp);
        maybe_fire_inner(&default_config(), false, false, &result, &json, &launcher);

        assert_eq!(launcher.calls.lock().unwrap().len(), 1);
    }
}
