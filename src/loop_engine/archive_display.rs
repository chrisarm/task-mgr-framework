//! Display formatting for archive command results.
//!
//! Separated from `archive.rs` to keep the core archive pipeline (run_archive,
//! learning extraction, discovery/clearing) cohesive while isolating
//! human-readable formatting.

use super::archive::{ArchiveResult, PrdArchiveSummary, PrdSkipReason};

/// Format archive result as human-readable text.
pub fn format_text(result: &ArchiveResult) -> String {
    let mut out = String::new();

    if result.dry_run {
        out.push_str("=== Archive Dry Run ===\n\n");
    } else {
        out.push_str("=== Archive Results ===\n\n");
    }

    let total_prds = result.prds_archived.len() + result.prds_skipped.len();

    // Empty: no PRDs at all
    if total_prds == 0 {
        out.push_str(&format!("{}\n", result.message));
        return out;
    }

    let move_verb = if result.dry_run {
        "Would move"
    } else {
        "Moved"
    };

    // Per-archived-PRD sections
    for summary in &result.prds_archived {
        format_archived_prd(&mut out, summary, result, move_verb);
    }

    // Per-skipped-PRD sections
    for skip in &result.prds_skipped {
        format_skipped_prd(&mut out, skip);
    }

    // Aggregate summary
    let archived_count = result.prds_archived.len();
    out.push_str(&format!(
        "Summary: {} of {} PRD(s) archived\n",
        archived_count, total_prds
    ));

    if result.learnings_extracted > 0 {
        let verb = if result.dry_run {
            "Would extract"
        } else {
            "Extracted"
        };
        out.push_str(&format!(
            "{} {} learning(s) to learnings.md\n",
            verb, result.learnings_extracted
        ));
    }

    out
}

fn format_archived_prd(
    out: &mut String,
    summary: &PrdArchiveSummary,
    result: &ArchiveResult,
    move_verb: &str,
) {
    out.push_str(&format!(
        "[PRD: {} (prefix: {})]\n",
        summary.project, summary.task_prefix
    ));
    out.push_str(&format!("  Archive folder: {}\n", summary.archive_folder));

    // Collect files belonging to this PRD by matching destination path
    let prd_files: Vec<_> = result
        .archived
        .iter()
        .filter(|item| item.destination.contains(summary.archive_folder.as_str()))
        .collect();

    for item in &prd_files {
        out.push_str(&format!(
            "  {} {} -> {}\n",
            move_verb, item.source, item.destination
        ));
    }

    let clear_verb = if result.dry_run {
        "Would clear"
    } else {
        "Cleared"
    };
    if summary.tasks_cleared > 0 {
        out.push_str(&format!(
            "  {} {} task(s) from database\n",
            clear_verb, summary.tasks_cleared
        ));
    }

    out.push('\n');
}

