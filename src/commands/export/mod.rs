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

use crate::commands::context::{
    default_prd_roots, find_registered_by_path_identity, resolve_context_with_roots,
};
use crate::commands::prd_json::unique_tmp_path;
use crate::db::LockGuard;
use crate::db::open_and_migrate as open_connection;
use crate::{TaskMgrError, TaskMgrResult};

// Re-export public types
pub use prd::{ExportedPrd, ExportedUserStory};

use prd::{MetadataScope, load_prd_metadata, load_tasks};
use progress::{export_progress, load_learnings};

/// Library + CLI options for [`export`].
///
/// Scope fields (`from_json` / `all`) select the dump source (CONTRACT-001).
/// `force` opts into overwriting a registered `task_list` dest (CONTRACT-002).
pub struct ExportOpts<'a> {
    /// Write target for the PRD dump (`--to-json`). Never remapped.
    pub to_json: &'a Path,
    /// Also export `progress.json` next to the dest (DB-global).
    pub with_progress: bool,
    /// Optional separate learnings dump path (DB-global).
    pub learnings_file: Option<&'a Path>,
    /// Pin an already-registered effort as the dump **source** (not dest).
    pub from_json: Option<&'a Path>,
    /// Today's dump: all unarchived tasks + `prd_metadata ORDER BY id LIMIT 1`.
    pub all: bool,
    /// Opt-in overwrite of a registered `task_list` path (lossy dump, not merge).
    pub force: bool,
}

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
/// Dest overwrite-guard + `LockGuard` live here (after `dest.is_file()`), not
/// in `main.rs`. Scope selection follows CONTRACT-001; refuse-without-`--force`
/// follows CONTRACT-002.
pub fn export(dir: &Path, opts: &ExportOpts<'_>) -> TaskMgrResult<ExportResult> {
    let conn = open_connection(dir)?;
    let dest = opts.to_json;
    let (source_root, worktree_root) = default_prd_roots(dir);

    // Directory dest → error before dump; no lock.
    if dest.exists() && dest.is_dir() {
        return Err(TaskMgrError::invalid_state(
            "export",
            dest.display().to_string(),
            "a file path for --to-json",
            "path is a directory",
        ));
    }

    // Overwrite-guard: lock only when dest already exists as a file.
    // Refuse registered dest without --force BEFORE serializing tasks.
    let _lock = if dest.is_file() {
        let lock = LockGuard::acquire(dir)?;
        let dest_canon = fs::canonicalize(dest).map_err(|e| {
            TaskMgrError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to canonicalize {}: {}", dest.display(), e),
            ))
        })?;
        let registered = find_registered_by_path_identity(
            &conn,
            &dest_canon,
            Some(&source_root),
            Some(&worktree_root),
        )?;
        if registered.is_some() && !opts.force {
            return Err(TaskMgrError::invalid_state(
                "export",
                dest.display().to_string(),
                "pass --force to overwrite a registered task-list (export is a dump, not a merge)",
                "destination is a registered task_list",
            ));
        }
        Some(lock)
    } else {
        None
    };

    // Scope selection (CONTRACT-001).
    let (metadata, tasks) = if opts.all {
        (
            load_prd_metadata(&conn, MetadataScope::Unscoped)?,
            load_tasks(&conn, None)?,
        )
    } else if let Some(pin) = opts.from_json {
        let ctx = resolve_context_with_roots(
            &conn,
            Some(pin),
            "export",
            Some(&source_root),
            Some(&worktree_root),
        )?
        .ok_or_else(|| {
            // resolve_context returns Err for missing/directory/unregistered;
            // Ok(None) is only the no-flag probe path. Defensive.
            TaskMgrError::invalid_state(
                "export",
                "active PRD",
                "pin via --from-json, pass --all, or run task-mgr current",
                "no active PRD selected",
            )
        })?;
        if ctx.prefix.is_empty() {
            // Empty-prefix pin: all unarchived tasks; metadata by identity prd_id.
            let hit = find_registered_by_path_identity(
                &conn,
                &ctx.prd_json_path,
                Some(&source_root),
                Some(&worktree_root),
            )?
            .ok_or_else(|| {
                TaskMgrError::invalid_state(
                    "export",
                    pin.display().to_string(),
                    "registered task_list path (run task-mgr loop init first)",
                    "path identity miss after resolve",
                )
            })?;
            (
                load_prd_metadata(&conn, MetadataScope::ByPrdId(hit.0))?,
                load_tasks(&conn, None)?,
            )
        } else {
            (
                load_prd_metadata(&conn, MetadataScope::NamedPrefix(&ctx.prefix))?,
                load_tasks(&conn, Some(&ctx.prefix))?,
            )
        }
    } else {
        match resolve_context_with_roots(
            &conn,
            None,
            "export",
            Some(&source_root),
            Some(&worktree_root),
        )? {
            Some(ctx) if !ctx.prefix.is_empty() => (
                load_prd_metadata(&conn, MetadataScope::NamedPrefix(&ctx.prefix))?,
                load_tasks(&conn, Some(&ctx.prefix))?,
            ),
            _ => {
                return Err(TaskMgrError::invalid_state(
                    "export",
                    "active PRD",
                    "pin via --from-json, pass --all, or run task-mgr current",
                    "no active PRD selected",
                ));
            }
        }
    };

    let tasks_exported = tasks.len();

    // Build the exported PRD (lossy: no taskPrefix).
    let prd = ExportedPrd {
        project: metadata.project,
        branch_name: metadata.branch_name,
        description: metadata.description,
        priority_philosophy: metadata.priority_philosophy,
        global_acceptance_criteria: metadata.global_acceptance_criteria,
        review_guidelines: metadata.review_guidelines,
        model: metadata.default_model,
        default_max_retries: metadata.default_max_retries,
        user_stories: tasks,
    };

    // Write PRD with atomic file operation (unique_tmp + rename).
    write_json_atomic(dest, &prd)?;

    let mut result = ExportResult {
        prd_file: dest.display().to_string(),
        tasks_exported,
        progress_file: None,
        learnings_file: None,
        learnings_exported: None,
        runs_exported: None,
    };

    // Export progress.json if requested (DB-global).
    if opts.with_progress {
        let progress_path = dest.with_file_name("progress.json");
        let (runs_exported, learnings_exported) = export_progress(&conn, dir, &progress_path)?;
        result.progress_file = Some(progress_path.display().to_string());
        result.runs_exported = Some(runs_exported);
        result.learnings_exported = Some(learnings_exported);
    }

    // Export learnings to separate file if requested (DB-global).
    if let Some(learnings_path) = opts.learnings_file {
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

/// Write JSON to a file atomically (`prd_json::unique_tmp_path` then rename).
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, data: &T) -> TaskMgrResult<()> {
    let tmp_path = unique_tmp_path(path);

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
