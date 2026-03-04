//! Rendering functions for the status dashboard.
//!
//! Formats a [`DashboardResult`] into human-readable text output.
//! Called from `handlers.rs` via the `TextFormattable` trait.

use super::status::DashboardResult;

/// Format the dashboard result as human-readable text.
pub fn format_text(result: &DashboardResult) -> String {
    let mut output = String::new();

    // Status icon based on completion
    let icon = status_icon(result.completion_percentage);

    output.push_str("=== Status Dashboard ===\n");

    // Project info
    if let Some(ref project) = result.project {
        output.push_str(&format!("{} Project: {}\n", icon, project.name));
        if let Some(ref branch) = project.branch {
            output.push_str(&format!("  Branch:  {}\n", branch));
        }
    } else {
        output.push_str(&format!("{} No project initialized\n", icon));
    }

    // Completion bar
    output.push('\n');
    output.push_str(&format!(
        "Progress: {}/{} tasks ({:.1}%)\n",
        result.tasks.done, result.tasks.total, result.completion_percentage
    ));
    output.push_str(&format!(
        "  {}\n",
        progress_bar(result.completion_percentage)
    ));

    // Status breakdown
    output.push('\n');
    output.push_str("Status:\n");
    output.push_str(&format!("  done:        {:>4}\n", result.tasks.done));
    output.push_str(&format!("  todo:        {:>4}\n", result.tasks.todo));
    output.push_str(&format!("  in_progress: {:>4}\n", result.tasks.in_progress));
    output.push_str(&format!("  blocked:     {:>4}\n", result.tasks.blocked));
    output.push_str(&format!("  skipped:     {:>4}\n", result.tasks.skipped));
    output.push_str(&format!("  irrelevant:  {:>4}\n", result.tasks.irrelevant));

    // Deadline
    if let Some(ref deadline) = result.deadline {
        output.push('\n');
        if deadline.expired {
            output.push_str(&format!("Deadline: EXPIRED ({})\n", deadline.prd_basename));
        } else {
            output.push_str(&format!(
                "Deadline: {} ({})\n",
                deadline.time_remaining, deadline.prd_basename
            ));
        }
    }

    // Verbose: pending tasks
    if !result.pending_tasks.is_empty() {
        output.push('\n');
        output.push_str("Pending tasks:\n");
        for task in &result.pending_tasks {
            let status_indicator = match task.status.as_str() {
                "in_progress" => ">",
                "blocked" => "!",
                _ => " ",
            };
            output.push_str(&format!(
                "  {} [P{}] {} - {}\n",
                status_indicator, task.priority, task.id, task.title
            ));
        }
    }

    // Multi-PRD summary table
    if !result.prd_summaries.is_empty() {
        output.push('\n');
        output.push_str("PRD Summaries:\n");
        output.push_str(&format!(
            "  {:<12} {:>5} {:>5} {:>5} {:>7}  {}\n",
            "PREFIX", "TOTAL", "DONE", "WIP", "%", "LOCK"
        ));
        output.push_str(&format!("  {}\n", "-".repeat(48)));
        for prd in &result.prd_summaries {
            let lock_indicator = if prd.active_lock { "[ACTIVE]" } else { "" };
            output.push_str(&format!(
                "  {:<12} {:>5} {:>5} {:>5} {:>6.1}%  {}\n",
                prd.prefix,
                prd.total,
                prd.done,
                prd.in_progress,
                prd.completion_pct,
                lock_indicator,
            ));
        }
    }

    output
}

/// Generate a progress bar string.
fn progress_bar(percentage: f64) -> String {
    let filled = (percentage / 5.0).round() as usize; // 20 chars wide
    let empty = 20_usize.saturating_sub(filled);
    format!(
        "[{}{}] {:.1}%",
        "#".repeat(filled),
        "-".repeat(empty),
        percentage
    )
}

/// Return a status icon based on completion percentage.
fn status_icon(percentage: f64) -> &'static str {
    if percentage >= 100.0 {
        "[DONE]"
    } else if percentage > 0.0 {
        "[....]"
    } else {
        "[    ]"
    }
}

#[cfg(test)]
mod tests {
    use super::super::status::{
        DashboardTaskCounts, DeadlineInfo, PendingTask, PrdSummary, ProjectInfo,
    };
    use super::*;

    #[test]
    fn test_progress_bar_0_percent() {
        let bar = progress_bar(0.0);
        assert_eq!(bar, "[--------------------] 0.0%");
    }

    #[test]
    fn test_progress_bar_50_percent() {
        let bar = progress_bar(50.0);
        assert_eq!(bar, "[##########----------] 50.0%");
    }

