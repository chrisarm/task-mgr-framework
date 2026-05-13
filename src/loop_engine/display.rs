//! Display functions for the autonomous agent loop.
//!
//! Provides banners, iteration headers, and duration formatting.
//! All output goes to stderr (stdout reserved for Claude subprocess passthrough).

/// Format a duration in seconds to a human-readable string.
///
/// Examples:
/// - 45 → "45s"
/// - 90 → "1m 30s"
/// - 3723 → "1h 2m 3s"
/// - 86400 → "24h 0m 0s"
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        return format!("{}s", secs);
    }

    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else {
        format!("{}m {}s", minutes, seconds)
    }
}

/// Print the session start banner to stderr.
pub fn print_session_banner(
    prd_file: &str,
    branch: &str,
    max_iterations: u32,
    deadline_hours: Option<f64>,
    hints: Option<&SessionBannerHints<'_>>,
) {
    eprint!(
        "{}",
        format_session_banner(prd_file, branch, max_iterations, deadline_hours, hints)
    );
}

/// Operational hints to display in the session start banner.
///
/// These guide the user on how to interact with a running loop (e.g. how to
/// pause it, stop it, where progress is logged, and which worktree is active).
pub struct SessionBannerHints<'a> {
    /// Path to the task-mgr database file shown in the banner.
    pub db_path: &'a std::path::Path,
    /// Optional task-prefix; when `Some("P1")` the stop-file hint shows `.stop-P1`.
    pub prefix: Option<&'a str>,
    /// Optional active worktree path; when `None` the worktree line is omitted.
    pub worktree_path: Option<&'a std::path::Path>,
    /// Directory where `.stop` and `.pause` signal files are read from.
    /// When `Some`, the Stop/Pause hints display a path relative to cwd
    /// (e.g. `tasks/.stop-P1`) so the operator doesn't have to guess where
    /// to drop the file. When `None`, the legacy bare-filename hint is used.
    pub tasks_dir: Option<&'a std::path::Path>,
}

/// Format the session start banner as a string (for testability).
///
/// Returns a multi-line string containing the banner with operational hints.
/// Use [`print_session_banner`] for normal output to stderr.
pub fn format_session_banner(
    prd_file: &str,
    branch: &str,
    max_iterations: u32,
    deadline_hours: Option<f64>,
    hints: Option<&SessionBannerHints<'_>>,
) -> String {
    const MIN_WIDTH: usize = 48;
    const MAX_WIDTH: usize = 110;
    let term_width = terminal_width().unwrap_or(MIN_WIDTH);
    let width = term_width.clamp(MIN_WIDTH, MAX_WIDTH);
    let inner = width - 2;

    let mut lines = Vec::new();

    let top = format!("╔{}╗", "═".repeat(inner));
    let sep = format!("╠{}╣", "═".repeat(inner));
    let bot = format!("╚{}╝", "═".repeat(inner));

    let title = format!("║{:^width$}║", "AUTONOMOUS AGENT LOOP START", width = inner);

    lines.push(String::new());
    lines.push(top);
    lines.push(title);
    lines.push(sep.clone());

    // Core fields
    let prd_width = inner - "  PRD: ".len();
    lines.push(format!(
        "║  PRD: {:<width$}║",
        truncate_display(prd_file, prd_width),
        width = prd_width
    ));
    let branch_width = inner - "  Branch: ".len();
    lines.push(format!(
        "║  Branch: {:<width$}║",
        truncate_display(branch, branch_width),
        width = branch_width
    ));
    let iter_width = inner - "  Max iterations: ".len();
    lines.push(format!(
        "║  Max iterations: {:<width$}║",
        max_iterations,
        width = iter_width
    ));
    if let Some(hours) = deadline_hours {
        let dl_width = inner - "  Deadline: ".len();
        lines.push(format!(
            "║  Deadline: {:<width$}║",
            format!("{:.1}h", hours),
            width = dl_width
        ));
    }

    // Hints section
    if let Some(h) = hints {
        lines.push(sep.clone());

        // DB path
        let db_str = shorten_path_for_display(h.db_path);
        let db_width = inner - "  DB: ".len();
        lines.push(format!(
            "║  DB: {:<width$}║",
            truncate_display(&db_str, db_width),
            width = db_width
        ));

        // Stop hint
        let stop_name = match h.prefix {
            Some(p) => format!(".stop-{}", p),
            None => ".stop".to_string(),
        };
        let stop_display = match h.tasks_dir {
            Some(dir) => shorten_path_for_display(&dir.join(&stop_name)),
            None => stop_name,
        };
        let stop_hint = format!("touch {} to stop", stop_display);
        let stop_width = inner - "  Stop: ".len();
        lines.push(format!(
            "║  Stop: {:<width$}║",
            truncate_display(&stop_hint, stop_width),
            width = stop_width
        ));

        // Pause hint
        let pause_name = match h.prefix {
            Some(p) => format!(".pause-{}", p),
            None => ".pause".to_string(),
        };
        let pause_display = match h.tasks_dir {
            Some(dir) => shorten_path_for_display(&dir.join(&pause_name)),
            None => pause_name,
        };
        let pause_hint = format!("touch {} to pause", pause_display);
        let pause_width = inner - "  Pause: ".len();
        lines.push(format!(
            "║  Pause: {:<width$}║",
            truncate_display(&pause_hint, pause_width),
            width = pause_width
        ));

        // Worktree (only when Some)
        if let Some(wt) = h.worktree_path {
            let wt_str = shorten_path_for_display(wt);
            let wt_width = inner - "  Worktree: ".len();
            lines.push(format!(
                "║  Worktree: {:<width$}║",
                truncate_display(&wt_str, wt_width),
                width = wt_width
            ));
        }
    }

    lines.push(bot);
    lines.push(String::new());
    lines.join("\n")
}

