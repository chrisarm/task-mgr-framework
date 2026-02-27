//! Initialize database from JSON PRD file(s).
//!
//! This module implements the `init` command which imports task data from
//! JSON PRD files into the SQLite database.
//!
//! # Security Considerations
//!
//! ## Trusted vs Untrusted Input
//!
//! - **`--from-json` path (trusted)**: CLI argument from the user running the command.
//!   The user controls which file to import and has filesystem permissions to read it.
//!   No validation is performed on this path.
//!
//! - **`touchesFiles` in PRD (untrusted)**: Paths embedded in the PRD JSON content.
//!   PRD files may come from external sources (shared repos, downloaded files).
//!   These paths are validated to prevent path traversal attacks.
//!
//! ## Path Traversal Protection
//!
//! The `touchesFiles` array in each user story is validated using [`validate_safe_path`].
//! Rejected patterns:
//! - Absolute paths (`/etc/passwd`, `C:\Windows`)
//! - Parent directory traversal (`../../../etc/passwd`)
//! - Home directory expansion (`~/.ssh/id_rsa`)
//! - Network paths (`\\server\share`, `//server/share`)
//!
//! This protects against malicious PRD files that could:
//! - Reference sensitive system files
//! - Escape the project directory
//! - Access network resources
//!
//! [`validate_safe_path`]: crate::error::validate_safe_path

pub mod import;
pub mod output;
pub mod parse;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::Path;

use crate::db::open_connection;
use crate::error::validate_safe_path;
use crate::TaskMgrError;
use crate::TaskMgrResult;

// Re-export public types
pub use output::{format_init_verbose, format_text, DryRunDeletePreview, InitResult};
pub use parse::{PrdFile, PrdUserStory};

/// Controls how task ID prefixing behaves during import.
#[derive(Debug, Clone)]
pub enum PrefixMode {
    /// Use JSON `taskPrefix` field, or auto-generate a deterministic hash if absent.
    /// The hash is derived from `md5(branchName + ":" + filename)[..8]`.
    /// Auto-generated prefixes are written back to the JSON file for stability.
    Auto,
    /// Use this explicit prefix (overrides JSON field).
    Explicit(String),
    /// No prefix — import task IDs exactly as they appear in the JSON.
    Disabled,
}

/// Apply a prefix to a single task ID.
fn prefix_id(prefix: &str, id: &str) -> String {
    format!("{}-{}", prefix, id)
}

/// Apply a prefix to all IDs and cross-references in a story.
fn prefix_story(prefix: &str, story: &mut PrdUserStory) {
    story.id = prefix_id(prefix, &story.id);
    story.depends_on = story
        .depends_on
        .iter()
        .map(|d| prefix_id(prefix, d))
        .collect();
    story.synergy_with = story
        .synergy_with
        .iter()
        .map(|s| prefix_id(prefix, s))
        .collect();
    story.batch_with = story
        .batch_with
        .iter()
        .map(|b| prefix_id(prefix, b))
        .collect();
    story.conflicts_with = story
        .conflicts_with
        .iter()
        .map(|c| prefix_id(prefix, c))
        .collect();
}

/// Write an auto-generated `taskPrefix` back to the PRD JSON file.
/// Uses `serde_json::Value` to preserve existing formatting and unknown fields.
fn write_prefix_to_json(json_path: &Path, prefix: &str) -> TaskMgrResult<()> {
    let content = std::fs::read_to_string(json_path).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to read {} for prefix write-back: {}",
                json_path.display(),
                e
            ),
        ))
    })?;

    let mut value: serde_json::Value = serde_json::from_str(&content)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "taskPrefix".to_string(),
            serde_json::Value::String(prefix.to_string()),
        );
    }

    let output = serde_json::to_string_pretty(&value)?;
    std::fs::write(json_path, format!("{}\n", output)).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to write prefix to {}: {}", json_path.display(), e),
        ))
    })?;

    Ok(())
}

/// Generate a deterministic 8-char hex prefix from branch name and filename.
///
/// Formula: `md5(branch_name + ":" + filename)[..8]`
///
/// When `branch_name` is `None` or empty, the hash input is `":" + filename`,
/// which is still deterministic per-file.
fn generate_prefix(branch_name: Option<&str>, filename: &str) -> String {
    let branch = match branch_name {
        Some(b) if !b.is_empty() => b,
        _ => "",
    };
    let input = format!("{}:{}", branch, filename);
    let digest = md5::compute(input.as_bytes());
    format!("{:x}", digest)[..8].to_string()
}

use import::{
    delete_task_files, delete_task_relationships, drop_existing_data, get_delete_preview,
    get_existing_task_ids, insert_prd_metadata, insert_task, insert_task_file,
    insert_task_relationships, is_fresh_database, register_prd_files, update_task,
};

