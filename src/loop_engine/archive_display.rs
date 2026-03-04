//! Display formatting for archive command results.
//!
//! Separated from `archive.rs` to keep the core archive pipeline (run_archive,
//! learning extraction, discovery/clearing) cohesive while isolating
//! human-readable formatting.

use super::archive::ArchiveResult;

/// Format archive result as human-readable text.
pub fn format_text(result: &ArchiveResult) -> String {
    let mut out = String::new();

    if result.dry_run {
        out.push_str("=== Archive Dry Run ===\n\n");
    } else {
        out.push_str("=== Archive Results ===\n\n");
    }

    if result.archived.is_empty() {
        out.push_str(&format!("{}\n", result.message));
        return out;
    }

    let action = if result.dry_run {
        "Would move"
    } else {
        "Moved"
    };

    for item in &result.archived {
        out.push_str(&format!(
            "  {} {} -> {}\n",
            action, item.source, item.destination
        ));
    }

    out.push('\n');

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

    if result.tasks_cleared > 0 {
        let verb = if result.dry_run {
            "Would clear"
        } else {
            "Cleared"
        };
        out.push_str(&format!(
            "{} {} task(s) from database (learnings preserved)\n",
            verb, result.tasks_cleared
        ));
    }

    out.push_str(&format!("\n{}\n", result.message));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::archive::ArchivedItem;

    #[test]
    fn test_format_text_dry_run() {
        let result = ArchiveResult {
            archived: vec![ArchivedItem {
                source: "my-project.json".to_string(),
                destination: "archive/2026-02-05-feature/my-project.json".to_string(),
            }],
            learnings_extracted: 2,
            tasks_cleared: 3,
            dry_run: true,
            message:
                "Would archive 1 file(s) to archive/2026-02-05-feature. 2 learning(s) extracted."
                    .to_string(),
        };

        let text = format_text(&result);
        assert!(text.contains("Dry Run"));
        assert!(text.contains("Would move"));
        assert!(text.contains("Would extract 2 learning(s)"));
    }

    #[test]
    fn test_format_text_actual_run() {
        let result = ArchiveResult {
            archived: vec![ArchivedItem {
                source: "my-project.json".to_string(),
                destination: "archive/2026-02-05-feature/my-project.json".to_string(),
            }],
            learnings_extracted: 0,
            tasks_cleared: 0,
            dry_run: false,
            message: "Archived 1 file(s) to archive/2026-02-05-feature. 0 learning(s) extracted."
                .to_string(),
        };

        let text = format_text(&result);
        assert!(text.contains("Archive Results"));
        assert!(text.contains("Moved"));
        assert!(!text.contains("Dry Run"));
    }

    #[test]
    fn test_format_text_empty() {
        let result = ArchiveResult {
            archived: Vec::new(),
            learnings_extracted: 0,
            tasks_cleared: 0,
            dry_run: false,
            message: "No archivable files found.".to_string(),
        };

        let text = format_text(&result);
        assert!(text.contains("No archivable files found."));
    }
}
