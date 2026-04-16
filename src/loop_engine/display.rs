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
        let db_str = h.db_path.display().to_string();
        let db_width = inner - "  DB: ".len();
        lines.push(format!(
            "║  DB: {:<width$}║",
            truncate_display(&db_str, db_width),
            width = db_width
        ));

        // Stop hint
        let stop_file = match h.prefix {
            Some(p) => format!(".stop-{}", p),
            None => ".stop".to_string(),
        };
        let stop_hint = format!("touch {} to stop", stop_file);
        let stop_width = inner - "  Stop: ".len();
        lines.push(format!(
            "║  Stop: {:<width$}║",
            truncate_display(&stop_hint, stop_width),
            width = stop_width
        ));

        // Pause hint
        let pause_width = inner - "  Pause: ".len();
        lines.push(format!(
            "║  Pause: {:<width$}║",
            truncate_display("touch .pause to pause", pause_width),
            width = pause_width
        ));

        // Worktree (only when Some)
        if let Some(wt) = h.worktree_path {
            let wt_str = wt.display().to_string();
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

    #[test]
    fn test_format_session_banner_with_prefix_uses_stop_prefix_hint() {
        use std::path::Path;
        let db = Path::new("/tmp/tasks.db");
        let hints = SessionBannerHints {
            db_path: db,
            prefix: Some("P1"),
            worktree_path: None,
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
        };
        let banner =
            format_session_banner(".task-mgr/tasks/prd.json", "main", 10, None, Some(&hints));
        assert!(
            banner.contains("tasks.db"),
            "Banner must display the DB path hint, got:\n{}",
            banner
        );
    }
}