/// Initialize the database from JSON PRD file(s).
///
/// # Arguments
///
/// * `dir` - Directory for database files
/// * `json_files` - Path(s) to JSON PRD file(s)
/// * `force` - If true, drop existing data before import
/// * `append` - If true, add to existing data (for multi-phase projects)
/// * `update_existing` - If true with append, update existing tasks
/// * `dry_run` - If true, preview changes without making them
/// * `prefix_mode` - Controls task ID prefixing behavior
///
/// # Returns
///
/// Returns an `InitResult` with import statistics.
///
/// # Errors
///
/// Returns an error if:
/// - Any JSON file cannot be read or parsed
/// - Database operations fail
/// - Duplicate task IDs are found across files (when not in append mode)
/// - Cross-file dependencies reference non-existent tasks
pub fn init(
    dir: &Path,
    json_files: &[impl AsRef<Path>],
    force: bool,
    append: bool,
    update_existing: bool,
    dry_run: bool,
    prefix_mode: PrefixMode,
) -> TaskMgrResult<InitResult> {
    // Open/create database connection (schema created automatically)
    let mut conn = open_connection(dir)?;

    // Run pending migrations (e.g. v4 adds external_git_repo column)
    crate::db::run_migrations(&mut conn)?;

    // For dry-run with force, collect what would be deleted
    let would_delete = if dry_run && force {
        Some(get_delete_preview(&conn)?)
    } else {
        None
    };

    // Handle force flag - drop existing data (skip in dry-run mode)
    if force && !dry_run {
        drop_existing_data(&conn)?;
    }

    // Check if this is a fresh import
    // In dry-run mode with force, it WOULD be fresh after deletion
    let fresh_import = if dry_run && force {
        true
    } else {
        is_fresh_database(&conn)?
    };

    // Parse all JSON files first to collect all stories
    let mut all_stories: Vec<PrdUserStory> = Vec::new();
    let mut stories_to_update: Vec<PrdUserStory> = Vec::new();
    let mut prd_metadata: Option<PrdFile> = None;
    let mut raw_json: Option<String> = None;
    let mut existing_ids: HashSet<String> = HashSet::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut tasks_skipped = 0;
    let mut resolved_prefix: Option<String> = None;
    let mut json_file_registrations: Vec<(std::path::PathBuf, PrdFile)> = Vec::new();

    // Get existing task IDs if in append mode
    if append && !fresh_import {
        existing_ids = get_existing_task_ids(&conn)?;
    }

    // Collect all task IDs that will exist after import (for dependency validation)
    let mut all_task_ids: HashSet<String> = existing_ids.clone();

    for (file_idx, json_path) in json_files.iter().enumerate() {
        let json_path = json_path.as_ref();
        let content = std::fs::read_to_string(json_path).map_err(|e| {
            TaskMgrError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to read {}: {}", json_path.display(), e),
            ))
        })?;

        let prd: PrdFile = serde_json::from_str(&content)?;

        // Resolve prefix from first PRD file (CLI override > JSON field > auto-generate)
        if file_idx == 0 {
            resolved_prefix = match &prefix_mode {
                PrefixMode::Disabled => None,
                PrefixMode::Explicit(p) => Some(p.clone()),
                PrefixMode::Auto => {
                    if let Some(ref p) = prd.task_prefix {
                        Some(p.clone())
                    } else {
                        let filename = json_path
                            .file_name()
                            .and_then(|f| f.to_str())
                            .unwrap_or("unknown.json");
                        let generated = generate_prefix(prd.branch_name.as_deref(), filename);
                        if !dry_run {
                            write_prefix_to_json(json_path, &generated)?;
                        }
                        warnings.push(format!(
                            "Auto-generated taskPrefix '{}' (written to {})",
                            generated,
                            json_path.display()
                        ));
                        Some(generated)
                    }
                }
            };
        }

        // Process stories — collect raw IDs first for duplicate checking,
        // then apply prefix after all validation
        for story in &prd.user_stories {
            // Track all IDs for dependency validation (with prefix applied)
            let effective_id = if let Some(ref pfx) = resolved_prefix {
                prefix_id(pfx, &story.id)
            } else {
                story.id.clone()
            };
            all_task_ids.insert(effective_id.clone());

            if append && existing_ids.contains(&effective_id) {
                if update_existing {
                    let mut s = story.clone();
                    if let Some(ref pfx) = resolved_prefix {
                        prefix_story(pfx, &mut s);
                    }
                    stories_to_update.push(s);
                } else {
                    warnings.push(format!("Skipping existing task: {}", effective_id));
                    tasks_skipped += 1;
                }
                continue;
            }

            // Check for duplicates within the files being imported
            let already_imported = all_stories.iter().any(|s| s.id == effective_id);
            if already_imported {
                return Err(TaskMgrError::InvalidState {
                    resource_type: "Task".to_string(),
                    id: effective_id,
                    expected: "Unique task IDs across all files".to_string(),
                    actual: "Duplicate ID found in multiple files".to_string(),
                });
            }
        }

        // Store first PRD's metadata
        if prd_metadata.is_none() {
            raw_json = Some(content);
            prd_metadata = Some(PrdFile {
                project: prd.project.clone(),
                branch_name: prd.branch_name.clone(),
                description: prd.description.clone(),
                priority_philosophy: prd.priority_philosophy.clone(),
                global_acceptance_criteria: prd.global_acceptance_criteria.clone(),
                review_guidelines: prd.review_guidelines.clone(),
                user_stories: Vec::new(), // We'll collect stories separately
                external_git_repo: prd.external_git_repo.clone(),
                task_prefix: resolved_prefix.clone().or(prd.task_prefix.clone()),
                prd_file: prd.prd_file.clone(),
                model: prd.model.clone(),
            });
        }

        // Track JSON file + PRD for prd_files registration
        json_file_registrations.push((
            json_path.to_path_buf(),
            PrdFile {
                project: prd.project.clone(),
                branch_name: None,
                description: None,
                priority_philosophy: None,
                global_acceptance_criteria: None,
                review_guidelines: None,
                user_stories: Vec::new(),
                external_git_repo: None,
                task_prefix: None,
                prd_file: prd.prd_file.clone(),
                model: None,
            },
        ));

        // Collect new stories (with prefix applied)
        for story in prd.user_stories {
            let effective_id = if let Some(ref pfx) = resolved_prefix {
                prefix_id(pfx, &story.id)
            } else {
                story.id.clone()
            };

            if append && existing_ids.contains(&effective_id) {
                continue;
            }

            let mut s = story;
            if let Some(ref pfx) = resolved_prefix {
                prefix_story(pfx, &mut s);
            }
            all_stories.push(s);
        }
    }

    // Validate cross-file dependencies (dependsOn must reference valid task IDs)
    for story in all_stories.iter().chain(stories_to_update.iter()) {
        for dep in &story.depends_on {
            if !all_task_ids.contains(dep) {
                return Err(TaskMgrError::InvalidState {
                    resource_type: "Task".to_string(),
                    id: story.id.clone(),
                    expected: format!("dependsOn task '{}' to exist", dep),
                    actual: "Referenced task not found in any input file or database".to_string(),
                });
            }
        }
    }

    // SECURITY: Validate touchesFiles paths to prevent path traversal attacks
    for story in all_stories.iter().chain(stories_to_update.iter()) {
        for file_path in &story.touches_files {
            validate_safe_path(file_path, "touchesFiles", Some(&story.id))?;
        }
    }

    // Calculate counts for result (both for dry-run preview and actual import)
    let mut tasks_imported = 0;
    let mut tasks_updated = 0;
    let mut files_imported = 0;
    let mut relationships_imported = 0;

    for story in &all_stories {
        tasks_imported += 1;
        files_imported += story.touches_files.len();
        relationships_imported += story.depends_on.len()
            + story.synergy_with.len()
            + story.batch_with.len()
            + story.conflicts_with.len();
    }

    for story in &stories_to_update {
        tasks_updated += 1;
        files_imported += story.touches_files.len();
        relationships_imported += story.depends_on.len()
            + story.synergy_with.len()
            + story.batch_with.len()
            + story.conflicts_with.len();
    }

    // In dry-run mode, skip the actual database modifications
    if dry_run {
        return Ok(InitResult {
            tasks_imported,
            tasks_updated,
            tasks_skipped,
            files_imported,
            relationships_imported,
            fresh_import,
            warnings,
            dry_run: true,
            would_delete,
            prefix_applied: resolved_prefix,
        });
    }

    // Wrap all data imports in a transaction for atomicity
    let tx = conn.transaction()?;

    // Insert PRD metadata and get the upserted row id for prd_files linking
    let prd_id = if let Some(metadata) = prd_metadata {
        insert_prd_metadata(&tx, &metadata, raw_json.as_deref())?
    } else {
        1 // fallback: no metadata parsed (shouldn't happen in practice)
    };

    // Register PRD files for archive discovery
    let tasks_dir = dir.join("tasks");
    for (json_path, prd_for_reg) in &json_file_registrations {
        register_prd_files(&tx, prd_id, json_path, prd_for_reg, &tasks_dir)?;
    }

    // Import new tasks
    for story in &all_stories {
        insert_task(&tx, story)?;

        for file_path in &story.touches_files {
            insert_task_file(&tx, &story.id, file_path)?;
        }

        insert_task_relationships(&tx, story)?;
    }

    // Update existing tasks if --update-existing
    for story in &stories_to_update {
        update_task(&tx, story)?;

        if story.passes {
            let current_status: String = tx
                .query_row(
                    "SELECT status FROM tasks WHERE id = ?",
                    [&story.id],
                    |row| row.get(0),
                )
                .unwrap_or_default();
            if current_status != "done" {
                tx.execute(
                    "UPDATE tasks SET status = 'done', updated_at = datetime('now') WHERE id = ?",
                    [&story.id],
                )?;
            }
        }

        delete_task_files(&tx, &story.id)?;
        delete_task_relationships(&tx, &story.id)?;

        for file_path in &story.touches_files {
            insert_task_file(&tx, &story.id, file_path)?;
        }

        insert_task_relationships(&tx, story)?;
    }

    tx.commit()?;

    Ok(InitResult {
        tasks_imported,
        tasks_updated,
        tasks_skipped,
        files_imported,
        relationships_imported,
        fresh_import,
        warnings,
        dry_run: false,
        would_delete: None,
        prefix_applied: resolved_prefix,
    })
}