/// Format an iteration header as a string (for testability).
pub fn format_iteration_header(
    iteration: u32,
    max_iterations: u32,
    task_id: &str,
    elapsed_secs: u64,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    let model_display = model.unwrap_or("(default)");
    let effort_display = effort.unwrap_or("(default)");
    format!(
        "\n═══ Iteration {}/{} ═══ Task: {} ═══ Model: {} ═══ Effort: {} ═══ Elapsed: {} ═══",
        iteration,
        max_iterations,
        task_id,
        model_display,
        effort_display,
        format_duration(elapsed_secs)
    )
}

/// Format an iteration banner with optional overflow-recovery annotation.
///
/// Identical to [`format_iteration_header`] except the `Model:` field is
/// suffixed with ` (overflow recovery from <original>)` when `task_id` is
/// present in `overflow_recovered`. Gates on the dedicated `overflow_recovered`
/// set rather than `model_overrides` so crash-escalation and consecutive-
/// failure escalation paths (which also write `model_overrides`) cannot trip
/// the annotation (learning #893).
///
/// If `overflow_recovered` flags the task but `overflow_original_model` lacks
/// an entry (defensive — the recovery handler always inserts both together),
/// falls back to ` (overflow recovery)` without the `from X` suffix.
#[allow(clippy::too_many_arguments)]
pub fn format_iteration_banner_with_recovery(
    iteration: u32,
    max_iterations: u32,
    task_id: &str,
    elapsed_secs: u64,
    model: Option<&str>,
    effort: Option<&str>,
    overflow_recovered: &std::collections::HashSet<String>,
    overflow_original_model: &std::collections::HashMap<String, String>,
) -> String {
    let model_display = model.unwrap_or("(default)");
    let recovery_suffix = if overflow_recovered.contains(task_id) {
        match overflow_original_model.get(task_id) {
            Some(orig) => format!(" (overflow recovery from {})", orig),
            None => " (overflow recovery)".to_string(),
        }
    } else {
        String::new()
    };
    let effort_display = effort.unwrap_or("(default)");
    format!(
        "\n═══ Iteration {}/{} ═══ Task: {} ═══ Model: {}{} ═══ Effort: {} ═══ Elapsed: {} ═══",
        iteration,
        max_iterations,
        task_id,
        model_display,
        recovery_suffix,
        effort_display,
        format_duration(elapsed_secs)
    )
}

/// Print an iteration header to stderr.
pub fn print_iteration_header(
    iteration: u32,
    max_iterations: u32,
    task_id: &str,
    elapsed_secs: u64,
    model: Option<&str>,
    effort: Option<&str>,
) {
    eprintln!(
        "{}",
        format_iteration_header(
            iteration,
            max_iterations,
            task_id,
            elapsed_secs,
            model,
            effort,
        )
    );
}