    #[test]
    fn test_progress_bar_100_percent() {
        let bar = progress_bar(100.0);
        assert_eq!(bar, "[####################] 100.0%");
    }

    #[test]
    fn test_status_icon_complete() {
        assert_eq!(status_icon(100.0), "[DONE]");
    }

    #[test]
    fn test_status_icon_partial() {
        assert_eq!(status_icon(50.0), "[....]");
        assert_eq!(status_icon(25.0), "[....]");
    }

    #[test]
    fn test_status_icon_empty() {
        assert_eq!(status_icon(0.0), "[    ]");
    }

    #[test]
    fn test_format_text_with_data() {
        let result = DashboardResult {
            project: Some(ProjectInfo {
                name: "my-project".to_string(),
                branch: Some("feat/cool".to_string()),
                description: Some("A description".to_string()),
            }),
            tasks: DashboardTaskCounts {
                total: 10,
                done: 5,
                todo: 3,
                in_progress: 1,
                blocked: 1,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 50.0,
            deadline: None,
            pending_tasks: vec![],
            prd_summaries: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("my-project"));
        assert!(text.contains("feat/cool"));
        assert!(text.contains("5/10 tasks"));
        assert!(text.contains("50.0%"));
        assert!(text.contains("done:"));
        assert!(text.contains("todo:"));
    }

    #[test]
    fn test_format_text_no_project() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 0,
                done: 0,
                todo: 0,
                in_progress: 0,
                blocked: 0,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            deadline: None,
            pending_tasks: vec![],
            prd_summaries: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("No project initialized"));
    }

    #[test]
    fn test_format_text_with_deadline() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 0,
                done: 0,
                todo: 0,
                in_progress: 0,
                blocked: 0,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            deadline: Some(DeadlineInfo {
                prd_basename: "test-prd".to_string(),
                expired: false,
                seconds_remaining: 3600,
                time_remaining: "1h 0m remaining".to_string(),
            }),
            pending_tasks: vec![],
            prd_summaries: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("1h 0m remaining"));
        assert!(text.contains("test-prd"));
    }

    #[test]
    fn test_format_text_with_expired_deadline() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 0,
                done: 0,
                todo: 0,
                in_progress: 0,
                blocked: 0,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            deadline: Some(DeadlineInfo {
                prd_basename: "test-prd".to_string(),
                expired: true,
                seconds_remaining: 0,
                time_remaining: "expired".to_string(),
            }),
            pending_tasks: vec![],
            prd_summaries: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("EXPIRED"));
    }

    #[test]
    fn test_format_text_with_pending_tasks() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 3,
                done: 0,
                todo: 1,
                in_progress: 1,
                blocked: 1,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 0.0,
            deadline: None,
            pending_tasks: vec![
                PendingTask {
                    id: "T-001".to_string(),
                    title: "In progress task".to_string(),
                    priority: 10,
                    status: "in_progress".to_string(),
                },
                PendingTask {
                    id: "T-002".to_string(),
                    title: "Todo task".to_string(),
                    priority: 20,
                    status: "todo".to_string(),
                },
                PendingTask {
                    id: "T-003".to_string(),
                    title: "Blocked task".to_string(),
                    priority: 30,
                    status: "blocked".to_string(),
                },
            ],
            prd_summaries: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("Pending tasks:"));
        assert!(text.contains("> [P10] T-001"));
        assert!(text.contains("  [P20] T-002"));
        assert!(text.contains("! [P30] T-003"));
    }

    #[test]
    fn test_format_text_shows_prd_summaries() {
        let result = DashboardResult {
            project: None,
            tasks: DashboardTaskCounts {
                total: 4,
                done: 2,
                todo: 1,
                in_progress: 1,
                blocked: 0,
                skipped: 0,
                irrelevant: 0,
            },
            completion_percentage: 50.0,
            deadline: None,
            pending_tasks: vec![],
            prd_summaries: vec![
                PrdSummary {
                    prefix: "abc123".to_string(),
                    total: 2,
                    done: 1,
                    in_progress: 0,
                    completion_pct: 50.0,
                    active_lock: false,
                },
                PrdSummary {
                    prefix: "def456".to_string(),
                    total: 2,
                    done: 1,
                    in_progress: 1,
                    completion_pct: 50.0,
                    active_lock: true,
                },
            ],
        };

        let text = format_text(&result);
        assert!(text.contains("PRD Summaries:"));
        assert!(text.contains("abc123"));
        assert!(text.contains("def456"));
        assert!(text.contains("[ACTIVE]"));
    }
}