fn format_skipped_prd(out: &mut String, skip: &PrdSkipReason) {
    out.push_str(&format!("[PRD: {}]\n", skip.project));
    out.push_str(&format!("  Skipped: {}\n\n", skip.reason));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::archive::{ArchivedItem, PrdArchiveSummary, PrdSkipReason};

    fn make_result(
        archived: Vec<ArchivedItem>,
        prds_archived: Vec<PrdArchiveSummary>,
        prds_skipped: Vec<PrdSkipReason>,
        dry_run: bool,
        message: &str,
        learnings: usize,
    ) -> ArchiveResult {
        ArchiveResult {
            tasks_cleared: prds_archived.iter().map(|p| p.tasks_cleared).sum(),
            archived,
            learnings_extracted: learnings,
            dry_run,
            message: message.to_string(),
            prds_archived,
            prds_skipped,
        }
    }

    #[test]
    fn test_format_text_empty_no_prds() {
        let result = make_result(vec![], vec![], vec![], false, "No PRD metadata found.", 0);
        let text = format_text(&result);
        assert!(text.contains("Archive Results"));
        assert!(text.contains("No PRD metadata found."));
    }

    #[test]
    fn test_format_text_dry_run_header() {
        let result = make_result(
            vec![ArchivedItem {
                source: "my-project.json".to_string(),
                destination: "archive/2026-02-05-my-project/my-project.json".to_string(),
            }],
            vec![PrdArchiveSummary {
                prd_id: 1,
                project: "my-project".to_string(),
                task_prefix: "PA".to_string(),
                archive_folder: "2026-02-05-my-project".to_string(),
                files_archived: 1,
                tasks_cleared: 0,
            }],
            vec![],
            true,
            "",
            0,
        );
        let text = format_text(&result);
        assert!(text.contains("Dry Run"));
        assert!(text.contains("Would move"));
        assert!(!text.contains("Moved"));
    }

    #[test]
    fn test_format_text_archived_prd_shows_project_prefix_folder() {
        let result = make_result(
            vec![ArchivedItem {
                source: "my-project.json".to_string(),
                destination: "archive/2026-02-05-my-project/my-project.json".to_string(),
            }],
            vec![PrdArchiveSummary {
                prd_id: 1,
                project: "my-project".to_string(),
                task_prefix: "PA".to_string(),
                archive_folder: "2026-02-05-my-project".to_string(),
                files_archived: 1,
                tasks_cleared: 3,
            }],
            vec![],
            false,
            "",
            0,
        );
        let text = format_text(&result);
        assert!(text.contains("PRD: my-project (prefix: PA)"));
        assert!(text.contains("2026-02-05-my-project"));
        assert!(text.contains("Moved my-project.json"));
        assert!(text.contains("Cleared 3 task(s)"));
    }

    #[test]
    fn test_format_text_skipped_prd_shows_reason() {
        let result = make_result(
            vec![],
            vec![],
            vec![PrdSkipReason {
                prd_id: 2,
                project: "other-project".to_string(),
                reason: "incomplete (2 task(s) not in terminal state)".to_string(),
            }],
            false,
            "",
            0,
        );
        let text = format_text(&result);
        assert!(text.contains("PRD: other-project"));
        assert!(text.contains("Skipped: incomplete"));
    }

    #[test]
    fn test_format_text_aggregate_summary() {
        let result = make_result(
            vec![ArchivedItem {
                source: "p.json".to_string(),
                destination: "archive/2026-02-05-p/p.json".to_string(),
            }],
            vec![PrdArchiveSummary {
                prd_id: 1,
                project: "p".to_string(),
                task_prefix: "PA".to_string(),
                archive_folder: "2026-02-05-p".to_string(),
                files_archived: 1,
                tasks_cleared: 0,
            }],
            vec![PrdSkipReason {
                prd_id: 2,
                project: "q".to_string(),
                reason: "no prefix".to_string(),
            }],
            false,
            "",
            0,
        );
        let text = format_text(&result);
        assert!(text.contains("Summary: 1 of 2 PRD(s) archived"));
    }

    #[test]
    fn test_format_text_learnings_shown_when_nonzero() {
        let result = make_result(
            vec![ArchivedItem {
                source: "p.json".to_string(),
                destination: "archive/2026-02-05-p/p.json".to_string(),
            }],
            vec![PrdArchiveSummary {
                prd_id: 1,
                project: "p".to_string(),
                task_prefix: "PA".to_string(),
                archive_folder: "2026-02-05-p".to_string(),
                files_archived: 1,
                tasks_cleared: 0,
            }],
            vec![],
            false,
            "",
            5,
        );
        let text = format_text(&result);
        assert!(text.contains("Extracted 5 learning(s)"));
    }

    #[test]
    fn test_format_text_dry_run_learnings() {
        let result = make_result(
            vec![ArchivedItem {
                source: "p.json".to_string(),
                destination: "archive/2026-02-05-p/p.json".to_string(),
            }],
            vec![PrdArchiveSummary {
                prd_id: 1,
                project: "p".to_string(),
                task_prefix: "PA".to_string(),
                archive_folder: "2026-02-05-p".to_string(),
                files_archived: 1,
                tasks_cleared: 0,
            }],
            vec![],
            true,
            "",
            3,
        );
        let text = format_text(&result);
        assert!(text.contains("Would extract 3 learning(s)"));
    }

    // Legacy compatibility: existing tests that use the old ArchiveResult shape
    // (prds_archived/prds_skipped empty) still work via the empty-PRD path.
    #[test]
    fn test_format_text_legacy_empty_prds_list_falls_through_to_message() {
        let result = ArchiveResult {
            archived: vec![ArchivedItem {
                source: "my-project.json".to_string(),
                destination: "archive/2026-02-05-feature/my-project.json".to_string(),
            }],
            learnings_extracted: 2,
            tasks_cleared: 3,
            dry_run: true,
            message: "legacy message".to_string(),
            prds_archived: Vec::new(),
            prds_skipped: Vec::new(),
        };
        let text = format_text(&result);
        // With no per-PRD data, falls through to empty path showing message
        assert!(text.contains("legacy message"));
    }
}