/// Print the final session banner to stderr.
pub fn print_final_banner(
    iterations_completed: u32,
    tasks_completed: u32,
    elapsed_secs: u64,
    exit_reason: &str,
    prd_file: &str,
) {
    eprint!(
        "{}",
        format_final_banner(
            iterations_completed,
            tasks_completed,
            elapsed_secs,
            exit_reason,
            prd_file,
        )
    );
}

/// Format the final session banner as a string (for testability).
///
/// Adapts width to the terminal: minimum 48 columns, expands up to terminal
/// width (capped at 80). Falls back to 48 if terminal size can't be detected.
pub fn format_final_banner(
    iterations_completed: u32,
    tasks_completed: u32,
    elapsed_secs: u64,
    exit_reason: &str,
    prd_file: &str,
) -> String {
    const MIN_WIDTH: usize = 48;
    const MAX_WIDTH: usize = 110;

    let term_width = terminal_width().unwrap_or(MIN_WIDTH);
    let width = term_width.clamp(MIN_WIDTH, MAX_WIDTH);
    let inner = width - 2; // space between ║ and ║

    let top = format!("╔{}╗", "═".repeat(inner));
    let sep = format!("╠{}╣", "═".repeat(inner));
    let bot = format!("╚{}╝", "═".repeat(inner));
    let title = format!("║{:^inner$}║", "AUTONOMOUS AGENT LOOP END");

    let pad = inner - 2; // content area after "║  " and before "║"

    let mut lines = vec![String::new(), top, title, sep];

    let fields: Vec<(&str, String)> = vec![
        ("PRD", truncate_display(prd_file, pad - "PRD: ".len())),
        ("Iterations", iterations_completed.to_string()),
        ("Tasks completed", tasks_completed.to_string()),
        ("Total time", format_duration(elapsed_secs)),
        (
            "Exit reason",
            truncate_display(exit_reason, pad - "Exit reason: ".len()),
        ),
    ];
    for (label, value) in &fields {
        let content = format!("{}: {}", label, value);
        lines.push(format!("║  {:<pad$}║", content));
    }

    lines.push(bot);
    lines.push(String::new());
    lines.join("\n")
}

