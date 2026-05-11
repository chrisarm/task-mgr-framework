//! Output types and formatting for the init command.
//!
//! This module contains the result structures returned by the init command
//! and formatting functions for verbose output.

use serde::Serialize;

/// Result of the init command.
#[derive(Debug, Serialize)]
pub struct InitResult {
    /// Number of tasks imported (new)
    pub tasks_imported: usize,
    /// Number of tasks updated (when --update-existing)
    pub tasks_updated: usize,
    /// Number of tasks skipped (existing, not updated)
    pub tasks_skipped: usize,
    /// Number of task files imported
    pub files_imported: usize,
    /// Number of relationships imported
    pub relationships_imported: usize,
    /// Whether this was a fresh import or update
    pub fresh_import: bool,
    /// Warning messages (e.g., duplicate tasks skipped)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Whether this was a dry run (no changes made)
    pub dry_run: bool,
    /// Preview of what would be deleted (only populated in dry-run mode with --force)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub would_delete: Option<DryRunDeletePreview>,
    /// The prefix that was applied to task IDs, if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_applied: Option<String>,
    /// Whether the `.task-mgr/` directory was newly created by `init_project`
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub created_dirs: bool,
    /// Whether `config.json` was newly created by `init_project`
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub created_config: bool,
}

/// Preview of what would be deleted in dry-run mode.
#[derive(Debug, Serialize)]
pub struct DryRunDeletePreview {
    /// Number of tasks that would be deleted
    pub tasks: usize,
    /// Number of files that would be deleted
    pub files: usize,
    /// Number of relationships that would be deleted
    pub relationships: usize,
    /// Number of learnings that would be deleted
    pub learnings: usize,
    /// Number of runs that would be deleted
    pub runs: usize,
}

/// Formats the init result for text output.
#[must_use]
pub fn format_text(result: &InitResult) -> String {
    let mut output = String::new();

    if result.dry_run {
        output.push_str("[DRY RUN] Preview of changes:\n");
        if let Some(ref preview) = result.would_delete {
            output.push_str(&format!(
                "  Would delete: {} tasks, {} files, {} relationships, {} learnings, {} runs\n",
                preview.tasks,
                preview.files,
                preview.relationships,
                preview.learnings,
                preview.runs
            ));
        }
        output.push_str(&format!(
            "  Would import: {} tasks, {} files, {} relationships\n",
            result.tasks_imported, result.files_imported, result.relationships_imported
        ));
    } else {
        if result.created_dirs {
            output.push_str("Created: .task-mgr/\n");
        }
        if result.created_config {
            output.push_str("Created: .task-mgr/config.json\n");
        }
        if let Some(ref prefix) = result.prefix_applied {
            output.push_str(&format!("Prefix: {}-\n", prefix));
        }
        output.push_str(&format!(
            "Initialized: {} tasks, {} files, {} relationships\n",
            result.tasks_imported, result.files_imported, result.relationships_imported
        ));
        if result.tasks_updated > 0 {
            output.push_str(&format!("Updated: {} tasks\n", result.tasks_updated));
        }
        if result.tasks_skipped > 0 {
            output.push_str(&format!(
                "Skipped: {} existing tasks\n",
                result.tasks_skipped
            ));
        }
        if !result.warnings.is_empty() {
            output.push_str("\nWarnings:\n");
            for warning in &result.warnings {
                output.push_str(&format!("  - {}\n", warning));
            }
        }
    }

    output
}

/// Formats verbose output for the init command (to stderr).
///
/// Returns a string that should be written to stderr when --verbose is enabled.
#[must_use]
pub fn format_init_verbose(result: &InitResult) -> String {
    let mut output = String::new();

    output.push_str("[verbose] Init Command Details:\n");
    output.push_str(&format!("{}\n", "-".repeat(50)));

    // Dry-run status
    if result.dry_run {
        output.push_str("  Mode: DRY RUN (no changes will be made)\n");
    } else {
        output.push_str("  Mode: ACTUAL IMPORT\n");
    }

    // Fresh vs append
    if result.fresh_import {
        output.push_str("  Import type: Fresh (new database)\n");
    } else {
        output.push_str("  Import type: Append/Update (existing database)\n");
    }

    output.push_str(&format!("{}\n", "-".repeat(50)));

    // What would be deleted (dry-run with --force)
    if let Some(ref preview) = result.would_delete {
        output.push_str("\n[verbose] Would delete (--force flag):\n");
        output.push_str(&format!("  Tasks: {}\n", preview.tasks));
        output.push_str(&format!("  Files: {}\n", preview.files));
        output.push_str(&format!("  Relationships: {}\n", preview.relationships));
        output.push_str(&format!("  Learnings: {}\n", preview.learnings));
        output.push_str(&format!("  Runs: {}\n", preview.runs));
    }

    // Import details
    output.push_str("\n[verbose] Import Summary:\n");
    output.push_str(&format!(
        "  Tasks imported (new): {}\n",
        result.tasks_imported
    ));
    output.push_str(&format!(
        "  Tasks updated (--update-existing): {}\n",
        result.tasks_updated
    ));
    output.push_str(&format!(
        "  Tasks skipped (existing): {}\n",
        result.tasks_skipped
    ));
    output.push_str(&format!("  Files imported: {}\n", result.files_imported));
    output.push_str(&format!(
        "  Relationships imported: {}\n",
        result.relationships_imported
    ));

    // Warnings
    if !result.warnings.is_empty() {
        output.push_str("\n[verbose] Warnings:\n");
        for warning in &result.warnings {
            output.push_str(&format!("  - {}\n", warning));
        }
    }

    output.push_str(&format!("\n{}\n", "-".repeat(50)));

    let total = result.tasks_imported + result.tasks_updated + result.tasks_skipped;
    output.push_str(&format!("[verbose] Total tasks processed: {}\n", total));

    output
}
