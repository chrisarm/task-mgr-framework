//! Export database state to JSON PRD format.
//!
//! This module implements the `export` command which exports the database
//! state back to JSON PRD format, enabling round-trip fidelity.
//!
//! # Security Considerations
//!
//! The `--to-json` path is a CLI argument provided directly by the user running
//! the command. This is trusted input because:
//! - The user explicitly specifies the destination path
//! - The command runs with the user's filesystem permissions
//! - Path validation would prevent legitimate use cases
//!
//! Unlike `touchesFiles` in PRD input (validated in init.rs), CLI output paths
//! are not validated for traversal since the user controls both the command
//! invocation and the destination.

mod prd;
mod progress;

#[cfg(test)]
mod tests;

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::db::open_connection;
use crate::{TaskMgrError, TaskMgrResult};

// Re-export public types
pub use prd::{ExportedPrd, ExportedUserStory};

use prd::{load_prd_metadata, load_tasks};
use progress::{export_progress, load_learnings};

/// Result of the export command.
#[derive(Debug, Serialize)]
pub struct ExportResult {
    /// Path to the exported PRD JSON file
    pub prd_file: String,
    /// Number of tasks exported
    pub tasks_exported: usize,
    /// Path to the progress.json file (if --with-progress)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_file: Option<String>,
    /// Path to the learnings file (if --learnings-file)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learnings_file: Option<String>,
    /// Number of learnings exported
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learnings_exported: Option<usize>,
    /// Number of runs exported
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs_exported: Option<usize>,
}

/// Export the database state to JSON PRD format.
///
/// # Arguments
///
/// * `dir` - Directory containing database files
/// * `to_json` - Path to write the PRD JSON file
/// * `with_progress` - If true, also export progress.json
/// * `learnings_file` - Optional path to export learnings separately
///
/// # Returns
///
/// Returns an `ExportResult` with export statistics.
pub fn export(
    dir: &Path,
    to_json: &Path,
    with_progress: bool,
    learnings_file: Option<&Path>,
) -> TaskMgrResult<ExportResult> {
    let conn = open_connection(dir)?;

    // Load PRD metadata
    let metadata = load_prd_metadata(&conn)?;

    // Load all tasks ordered by ID for determinism
    let tasks = load_tasks(&conn)?;
    let tasks_exported = tasks.len();

    // Build the exported PRD
    let prd = ExportedPrd {
        project: metadata.project,
        branch_name: metadata.branch_name,
        description: metadata.description,
        priority_philosophy: metadata.priority_philosophy,
        global_acceptance_criteria: metadata.global_acceptance_criteria,
        review_guidelines: metadata.review_guidelines,
        model: metadata.default_model,
        user_stories: tasks,
    };

    // Write PRD with atomic file operation
    write_json_atomic(to_json, &prd)?;

    let mut result = ExportResult {
        prd_file: to_json.display().to_string(),
        tasks_exported,
        progress_file: None,
        learnings_file: None,
        learnings_exported: None,
        runs_exported: None,
    };

    // Export progress.json if requested
    if with_progress {
        let progress_path = to_json.with_file_name("progress.json");
        let (runs_exported, learnings_exported) = export_progress(&conn, dir, &progress_path)?;
        result.progress_file = Some(progress_path.display().to_string());
        result.runs_exported = Some(runs_exported);
        result.learnings_exported = Some(learnings_exported);
    }

    // Export learnings to separate file if requested
    if let Some(learnings_path) = learnings_file {
        let learnings = load_learnings(&conn)?;
        let count = learnings.len();
        write_json_atomic(learnings_path, &learnings)?;
        result.learnings_file = Some(learnings_path.display().to_string());
        if result.learnings_exported.is_none() {
            result.learnings_exported = Some(count);
        }
    }

    Ok(result)
}

/// Write JSON to a file atomically (write to .tmp then rename).
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, data: &T) -> TaskMgrResult<()> {
    let tmp_path = path.with_extension("json.tmp");

    // Serialize with pretty formatting
    let json = serde_json::to_string_pretty(data)?;

    // Write to temp file
    let mut file = File::create(&tmp_path).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to create temp file {}: {}", tmp_path.display(), e),
        ))
    })?;

    file.write_all(json.as_bytes()).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to write to {}: {}", tmp_path.display(), e),
        ))
    })?;

    file.sync_all().map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to sync {}: {}", tmp_path.display(), e),
        ))
    })?;

    // Atomic rename
    fs::rename(&tmp_path, path).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to rename {} to {}: {}",
                tmp_path.display(),
                path.display(),
                e
            ),
        ))
    })?;

    Ok(())
}

/// Format export result for text output.
pub fn format_text(result: &ExportResult) -> String {
    let mut output = String::new();

    output.push_str(&format!("Exported PRD to: {}\n", result.prd_file));
    output.push_str(&format!("Tasks exported: {}\n", result.tasks_exported));

    if let Some(ref progress_file) = result.progress_file {
        output.push_str(&format!("\nProgress exported to: {}\n", progress_file));
        if let Some(runs) = result.runs_exported {
            output.push_str(&format!("Runs exported: {}\n", runs));
        }
    }

    if let Some(ref learnings_file) = result.learnings_file {
        output.push_str(&format!("\nLearnings exported to: {}\n", learnings_file));
    }

    if let Some(learnings) = result.learnings_exported {
        output.push_str(&format!("Learnings exported: {}\n", learnings));
    }

    output
}