/// Get the terminal width in columns, or `None` if unavailable.
#[cfg(unix)]
fn terminal_width() -> Option<usize> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && ws.ws_col > 0 {
        Some(ws.ws_col as usize)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn terminal_width() -> Option<usize> {
    None
}

/// Shorten an absolute path for banner display by replacing the current
/// working directory or `$HOME` with a relative or `~`-prefixed form.
///
/// Priority:
/// 1. If `path` is under `cwd`, return a path relative to cwd (e.g.
///    `.task-mgr/tasks.db`). An exact cwd match returns `.`.
/// 2. Else if `path` is under `$HOME`, return `~/...`.
/// 3. Else return the absolute path unchanged.
///
/// On any failure (cwd unavailable, `$HOME` unset, non-prefix path), falls
/// back to the absolute form so the banner is always meaningful.
fn shorten_path_for_display(path: &std::path::Path) -> String {
    let cwd = std::env::current_dir().ok();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    shorten_path_for_display_with(path, cwd.as_deref(), home.as_deref())
}

/// Inner form of [`shorten_path_for_display`] with injectable `cwd` and
/// `home` for deterministic tests.
fn shorten_path_for_display_with(
    path: &std::path::Path,
    cwd: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> String {
    if let Some(cwd) = cwd {
        if path == cwd {
            return ".".to_string();
        }
        if let Ok(rel) = path.strip_prefix(cwd) {
            return rel.display().to_string();
        }
    }
    if let Some(home) = home
        && let Ok(rel) = path.strip_prefix(home)
    {
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}

/// Truncate a string for display in a fixed-width box.
fn truncate_display(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::model::SONNET_MODEL;

    // --- format_duration tests ---

    #[test]
    fn test_format_duration_seconds_only() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(1), "1s");
        assert_eq!(format_duration(59), "59s");
    }

    #[test]
    fn test_format_duration_minutes_and_seconds() {
        assert_eq!(format_duration(60), "1m 0s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3599), "59m 59s");
    }

    #[test]
    fn test_format_duration_hours_minutes_seconds() {
        assert_eq!(format_duration(3600), "1h 0m 0s");
        assert_eq!(format_duration(3723), "1h 2m 3s");
        assert_eq!(format_duration(86400), "24h 0m 0s");
    }

    #[test]
    fn test_format_duration_large_values() {
        // 100 hours
        assert_eq!(format_duration(360000), "100h 0m 0s");
    }

    // --- truncate_display tests ---

    #[test]
    fn test_truncate_display_short_string() {
        assert_eq!(truncate_display("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_display_exact_length() {
        assert_eq!(truncate_display("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_display_long_string() {
        let result = truncate_display("this is a very long string", 10);
        assert_eq!(result, "this is...");
        assert!(result.len() <= 10);
    }

    #[test]
    fn test_truncate_display_empty_string() {
        assert_eq!(truncate_display("", 10), "");
    }

    // --- Banner functions produce output without panicking ---

    #[test]
    fn test_print_session_banner_no_panic() {
        print_session_banner(".task-mgr/tasks/prd.json", "main", 10, Some(2.0), None);
    }

    #[test]
    fn test_print_session_banner_no_deadline() {
        print_session_banner(".task-mgr/tasks/prd.json", "main", 10, None, None);
    }

    #[test]
    fn test_print_iteration_header_no_panic() {
        print_iteration_header(3, 10, "FEAT-001", 125, None, None);
    }

    #[test]
    fn test_format_iteration_header_with_none_model() {
        let header = format_iteration_header(3, 10, "FEAT-001", 125, None, None);
        assert!(header.contains("Iteration 3/10"));
        assert!(header.contains("Task: FEAT-001"));
        assert!(header.contains("Model: (default)"));
        assert!(header.contains("Effort: (default)"));
        assert!(header.contains("Elapsed: 2m 5s"));
    }

    #[test]
    fn test_format_iteration_header_with_some_model() {
        let header =
            format_iteration_header(1, 5, "TEST-002", 60, Some(SONNET_MODEL), Some("xhigh"));
        assert!(header.contains("Iteration 1/5"));
        assert!(header.contains("Task: TEST-002"));
        assert!(header.contains(&format!("Model: {SONNET_MODEL}")));
        assert!(header.contains("Effort: xhigh"));
        assert!(header.contains("Elapsed: 1m 0s"));
    }

    #[test]
    fn test_print_final_banner_no_panic() {
        print_final_banner(10, 5, 3600, "all tasks complete", "my-prd");
    }

    // --- TEST-INIT-003: format_session_banner() with hints ---

    #[test]
    fn test_format_session_banner_with_all_hints_no_panic() {
        use std::path::Path;
        let db = Path::new("/tmp/tasks.db");
        let wt = Path::new("/tmp/worktrees/feat-branch");
        let hints = SessionBannerHints {
            db_path: db,
            prefix: None,
            worktree_path: Some(wt),
            tasks_dir: None,
        };
        // Must not panic and must return a non-empty string
        let banner = format_session_banner(
            ".task-mgr/tasks/prd.json",
            "main",
            10,
            Some(2.0),
            Some(&hints),
        );
        assert!(!banner.is_empty(), "banner must be non-empty");
    }

    #[test]
    fn test_format_session_banner_without_worktree_omits_worktree_line() {
        use std::path::Path;
        let db = Path::new("/tmp/tasks.db");
        let hints = SessionBannerHints {
            db_path: db,
            prefix: None,
            worktree_path: None,
            tasks_dir: None,
        };
        let banner =
            format_session_banner(".task-mgr/tasks/prd.json", "main", 10, None, Some(&hints));
        let banner_lower = banner.to_lowercase();
        assert!(
            !banner_lower.contains("worktree"),
            "Banner without worktree_path must not mention 'worktree', got:\n{}",
            banner
        );
    }

    // --- shorten_path_for_display tests ---

    #[test]
    fn test_shorten_path_under_cwd_returns_relative() {
        use std::path::Path;
        let cwd = Path::new("/work/project");
        let p = Path::new("/work/project/.task-mgr/tasks.db");
        let out = shorten_path_for_display_with(p, Some(cwd), Some(Path::new("/home/u")));
        assert_eq!(out, ".task-mgr/tasks.db");
    }

    #[test]
    fn test_shorten_path_exactly_cwd_returns_dot() {
        use std::path::Path;
        let cwd = Path::new("/work/project");
        let out = shorten_path_for_display_with(cwd, Some(cwd), None);
        assert_eq!(out, ".");
    }

    #[test]
    fn test_shorten_path_under_home_returns_tilde() {
        use std::path::Path;
        let cwd = Path::new("/work/project");
        let home = Path::new("/home/u");
        let p = Path::new("/home/u/repos/project-worktrees/feat-x");
        let out = shorten_path_for_display_with(p, Some(cwd), Some(home));
        assert_eq!(out, "~/repos/project-worktrees/feat-x");
    }

    #[test]
    fn test_shorten_path_outside_cwd_and_home_returns_absolute() {
        use std::path::Path;
        let p = Path::new("/tmp/elsewhere/tasks.db");
        let out = shorten_path_for_display_with(
            p,
            Some(Path::new("/work/project")),
            Some(Path::new("/home/u")),
        );
        assert_eq!(out, "/tmp/elsewhere/tasks.db");
    }

    #[test]
    fn test_shorten_path_cwd_takes_priority_over_home() {
        use std::path::Path;
        // path is under both cwd and home; cwd wins (relative is more useful).
        let cwd = Path::new("/home/u/work");
        let home = Path::new("/home/u");
        let p = Path::new("/home/u/work/.task-mgr/tasks.db");
        let out = shorten_path_for_display_with(p, Some(cwd), Some(home));
        assert_eq!(out, ".task-mgr/tasks.db");
    }

    #[test]
    fn test_format_session_banner_with_tasks_dir_shows_stop_pause_relative() {
        use std::path::Path;
        // Use short paths and a short prefix so the Stop/Pause hint survives
        // the MIN_WIDTH (48-col) truncation when no terminal is detected.
        let db = Path::new("/x/db.db");
        let tasks_dir = Path::new("/x/t");
        let hints = SessionBannerHints {
            db_path: db,
            prefix: Some("P1"),
            worktree_path: None,
            tasks_dir: Some(tasks_dir),
        };
        let banner =
            format_session_banner("prd.json", "main", 5, None, Some(&hints));
        assert!(
            banner.contains("/x/t/.stop-P1"),
            "Stop hint must include tasks_dir prefix, got:\n{}",
            banner
        );
        assert!(
            banner.contains("/x/t/.pause-P1"),
            "Pause hint must include tasks_dir prefix, got:\n{}",
            banner
        );
    }

    #[test]
    fn test_format_session_banner_with_prefix_uses_stop_prefix_hint() {
        use std::path::Path;
        let db = Path::new("/tmp/tasks.db");
        let hints = SessionBannerHints {
            db_path: db,
            prefix: Some("P1"),
            worktree_path: None,
            tasks_dir: None,
        };
        let banner =
            format_session_banner(".task-mgr/tasks/prd.json", "main", 10, None, Some(&hints));
        assert!(
            banner.contains(".stop-P1"),
            "Banner with prefix 'P1' must contain '.stop-P1' in stop-file hint, got:\n{}",
            banner
        );
    }

    #[test]
    fn test_format_session_banner_without_prefix_uses_plain_stop_hint() {
        use std::path::Path;
        let db = Path::new("/tmp/tasks.db");
        let hints = SessionBannerHints {
            db_path: db,
            prefix: None,
            worktree_path: None,
            tasks_dir: None,
        };
        let banner =
            format_session_banner(".task-mgr/tasks/prd.json", "main", 5, None, Some(&hints));
        // Must contain ".stop" but NOT ".stop-" (no prefix suffix)
        assert!(
            banner.contains(".stop"),
            "Banner without prefix must contain '.stop' hint, got:\n{}",
            banner
        );
        assert!(
            !banner.contains(".stop-"),
            "Banner without prefix must NOT contain '.stop-<prefix>', got:\n{}",
            banner
        );
    }

    #[test]
    fn test_format_session_banner_with_db_path_in_hints() {
        use std::path::Path;
        let db = Path::new("/home/user/.task-mgr/tasks.db");
        let hints = SessionBannerHints {
            db_path: db,
            prefix: None,
            worktree_path: None,
            tasks_dir: None,
        };
        let banner =
            format_session_banner(".task-mgr/tasks/prd.json", "main", 10, None, Some(&hints));
        assert!(
            banner.contains("tasks.db"),
            "Banner must display the DB path hint, got:\n{}",
            banner
        );
    }

    // --- TEST-INIT-005: banner annotation gating on overflow_recovered ---

    use crate::loop_engine::engine::IterationContext;
    use crate::loop_engine::model::OPUS_MODEL;

    /// Positive: overflow_recovered marks the task AND original is recorded →
    /// banner Model field gets the "(overflow recovery from <orig>)" suffix.
    #[test]
    fn test_banner_annotated_when_overflow_recovered_with_original() {
        let mut ctx = IterationContext::new(3);
        let task_id = "FEAT-042".to_string();
        ctx.overflow_recovered.insert(task_id.clone());
        ctx.overflow_original_model
            .insert(task_id.clone(), SONNET_MODEL.to_string());

        let banner = format_iteration_banner_with_recovery(
            1,
            10,
            &task_id,
            30,
            Some(OPUS_MODEL),
            Some("high"),
            &ctx.overflow_recovered,
            &ctx.overflow_original_model,
        );

        assert!(
            banner.contains(&format!("(overflow recovery from {})", SONNET_MODEL)),
            "Banner must include 'overflow recovery from <orig>' suffix; got:\n{}",
            banner
        );
        assert!(
            banner.contains(&format!("Model: {}", OPUS_MODEL)),
            "Banner must still show current (post-escalation) model; got:\n{}",
            banner
        );
    }

    /// Negative (BLOCKER-1): model_overrides has the task but
    /// overflow_recovered does NOT — banner MUST NOT carry the recovery
    /// annotation. This guards against future writers to model_overrides
    /// (e.g. crash escalation) tripping the banner.
    #[test]
    fn test_banner_not_annotated_when_only_model_overrides_set() {
        let mut ctx = IterationContext::new(3);
        let task_id = "FEAT-042".to_string();
        ctx.model_overrides
            .insert(task_id.clone(), OPUS_MODEL.to_string());
        // Intentionally NOT inserting into overflow_recovered.

        let banner = format_iteration_banner_with_recovery(
            1,
            10,
            &task_id,
            30,
            Some(OPUS_MODEL),
            Some("high"),
            &ctx.overflow_recovered,
            &ctx.overflow_original_model,
        );

        assert!(
            !banner.contains("overflow recovery"),
            "Banner must NOT show overflow recovery when only model_overrides is set; got:\n{}",
            banner
        );
    }

    /// No-op: neither field set → banner matches existing format unchanged.
    #[test]
    fn test_banner_unchanged_when_no_overflow_state() {
        let ctx = IterationContext::new(3);
        let task_id = "FEAT-042";

        let banner = format_iteration_banner_with_recovery(
            1,
            10,
            task_id,
            30,
            Some(OPUS_MODEL),
            Some("high"),
            &ctx.overflow_recovered,
            &ctx.overflow_original_model,
        );
        let baseline = format_iteration_header(1, 10, task_id, 30, Some(OPUS_MODEL), Some("high"));

        assert_eq!(
            banner, baseline,
            "Banner with no overflow state must equal the existing format_iteration_header output"
        );
        assert!(!banner.contains("overflow recovery"));
    }

    /// Defensive: overflow_recovered marks the task but overflow_original_model
    /// has no entry. The annotation should appear without "from X" — and
    /// crucially must not contain the literal "None".
    #[test]
    fn test_banner_falls_back_when_original_model_missing() {
        let mut ctx = IterationContext::new(3);
        let task_id = "FEAT-042".to_string();
        ctx.overflow_recovered.insert(task_id.clone());
        // overflow_original_model intentionally empty.

        let banner = format_iteration_banner_with_recovery(
            1,
            10,
            &task_id,
            30,
            Some(OPUS_MODEL),
            Some("high"),
            &ctx.overflow_recovered,
            &ctx.overflow_original_model,
        );

        assert!(
            banner.contains("(overflow recovery)"),
            "Defensive fallback annotation must appear; got:\n{}",
            banner
        );
        assert!(
            !banner.contains("from"),
            "Fallback must omit the 'from <orig>' clause; got:\n{}",
            banner
        );
        assert!(
            !banner.contains("None"),
            "Banner must never leak literal 'None' into user output; got:\n{}",
            banner
        );
    }
}
