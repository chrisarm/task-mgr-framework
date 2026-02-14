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
) {
    eprintln!("\n╔══════════════════════════════════════════════╗");
    eprintln!("║         AUTONOMOUS AGENT LOOP START          ║");
    eprintln!("╠══════════════════════════════════════════════╣");
    eprintln!("║  PRD: {:<38} ║", truncate_display(prd_file, 38));
    eprintln!("║  Branch: {:<35} ║", truncate_display(branch, 35));
    eprintln!("║  Max iterations: {:<27} ║", max_iterations);
    if let Some(hours) = deadline_hours {
        eprintln!("║  Deadline: {:<33} ║", format!("{:.1}h", hours));
    }
    eprintln!("╚══════════════════════════════════════════════╝\n");
}

/// Print an iteration header to stderr.
pub fn print_iteration_header(
    iteration: u32,
    max_iterations: u32,
    task_id: &str,
    elapsed_secs: u64,
) {
    eprintln!(
        "\n═══ Iteration {}/{} ═══ Task: {} ═══ Elapsed: {} ═══",
        iteration,
        max_iterations,
        task_id,
        format_duration(elapsed_secs)
    );
}

/// Print the final session banner to stderr.
pub fn print_final_banner(
    iterations_completed: u32,
    tasks_completed: u32,
    elapsed_secs: u64,
    exit_reason: &str,
) {
    eprintln!("\n╔══════════════════════════════════════════════╗");
    eprintln!("║         AUTONOMOUS AGENT LOOP END            ║");
    eprintln!("╠══════════════════════════════════════════════╣");
    eprintln!("║  Iterations: {:<31} ║", iterations_completed);
    eprintln!("║  Tasks completed: {:<26} ║", tasks_completed);
    eprintln!("║  Total time: {:<31} ║", format_duration(elapsed_secs));
    eprintln!(
        "║  Exit reason: {:<30} ║",
        truncate_display(exit_reason, 30)
    );
    eprintln!("╚══════════════════════════════════════════════╝\n");
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
        print_session_banner("tasks/prd.json", "main", 10, Some(2.0));
    }

    #[test]
    fn test_print_session_banner_no_deadline() {
        print_session_banner("tasks/prd.json", "main", 10, None);
    }

    #[test]
    fn test_print_iteration_header_no_panic() {
        print_iteration_header(3, 10, "FEAT-001", 125);
    }

    #[test]
    fn test_print_final_banner_no_panic() {
        print_final_banner(10, 5, 3600, "all tasks complete");
    }
}
