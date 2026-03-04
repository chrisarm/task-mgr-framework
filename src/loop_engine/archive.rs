//! Archive command for completed PRDs.
//!
//! Moves completed PRD files + associated prompt/markdown files to
//! `tasks/archive/YYYY-MM-DD-<branch>/`. Extracts learnings from
//! `progress.txt` and appends to `tasks/learnings.md`.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde::Serialize;

use crate::db::open_connection;
use crate::db::prefix::make_like_pattern;
use crate::TaskMgrResult;

pub use super::archive_display::format_text;

/// Result of the archive command.
#[derive(Debug, Serialize)]
pub struct ArchiveResult {
    /// Items that were archived (or would be, in dry_run mode)
    pub archived: Vec<ArchivedItem>,
    /// Learnings extracted from progress.txt
    pub learnings_extracted: usize,
    /// Number of tasks cleared from the database
    pub tasks_cleared: usize,
    /// Whether this was a dry run
    pub dry_run: bool,
    /// Human-readable message
    pub message: String,
    /// Per-PRD archive summaries for successfully archived PRDs
    pub prds_archived: Vec<PrdArchiveSummary>,
    /// Per-PRD skip reasons for PRDs that were not archived
    pub prds_skipped: Vec<PrdSkipReason>,
}

/// Summary of a single PRD that was successfully archived.
#[derive(Debug, Serialize)]
pub struct PrdArchiveSummary {
    /// Database ID of the PRD
    pub prd_id: i64,
    /// Project name
    pub project: String,
    /// Task prefix (e.g. "PA")
    pub task_prefix: String,
    /// Archive folder name (relative to tasks/archive/)
    pub archive_folder: String,
    /// Number of files moved to the archive folder
    pub files_archived: usize,
    /// Number of tasks cleared from the database
    pub tasks_cleared: usize,
}

/// Reason a PRD was skipped during archiving.
#[derive(Debug, Serialize)]
pub struct PrdSkipReason {
    /// Database ID of the PRD
    pub prd_id: i64,
    /// Project name
    pub project: String,
    /// Human-readable reason for skipping
    pub reason: String,
}

/// A single archived item (file moved or would-be-moved).
#[derive(Debug, Serialize)]
pub struct ArchivedItem {
    /// Source path (relative to tasks dir)
    pub source: String,
    /// Destination path (relative to tasks dir)
    pub destination: String,
}

/// Run the archive command.
///
/// Iterates all PRDs in prd_metadata. For each: skips PRDs with NULL
/// task_prefix or incomplete tasks; archives completed PRDs by moving their
/// files to `tasks/archive/YYYY-MM-DD-<branch>/` and clearing DB data.
/// Extracts learnings from `progress.txt` once after all PRDs are processed
/// (only when at least one PRD was archived). Never moves `progress.txt`.
pub fn run_archive(dir: &Path, dry_run: bool) -> TaskMgrResult<ArchiveResult> {
    let mut conn = open_connection(dir)?;

    let all_prds = query_all_prds(&conn)?;
    if all_prds.is_empty() {
        return Ok(ArchiveResult {
            archived: Vec::new(),
            learnings_extracted: 0,
            tasks_cleared: 0,
            dry_run,
            message: "No PRD metadata found in database.".to_string(),
            prds_archived: Vec::new(),
            prds_skipped: Vec::new(),
        });
    }

    let tasks_dir = dir.join("tasks");
    let date_str = Local::now().format("%Y-%m-%d").to_string();

    let mut archived_items: Vec<ArchivedItem> = Vec::new();
    let mut prds_archived: Vec<PrdArchiveSummary> = Vec::new();
    let mut prds_skipped: Vec<PrdSkipReason> = Vec::new();
    let mut total_tasks_cleared: usize = 0;

    for prd in &all_prds {
        // Skip PRDs with NULL task_prefix — cannot scope by prefix
        let prefix = match prd.task_prefix.as_deref() {
            None => {
                prds_skipped.push(PrdSkipReason {
                    prd_id: prd.id,
                    project: prd.project.clone(),
                    reason: "No task prefix — cannot determine completion".to_string(),
                });
                continue;
            }
            Some(p) => p,
        };

        // Skip incomplete PRDs
        if !is_prd_completed_by_prefix(&conn, prefix)? {
            prds_skipped.push(PrdSkipReason {
                prd_id: prd.id,
                project: prd.project.clone(),
                reason: "Not fully completed".to_string(),
            });
            continue;
        }

        // Derive per-PRD archive folder from the PRD's own branch name
        let branch_slug = strip_branch_prefix(&prd.branch.clone().unwrap_or_default());
        let archive_folder_name = if branch_slug.is_empty() {
            date_str.clone()
        } else {
            format!("{}-{}", date_str, branch_slug)
        };
        let archive_dir = tasks_dir.join("archive").join(&archive_folder_name);

        // Discover files for this PRD (progress.txt is handled separately, never moved)
        let files_to_archive = discover_archivable_files(&conn, &tasks_dir, prd.id, &prd.project)?;

        let mut prd_items: Vec<ArchivedItem> = Vec::new();
        for source in &files_to_archive {
            let file_name = source
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            // Never move progress.txt
            if file_name == "progress.txt" {
                continue;
            }

            let dest = archive_dir.join(&file_name);
            prd_items.push(ArchivedItem {
                source: source
                    .strip_prefix(&tasks_dir)
                    .unwrap_or(source)
                    .display()
                    .to_string(),
                destination: format!("archive/{}/{}", archive_folder_name, file_name),
            });

            if !dry_run {
                fs::create_dir_all(&archive_dir).map_err(|e| {
                    crate::TaskMgrError::io_error(
                        archive_dir.display().to_string(),
                        "creating archive directory",
                        e,
                    )
                })?;
                fs::rename(source, &dest).map_err(|e| {
                    crate::TaskMgrError::io_error(
                        source.display().to_string(),
                        "moving file to archive",
                        e,
                    )
                })?;
            }
        }

        // Count tasks scoped to this prefix before clearing
        let like_pattern = make_like_pattern(prefix);
        let task_count: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id LIKE ? ESCAPE '\\'",
                rusqlite::params![like_pattern],
                |row| row.get(0),
            )
            .map_err(crate::TaskMgrError::DatabaseError)?;

        if !dry_run {
            clear_prd_data(&mut conn, prd.id, prefix)?;
        }

        prds_archived.push(PrdArchiveSummary {
            prd_id: prd.id,
            project: prd.project.clone(),
            task_prefix: prefix.to_string(),
            archive_folder: archive_folder_name,
            files_archived: prd_items.len(),
            tasks_cleared: task_count,
        });
        total_tasks_cleared += task_count;
        archived_items.extend(prd_items);
    }

    // Extract learnings once, only when at least one PRD was archived
    let learnings_count = if !prds_archived.is_empty() {
        let progress_path = tasks_dir.join("progress.txt");
        if progress_path.exists() {
            let learnings = extract_learnings_from_progress(&progress_path)?;
            if !learnings.is_empty() && !dry_run {
                append_learnings_to_file(&tasks_dir.join("learnings.md"), &learnings)?;
            }
            learnings.len()
        } else {
            0
        }
    } else {
        0
    };

    let message = if prds_archived.is_empty() {
        // Keep "not fully completed" wording for backward-compatible messaging
        format!(
            "{} PRD(s) not fully completed. Nothing archived.",
            prds_skipped.len()
        )
    } else {
        let action = if dry_run { "Would archive" } else { "Archived" };
        format!(
            "{} {} PRD(s), {} file(s). {} learning(s) extracted. Cleared {} task(s) from database.",
            action,
            prds_archived.len(),
            archived_items.len(),
            learnings_count,
            total_tasks_cleared,
        )
    };

    Ok(ArchiveResult {
        archived: archived_items,
        learnings_extracted: learnings_count,
        tasks_cleared: total_tasks_cleared,
        dry_run,
        message,
        prds_archived,
        prds_skipped,
    })
}

/// A single row from the prd_metadata table.
pub struct PrdRecord {
    pub id: i64,
    pub project: String,
    pub branch: Option<String>,
    pub task_prefix: Option<String>,
}

/// Return all rows from prd_metadata ordered by id.
pub fn query_all_prds(conn: &rusqlite::Connection) -> TaskMgrResult<Vec<PrdRecord>> {
    let mut stmt = conn
        .prepare("SELECT id, project, branch_name, task_prefix FROM prd_metadata ORDER BY id")
        .map_err(crate::TaskMgrError::DatabaseError)?;

    let records = stmt
        .query_map([], |row| {
            Ok(PrdRecord {
                id: row.get(0)?,
                project: row.get(1)?,
                branch: row.get(2)?,
                task_prefix: row.get(3)?,
            })
        })
        .map_err(crate::TaskMgrError::DatabaseError)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(crate::TaskMgrError::DatabaseError)?;

    Ok(records)
}

/// Check if all tasks belonging to a specific prefix are in a terminal state.
///
/// A prefix is archivable when it has at least one task AND no tasks are
/// `todo`, `in_progress`, or `blocked`. Terminal states: `done`, `skipped`,
/// `irrelevant`. Uses a LIKE pattern with dash separator so prefix "P1" does
/// not match tasks belonging to prefix "P10".
fn is_prd_completed_by_prefix(conn: &rusqlite::Connection, prefix: &str) -> TaskMgrResult<bool> {
    let pattern = make_like_pattern(prefix);

    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id LIKE ? ESCAPE '\\'",
            rusqlite::params![pattern],
            |row| row.get(0),
        )
        .map_err(crate::TaskMgrError::DatabaseError)?;

    if total == 0 {
        return Ok(false);
    }

    let non_terminal: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id LIKE ? ESCAPE '\\' AND status IN ('todo', 'in_progress', 'blocked')",
            rusqlite::params![pattern],
            |row| row.get(0),
        )
        .map_err(crate::TaskMgrError::DatabaseError)?;

    Ok(non_terminal == 0)
}

/// Strip common branch prefixes (feat/, fix/, chore/, ralph/).
fn strip_branch_prefix(branch: &str) -> String {
    let prefixes = ["feat/", "fix/", "chore/", "ralph/", "feature/", "bugfix/"];
    for prefix in &prefixes {
        if let Some(stripped) = branch.strip_prefix(prefix) {
            return stripped.to_string();
        }
    }
    branch.to_string()
}

/// Discover files associated with the PRD that should be archived.
///
/// Prefers the `prd_files` table (v6+) for accurate file discovery.
/// Falls back to project-name-based guessing for pre-v6 databases.
fn discover_archivable_files(
    conn: &rusqlite::Connection,
    tasks_dir: &Path,
    prd_id: i64,
    project: &str,
) -> TaskMgrResult<Vec<PathBuf>> {
    let mut files = Vec::new();

    // Try prd_files table first (v6+ databases)
    let prd_file_paths = query_prd_files(conn, prd_id);

    if !prd_file_paths.is_empty() {
        // Use paths from the database
        for relative_path in &prd_file_paths {
            let path = tasks_dir.join(relative_path);
            if path.exists() {
                files.push(path);
            }
        }
    } else {
        // Fallback: guess from project name (pre-v6 databases)
        let candidates = vec![
            format!("{}.json", project),
            format!("{}-prompt.md", project),
            format!("prd-{}.md", project),
        ];

        for candidate in &candidates {
            let path = tasks_dir.join(candidate);
            if path.exists() {
                files.push(path);
            }
        }
    }

    Ok(files)
}

/// Query the prd_files table for file paths scoped to a specific PRD.
/// Returns empty vec if table doesn't exist or has no rows for this prd_id.
fn query_prd_files(conn: &rusqlite::Connection, prd_id: i64) -> Vec<String> {
    let result: Result<Vec<String>, rusqlite::Error> = (|| {
        let mut stmt = conn.prepare("SELECT file_path FROM prd_files WHERE prd_id = ?")?;
        let paths = stmt
            .query_map(rusqlite::params![prd_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(paths)
    })();

    result.unwrap_or_default()
}

/// Clear task data scoped to a single PRD.
///
/// Wraps all deletions in a single transaction. Deletes tasks matching
/// `{prefix}-%` and all dependent data (run_tasks, orphaned runs,
/// task_relationships, task_files). Also removes prd_files and prd_metadata
/// for the given `prd_id`. NULLs out dangling `global_state` references, and
/// resets `iteration_counter` only when no tasks remain across all PRDs.
///
/// Returns the number of tasks deleted.
fn clear_prd_data(
    conn: &mut rusqlite::Connection,
    prd_id: i64,
    prefix: &str,
) -> TaskMgrResult<usize> {
    let pattern = make_like_pattern(prefix);

    let tx = conn
        .transaction()
        .map_err(crate::TaskMgrError::DatabaseError)?;

    // 1. Delete run_tasks for this prefix
    tx.execute(
        "DELETE FROM run_tasks WHERE task_id LIKE ? ESCAPE '\\'",
        rusqlite::params![pattern],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 2. Delete orphaned runs (no remaining run_tasks reference them)
    tx.execute(
        "DELETE FROM runs WHERE NOT EXISTS \
         (SELECT 1 FROM run_tasks WHERE run_tasks.run_id = runs.run_id)",
        [],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 3. Delete task_relationships touching this prefix
    tx.execute(
        "DELETE FROM task_relationships \
         WHERE task_id LIKE ? ESCAPE '\\' OR related_id LIKE ? ESCAPE '\\'",
        rusqlite::params![pattern, pattern],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 4. Delete task_files for this prefix
    tx.execute(
        "DELETE FROM task_files WHERE task_id LIKE ? ESCAPE '\\'",
        rusqlite::params![pattern],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 5. Delete tasks and capture the count for reporting
    let deleted = tx
        .execute(
            "DELETE FROM tasks WHERE id LIKE ? ESCAPE '\\'",
            rusqlite::params![pattern],
        )
        .map_err(crate::TaskMgrError::DatabaseError)?;

    // 6. Delete prd_files for this PRD (may not exist in pre-v6 databases)
    let _ = tx.execute(
        "DELETE FROM prd_files WHERE prd_id = ?",
        rusqlite::params![prd_id],
    );

    // 7. Delete prd_metadata row
    tx.execute(
        "DELETE FROM prd_metadata WHERE id = ?",
        rusqlite::params![prd_id],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 8. NULL out last_task_id if the referenced task no longer exists
    tx.execute(
        "UPDATE global_state SET last_task_id = NULL \
         WHERE id = 1 AND last_task_id IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM tasks WHERE tasks.id = global_state.last_task_id)",
        [],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 9. NULL out last_run_id if the referenced run no longer exists
    tx.execute(
        "UPDATE global_state SET last_run_id = NULL \
         WHERE id = 1 AND last_run_id IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM runs WHERE runs.run_id = global_state.last_run_id)",
        [],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;

    // 10. Reset counters only when no tasks remain across all PRDs
    let remaining: i64 = tx
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .map_err(crate::TaskMgrError::DatabaseError)?;

    if remaining == 0 {
        tx.execute(
            "UPDATE global_state \
             SET iteration_counter = 0, last_task_id = NULL, last_run_id = NULL, \
                 updated_at = datetime('now') \
             WHERE id = 1",
            [],
        )
        .map_err(crate::TaskMgrError::DatabaseError)?;
    }

    tx.commit().map_err(crate::TaskMgrError::DatabaseError)?;

    Ok(deleted)
}

/// Extract learnings from progress.txt.
///
/// Looks for lines matching `**Learnings:**` and collects the bullet points
/// that follow until the next section marker (## or ---).
fn extract_learnings_from_progress(path: &Path) -> TaskMgrResult<Vec<String>> {
    let content = fs::read_to_string(path).map_err(|e| {
        crate::TaskMgrError::io_error(path.display().to_string(), "reading progress.txt", e)
    })?;

    let mut learnings = Vec::new();
    let mut in_learning_section = false;

    for line in content.lines() {
        if line.contains("**Learnings:**") {
            in_learning_section = true;
            // Extract inline learning text after the marker
            let after_marker = line.split("**Learnings:**").nth(1).unwrap_or("").trim();
            if !after_marker.is_empty() {
                learnings.push(after_marker.to_string());
            }
            continue;
        }

        if in_learning_section {
            // End of learning section: next heading or separator
            if line.starts_with("##") || line.starts_with("---") {
                in_learning_section = false;
                continue;
            }

            let trimmed = line.trim();
            if !trimmed.is_empty() {
                learnings.push(trimmed.to_string());
            }
        }
    }

    Ok(learnings)
}

/// Append extracted learnings to the learnings file.
///
/// Creates the file if it doesn't exist. Appends with a timestamp header.
fn append_learnings_to_file(path: &Path, learnings: &[String]) -> TaskMgrResult<()> {
    use std::io::Write;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut content = String::new();
    content.push_str(&format!("\n## Archived Learnings - {}\n\n", timestamp));

    for learning in learnings {
        // Ensure each learning is a bullet point
        if learning.starts_with("- ") || learning.starts_with("* ") {
            content.push_str(&format!("{}\n", learning));
        } else {
            content.push_str(&format!("- {}\n", learning));
        }
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            crate::TaskMgrError::io_error(path.display().to_string(), "opening learnings file", e)
        })?;

    file.write_all(content.as_bytes()).map_err(|e| {
        crate::TaskMgrError::io_error(path.display().to_string(), "writing learnings to file", e)
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_strip_branch_prefix_feat() {
        assert_eq!(strip_branch_prefix("feat/my-feature"), "my-feature");
    }

    #[test]
    fn test_strip_branch_prefix_fix() {
        assert_eq!(strip_branch_prefix("fix/bug-123"), "bug-123");
    }

    #[test]
    fn test_strip_branch_prefix_chore() {
        assert_eq!(strip_branch_prefix("chore/cleanup"), "cleanup");
    }

    #[test]
    fn test_strip_branch_prefix_ralph() {
        assert_eq!(
            strip_branch_prefix("ralph/task-mgr-phase-2"),
            "task-mgr-phase-2"
        );
    }

    #[test]
    fn test_strip_branch_prefix_feature() {
        assert_eq!(strip_branch_prefix("feature/long-name"), "long-name");
    }

    #[test]
    fn test_strip_branch_prefix_bugfix() {
        assert_eq!(strip_branch_prefix("bugfix/issue-99"), "issue-99");
    }

    #[test]
    fn test_strip_branch_prefix_no_prefix() {
        assert_eq!(strip_branch_prefix("main"), "main");
    }

    #[test]
    fn test_strip_branch_prefix_empty() {
        assert_eq!(strip_branch_prefix(""), "");
    }

    #[test]
    fn test_strip_branch_prefix_nested() {
        // Only strips first matching prefix
        assert_eq!(strip_branch_prefix("feat/nested/deep"), "nested/deep");
    }

    #[test]
    fn test_extract_learnings_from_progress() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("progress.txt");
        fs::write(
            &path,
            "## 2026-02-05 - FEAT-001\n\
             - Something implemented\n\
             - **Learnings:** First learning about things.\n\
             ---\n\
             ## 2026-02-05 - FEAT-002\n\
             - Another thing\n\
             - **Learnings:** Second learning here.\n\
             ---\n",
        )
        .unwrap();

        let learnings = extract_learnings_from_progress(&path).unwrap();
        assert_eq!(learnings.len(), 2);
        assert_eq!(learnings[0], "First learning about things.");
        assert_eq!(learnings[1], "Second learning here.");
    }

    #[test]
    fn test_extract_learnings_multiline() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("progress.txt");
        fs::write(
            &path,
            "## 2026-02-05 - FEAT-001\n\
             - **Learnings:**\n\
             - Point one\n\
             - Point two\n\
             - Point three\n\
             ---\n",
        )
        .unwrap();

        let learnings = extract_learnings_from_progress(&path).unwrap();
        assert_eq!(learnings.len(), 3);
        assert_eq!(learnings[0], "- Point one");
        assert_eq!(learnings[1], "- Point two");
        assert_eq!(learnings[2], "- Point three");
    }

    #[test]
    fn test_extract_learnings_no_learnings() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("progress.txt");
        fs::write(
            &path,
            "## 2026-02-05 - FEAT-001\n\
             - Something implemented\n\
             ---\n",
        )
        .unwrap();

        let learnings = extract_learnings_from_progress(&path).unwrap();
        assert!(learnings.is_empty());
    }

    #[test]
    fn test_extract_learnings_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("progress.txt");
        fs::write(&path, "").unwrap();

        let learnings = extract_learnings_from_progress(&path).unwrap();
        assert!(learnings.is_empty());
    }

    #[test]
    fn test_append_learnings_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("learnings.md");

        let learnings = vec![
            "First learning".to_string(),
            "- Second learning (already bulleted)".to_string(),
        ];
        append_learnings_to_file(&path, &learnings).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("## Archived Learnings"));
        assert!(content.contains("- First learning"));
        assert!(content.contains("- Second learning (already bulleted)"));
    }

    #[test]
    fn test_append_learnings_appends_to_existing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("learnings.md");
        fs::write(&path, "# Existing Learnings\n\n- Old learning\n").unwrap();

        let learnings = vec!["New learning".to_string()];
        append_learnings_to_file(&path, &learnings).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("# Existing Learnings"));
        assert!(content.contains("- Old learning"));
        assert!(content.contains("## Archived Learnings"));
        assert!(content.contains("- New learning"));
    }

    #[test]
    fn test_discover_archivable_files_fallback() {
        // When prd_files table is empty, falls back to project-name-based discovery
        let dir = TempDir::new().unwrap();
        let tasks_dir = dir.path();

        let conn = setup_db(dir.path());

        // Create some files
        fs::write(tasks_dir.join("my-project.json"), "{}").unwrap();
        fs::write(tasks_dir.join("my-project-prompt.md"), "# Prompt").unwrap();
        fs::write(tasks_dir.join("prd-my-project.md"), "# PRD").unwrap();
        fs::write(tasks_dir.join("progress.txt"), "# Progress").unwrap();
        fs::write(tasks_dir.join("unrelated.txt"), "other").unwrap();

        let files = discover_archivable_files(&conn, tasks_dir, 1, "my-project").unwrap();
        assert_eq!(files.len(), 3);

        let filenames: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(filenames.contains(&"my-project.json".to_string()));
        assert!(filenames.contains(&"my-project-prompt.md".to_string()));
        assert!(filenames.contains(&"prd-my-project.md".to_string()));
        assert!(!filenames.contains(&"progress.txt".to_string()));
        assert!(!filenames.contains(&"unrelated.txt".to_string()));
    }

    #[test]
    fn test_discover_archivable_files_none_exist() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        let files = discover_archivable_files(&conn, dir.path(), 1, "nonexistent").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_archive_result_fields() {
        let result = ArchiveResult {
            archived: vec![ArchivedItem {
                source: "a.json".to_string(),
                destination: "archive/dir/a.json".to_string(),
            }],
            learnings_extracted: 5,
            tasks_cleared: 0,
            dry_run: true,
            message: "test".to_string(),
            prds_archived: Vec::new(),
            prds_skipped: Vec::new(),
        };

        assert_eq!(result.archived.len(), 1);
        assert_eq!(result.learnings_extracted, 5);
        assert!(result.dry_run);
        assert_eq!(result.archived[0].source, "a.json");
        assert_eq!(result.archived[0].destination, "archive/dir/a.json");
        assert!(result.prds_archived.is_empty());
        assert!(result.prds_skipped.is_empty());
    }

    #[test]
    fn test_prd_archive_summary_fields() {
        let summary = PrdArchiveSummary {
            prd_id: 42,
            project: "my-project".to_string(),
            task_prefix: "MP".to_string(),
            archive_folder: "2026-03-03-my-branch".to_string(),
            files_archived: 3,
            tasks_cleared: 10,
        };
        assert_eq!(summary.prd_id, 42);
        assert_eq!(summary.project, "my-project");
        assert_eq!(summary.task_prefix, "MP");
        assert_eq!(summary.archive_folder, "2026-03-03-my-branch");
        assert_eq!(summary.files_archived, 3);
        assert_eq!(summary.tasks_cleared, 10);
    }

    #[test]
    fn test_prd_skip_reason_fields() {
        let skip = PrdSkipReason {
            prd_id: 7,
            project: "other-project".to_string(),
            reason: "Not fully completed".to_string(),
        };
        assert_eq!(skip.prd_id, 7);
        assert_eq!(skip.project, "other-project");
        assert_eq!(skip.reason, "Not fully completed");
    }

    #[test]
    fn test_run_archive_no_metadata() {
        let dir = TempDir::new().unwrap();

        // Create DB with schema but no metadata
        drop(setup_db(dir.path()));

        let result = run_archive(dir.path(), false).unwrap();
        assert!(result.archived.is_empty());
        assert!(result.message.contains("No PRD metadata"));
    }

    #[test]
    fn test_run_archive_incomplete_prd() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());
        insert_prd(&conn, 1, "test-project", "feat/my-feature", Some("FEAT"));
        insert_task(&conn, "FEAT-001", "Test task", 1, "todo");
        drop(conn);

        let result = run_archive(dir.path(), false).unwrap();
        assert!(result.archived.is_empty());
        assert!(result.message.contains("not fully completed"));
    }

    #[test]
    fn test_run_archive_dry_run_completed_prd() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());
        insert_prd(&conn, 1, "test-project", "feat/my-feature", Some("FEAT"));
        insert_task(&conn, "FEAT-001", "Test task", 1, "done");
        drop(conn);

        // Create tasks dir with files
        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("test-project.json"), "{}").unwrap();
        fs::write(tasks_dir.join("test-project-prompt.md"), "# Prompt").unwrap();

        let result = run_archive(dir.path(), true).unwrap();
        assert!(result.dry_run);
        assert_eq!(result.archived.len(), 2);

        // Verify files were NOT moved
        assert!(tasks_dir.join("test-project.json").exists());
        assert!(tasks_dir.join("test-project-prompt.md").exists());
    }

    #[test]
    fn test_run_archive_actual_move() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());
        insert_prd(&conn, 1, "test-project", "ralph/test-branch", Some("FEAT"));
        insert_task(&conn, "FEAT-001", "Done task", 1, "done");
        drop(conn);

        // Create tasks dir with files
        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("test-project.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();
        assert!(!result.dry_run);
        assert_eq!(result.archived.len(), 1);

        // Verify file WAS moved
        assert!(!tasks_dir.join("test-project.json").exists());

        // Verify file exists in archive (we can't predict exact date)
        let archive_dir = tasks_dir.join("archive");
        assert!(archive_dir.exists());
        let entries: Vec<_> = fs::read_dir(&archive_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let archive_subfolder = entries[0].path();
        assert!(archive_subfolder.join("test-project.json").exists());

        // Verify branch prefix was stripped
        let folder_name = archive_subfolder
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(folder_name.contains("test-branch"));
        assert!(!folder_name.contains("ralph/"));

        // Verify DB was cleared
        assert_eq!(result.tasks_cleared, 1);
        let conn = crate::db::open_connection(dir.path()).unwrap();
        let task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(task_count, 0);
        let metadata_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |row| row.get(0))
            .unwrap();
        assert_eq!(metadata_count, 0);
    }

    #[test]
    fn test_run_archive_with_learnings() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());
        insert_prd(&conn, 1, "test-project", "main", Some("FEAT"));
        insert_task(&conn, "FEAT-001", "Done", 1, "done");
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("test-project.json"), "{}").unwrap();
        fs::write(
            tasks_dir.join("progress.txt"),
            "## FEAT-001\n- **Learnings:** Important discovery.\n---\n",
        )
        .unwrap();

        let result = run_archive(dir.path(), false).unwrap();
        assert_eq!(result.learnings_extracted, 1);

        // Verify learnings.md was created
        let learnings_path = tasks_dir.join("learnings.md");
        assert!(learnings_path.exists());
        let content = fs::read_to_string(&learnings_path).unwrap();
        assert!(content.contains("Important discovery."));
        assert!(content.contains("## Archived Learnings"));
    }

    #[test]
    fn test_run_archive_no_tasks() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());
        insert_prd(&conn, 1, "test-project", "main", None);
        // No tasks at all
        drop(conn);

        let result = run_archive(dir.path(), false).unwrap();
        assert!(result.archived.is_empty());
        assert!(result.message.contains("not fully completed"));
    }

    #[test]
    fn test_run_archive_preserves_learnings() {
        use crate::learnings::crud::{record_learning, RecordLearningParams};
        use crate::models::{Confidence, LearningOutcome};

        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());
        insert_prd(&conn, 1, "test-project", "main", Some("FEAT"));
        insert_task(&conn, "FEAT-001", "Done", 1, "done");

        // Record a learning in the database
        let params = RecordLearningParams {
            outcome: LearningOutcome::Success,
            title: "Archive test learning".to_string(),
            content: "This learning should survive archive".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        };
        record_learning(&conn, params).unwrap();

        drop(conn);

        // Create tasks dir with files to archive
        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("test-project.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();
        assert_eq!(result.tasks_cleared, 1);

        // Verify learnings survived
        let conn = crate::db::open_connection(dir.path()).unwrap();
        let learning_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learnings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(learning_count, 1);

        let title: String = conn
            .query_row("SELECT title FROM learnings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "Archive test learning");

        // Verify learning_tags survived (tags table should still exist)
        let tag_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_tags", [], |row| row.get(0))
            .unwrap();
        // No tags were added, but table should still be accessible
        assert_eq!(tag_count, 0);
    }

    #[test]
    fn test_prd_files_drives_discovery() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());

        // Insert prd_files entries (simulating what init would do)
        // Note: no branch/prefix here — project-only PRD
        conn.execute(
            "INSERT INTO prd_metadata (id, project) VALUES (1, 'model-selection')",
            [],
        )
        .unwrap();
        insert_prd_file(&conn, 1, "prd-model-phase1.json", "task_list");
        insert_prd_file(&conn, 1, "prd-model-phase1-prompt.md", "prompt");
        insert_prd_file(&conn, 1, "prd-model-selection.md", "prd");

        // Create the files on disk
        let tasks_dir = dir.path();
        fs::write(tasks_dir.join("prd-model-phase1.json"), "{}").unwrap();
        fs::write(tasks_dir.join("prd-model-phase1-prompt.md"), "# Prompt").unwrap();
        fs::write(tasks_dir.join("prd-model-selection.md"), "# PRD").unwrap();
        // Also create a project-name file that should NOT be found
        // (prd_files takes precedence over project-name guessing)
        fs::write(tasks_dir.join("model-selection.json"), "{}").unwrap();

        let files = discover_archivable_files(&conn, tasks_dir, 1, "model-selection").unwrap();

        let filenames: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        // Should find prd_files entries, NOT project-name-based guesses
        assert_eq!(files.len(), 3);
        assert!(filenames.contains(&"prd-model-phase1.json".to_string()));
        assert!(filenames.contains(&"prd-model-phase1-prompt.md".to_string()));
        assert!(filenames.contains(&"prd-model-selection.md".to_string()));
        // Should NOT include the project-name-guessed file
        assert!(!filenames.contains(&"model-selection.json".to_string()));
    }

    #[test]
    fn test_skipped_and_irrelevant_tasks_are_terminal() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());
        insert_prd(&conn, 1, "test-project", "main", Some("T"));
        // Mix of done, skipped, and irrelevant — all terminal
        insert_task(&conn, "T-001", "Done", 1, "done");
        insert_task(&conn, "T-002", "Skipped", 2, "skipped");
        insert_task(&conn, "T-003", "Irrelevant", 3, "irrelevant");

        // PRD should be considered completed (all tasks terminal, scoped to prefix "T")
        assert!(is_prd_completed_by_prefix(&conn, "T").unwrap());

        drop(conn);

        // Create tasks dir with files
        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("test-project.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();
        assert_eq!(result.tasks_cleared, 3);
        assert!(!result.archived.is_empty());
    }

    // -----------------------------------------------------------------------
    // Tests for is_prd_completed_by_prefix
    // -----------------------------------------------------------------------

    fn setup_db(dir: &std::path::Path) -> rusqlite::Connection {
        let mut conn = crate::db::open_connection(dir).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();
        conn
    }

    fn insert_prd(
        conn: &rusqlite::Connection,
        id: i64,
        project: &str,
        branch: &str,
        prefix: Option<&str>,
    ) {
        match prefix {
            Some(p) => conn.execute(
                "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, project, branch, p],
            ),
            None => conn.execute(
                "INSERT INTO prd_metadata (id, project, branch_name) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, project, branch],
            ),
        }
        .unwrap();
    }

    fn insert_task(
        conn: &rusqlite::Connection,
        id: &str,
        title: &str,
        priority: i64,
        status: &str,
    ) {
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, title, priority, status],
        )
        .unwrap();
    }

    fn insert_prd_file(conn: &rusqlite::Connection, prd_id: i64, path: &str, file_type: &str) {
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (?1, ?2, ?3)",
            rusqlite::params![prd_id, path, file_type],
        )
        .unwrap();
    }

    #[test]
    fn test_prefix_all_done_returns_true() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        insert_task(&conn, "P1-US-001", "Task 1", 1, "done");
        insert_task(&conn, "P1-US-002", "Task 2", 2, "done");

        assert!(is_prd_completed_by_prefix(&conn, "P1").unwrap());
    }

    #[test]
    fn test_prefix_one_todo_returns_false() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        insert_task(&conn, "P1-US-001", "Task 1", 1, "done");
        insert_task(&conn, "P1-US-002", "Task 2", 2, "todo");

        assert!(!is_prd_completed_by_prefix(&conn, "P1").unwrap());
    }

    #[test]
    fn test_prefix_zero_matching_tasks_returns_false() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        // Tasks exist but under a different prefix
        insert_task(&conn, "P2-US-001", "Task 1", 1, "done");

        assert!(!is_prd_completed_by_prefix(&conn, "P1").unwrap());
    }

    #[test]
    fn test_prefix_p1_does_not_match_p10_tasks() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        // P1 tasks: all done
        insert_task(&conn, "P1-US-001", "Task 1", 1, "done");
        // P10 tasks: still todo — must NOT be included in the P1 check
        insert_task(&conn, "P10-US-001", "Task 2", 2, "todo");

        // P1 should be completed (its tasks are all done)
        assert!(is_prd_completed_by_prefix(&conn, "P1").unwrap());
        // P10 should NOT be completed (has a todo task)
        assert!(!is_prd_completed_by_prefix(&conn, "P10").unwrap());
    }

    #[test]
    fn test_prefix_mixed_terminal_states_returns_true() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        insert_task(&conn, "P1-US-001", "Done", 1, "done");
        insert_task(&conn, "P1-US-002", "Skipped", 2, "skipped");
        insert_task(&conn, "P1-US-003", "Irrelevant", 3, "irrelevant");

        assert!(is_prd_completed_by_prefix(&conn, "P1").unwrap());
    }

    /// Discriminator: prefix-scoped check correctly distinguishes two PRDs
    /// when one is complete and the other has pending tasks.
    #[test]
    fn test_discriminator_prefix_scoped_check_distinguishes_prds() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        // P1 tasks: all done
        insert_task(&conn, "P1-US-001", "Done", 1, "done");
        // P2 tasks: still in progress
        insert_task(&conn, "P2-US-001", "In progress", 1, "in_progress");

        // Prefix-scoped check correctly distinguishes the two PRDs:
        assert!(
            is_prd_completed_by_prefix(&conn, "P1").unwrap(),
            "P1 should be complete"
        );
        assert!(
            !is_prd_completed_by_prefix(&conn, "P2").unwrap(),
            "P2 should not be complete"
        );
    }

    #[test]
    fn test_skipped_task_blocks_archive_when_mixed_with_todo() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());
        conn.execute(
            "INSERT INTO prd_metadata (id, project) VALUES (1, 'test-project')",
            [],
        )
        .unwrap();
        insert_task(&conn, "T-001", "Done", 1, "done");
        insert_task(&conn, "T-002", "Todo", 2, "todo");

        // PRD should NOT be considered completed (todo task remains)
        assert!(!is_prd_completed_by_prefix(&conn, "T").unwrap());
    }

    #[test]
    fn test_init_registers_prd_files() {
        use crate::commands::init::{init, PrefixMode};

        let dir = TempDir::new().unwrap();
        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();

        let json = r#"{
            "project": "test-project",
            "prdFile": "prd-model-selection.md",
            "userStories": [
                {"id": "US-001", "title": "Task 1", "priority": 1, "passes": true}
            ]
        }"#;
        let json_path = tasks_dir.join("prd-model-phase1.json");
        fs::write(&json_path, json).unwrap();

        // Create the prompt file so it gets registered
        fs::write(tasks_dir.join("prd-model-phase1-prompt.md"), "# Prompt").unwrap();

        init(
            dir.path(),
            &[&json_path],
            false,
            false,
            false,
            false,
            PrefixMode::Disabled,
        )
        .unwrap();

        let conn = crate::db::open_connection(dir.path()).unwrap();

        // Verify prd_files entries
        let file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(file_count, 3); // task_list + prompt + prd

        // Verify specific entries
        let task_list: String = conn
            .query_row(
                "SELECT file_path FROM prd_files WHERE file_type = 'task_list'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(task_list, "prd-model-phase1.json");

        let prompt: String = conn
            .query_row(
                "SELECT file_path FROM prd_files WHERE file_type = 'prompt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(prompt, "prd-model-phase1-prompt.md");

        let prd: String = conn
            .query_row(
                "SELECT file_path FROM prd_files WHERE file_type = 'prd'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(prd, "prd-model-selection.md");
    }

    // -----------------------------------------------------------------------
    // Tests for clear_prd_data_for_prefix
    // -----------------------------------------------------------------------

    /// Insert two PRDs' worth of tasks: PA-001/PA-002 (done) and PB-001/PB-002
    /// (in_progress/todo) so tests can assert scoped deletion.
    fn setup_two_prds(conn: &rusqlite::Connection) {
        insert_task(conn, "PA-001", "PA Task 1", 1, "done");
        insert_task(conn, "PA-002", "PA Task 2", 2, "done");
        insert_task(conn, "PB-001", "PB Task 1", 1, "in_progress");
        insert_task(conn, "PB-002", "PB Task 2", 2, "todo");
    }

    #[test]
    fn test_clear_prd_a_leaves_prd_b_intact() {
        let dir = TempDir::new().unwrap();
        let mut conn = setup_db(dir.path());
        setup_two_prds(&conn);

        clear_prd_data(&mut conn, 1, "PA").unwrap();

        let pa_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'PA-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(pa_count, 0, "PRD A tasks should be deleted");

        let pb_in_progress: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = 'PB-001' AND status = 'in_progress'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pb_in_progress, 1, "PRD B in_progress task must survive");

        let pb_todo: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = 'PB-002' AND status = 'todo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pb_todo, 1, "PRD B todo task must survive");
    }

    #[test]
    fn test_clear_prd_a_leaves_learnings_intact() {
        use crate::learnings::crud::{record_learning, RecordLearningParams};
        use crate::models::{Confidence, LearningOutcome};

        let dir = TempDir::new().unwrap();
        let mut conn = setup_db(dir.path());
        setup_two_prds(&conn);

        let params = RecordLearningParams {
            outcome: LearningOutcome::Success,
            title: "Survives PRD clear".to_string(),
            content: "This learning must not be deleted".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        };
        record_learning(&conn, params).unwrap();

        clear_prd_data(&mut conn, 1, "PA").unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "Learnings must survive PRD clear");
    }

    #[test]
    fn test_clear_prd_a_deletes_orphaned_runs() {
        let dir = TempDir::new().unwrap();
        let mut conn = setup_db(dir.path());
        setup_two_prds(&conn);

        // Run that only references PA tasks — will be orphaned after clear
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-a-only', 'completed')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-a-only', 'PA-001', 'completed', 1)",
            [],
        )
        .unwrap();

        clear_prd_data(&mut conn, 1, "PA").unwrap();

        let run_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id = 'run-a-only'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(run_count, 0, "Orphaned run should be deleted");
    }

    #[test]
    fn test_clear_prd_a_preserves_shared_runs() {
        let dir = TempDir::new().unwrap();
        let mut conn = setup_db(dir.path());
        setup_two_prds(&conn);

        // Run that references tasks from both PRDs
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-shared', 'completed')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-shared', 'PA-001', 'completed', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-shared', 'PB-001', 'completed', 2)",
            [],
        )
        .unwrap();

        clear_prd_data(&mut conn, 1, "PA").unwrap();

        // Run must survive because PB-001 still references it
        let run_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id = 'run-shared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(run_count, 1, "Shared run must be preserved");

        // The PA run_task entry should be gone
        let rt_pa: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_tasks WHERE run_id = 'run-shared' AND task_id = 'PA-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rt_pa, 0, "PA run_task entry should be removed");
    }

    #[test]
    fn test_global_state_counters_not_reset_while_other_prd_remains() {
        let dir = TempDir::new().unwrap();
        let mut conn = setup_db(dir.path());
        setup_two_prds(&conn);

        conn.execute(
            "UPDATE global_state SET iteration_counter = 42 WHERE id = 1",
            [],
        )
        .unwrap();

        // Clear PA only — PB still has tasks, so counters must NOT reset
        clear_prd_data(&mut conn, 1, "PA").unwrap();

        let counter: i64 = conn
            .query_row(
                "SELECT iteration_counter FROM global_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            counter, 42,
            "Counter must not reset while another PRD's tasks remain"
        );
    }

    #[test]
    fn test_global_state_counters_reset_when_last_prd_cleared() {
        let dir = TempDir::new().unwrap();
        let mut conn = setup_db(dir.path());
        setup_two_prds(&conn);

        conn.execute(
            "UPDATE global_state SET iteration_counter = 42 WHERE id = 1",
            [],
        )
        .unwrap();

        clear_prd_data(&mut conn, 1, "PA").unwrap();
        clear_prd_data(&mut conn, 2, "PB").unwrap();

        let counter: i64 = conn
            .query_row(
                "SELECT iteration_counter FROM global_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            counter, 0,
            "Counter must reset when the last PRD is cleared"
        );
    }

    #[test]
    fn test_global_state_last_task_id_nulled_if_deleted() {
        let dir = TempDir::new().unwrap();
        let mut conn = setup_db(dir.path());
        setup_two_prds(&conn);

        conn.execute(
            "UPDATE global_state SET last_task_id = 'PA-001' WHERE id = 1",
            [],
        )
        .unwrap();

        clear_prd_data(&mut conn, 1, "PA").unwrap();

        let last_task: Option<String> = conn
            .query_row(
                "SELECT last_task_id FROM global_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            last_task.is_none(),
            "last_task_id must be NULL after the referenced task is deleted"
        );
    }

    #[test]
    fn test_global_state_last_task_id_preserved_if_from_other_prd() {
        let dir = TempDir::new().unwrap();
        let mut conn = setup_db(dir.path());
        setup_two_prds(&conn);

        conn.execute(
            "UPDATE global_state SET last_task_id = 'PB-001' WHERE id = 1",
            [],
        )
        .unwrap();

        clear_prd_data(&mut conn, 1, "PA").unwrap();

        let last_task: Option<String> = conn
            .query_row(
                "SELECT last_task_id FROM global_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            last_task.as_deref(),
            Some("PB-001"),
            "last_task_id referencing another PRD's task must be preserved"
        );
    }

    // -----------------------------------------------------------------------
    // Tests for multi-PRD run_archive() top-level flow
    // -----------------------------------------------------------------------

    /// Insert two PRDs into prd_metadata with task_prefix set.
    /// PRD-A (prefix "PA", branch "feat/branch-a"): tasks PA-001/PA-002 — both done.
    /// PRD-B (prefix "PB", branch "feat/branch-b"): task PB-001 — in_progress.
    fn setup_two_prd_metadata_with_tasks(conn: &rusqlite::Connection) {
        insert_prd(conn, 1, "project-a", "feat/branch-a", Some("PA"));
        insert_prd(conn, 2, "project-b", "feat/branch-b", Some("PB"));
        // PA tasks: complete
        insert_task(conn, "PA-001", "PA Task 1", 1, "done");
        insert_task(conn, "PA-002", "PA Task 2", 2, "done");
        // PB task: incomplete
        insert_task(conn, "PB-001", "PB Task 1", 1, "in_progress");
    }

    /// Two PRDs, one complete: only the complete PRD's files are archived.
    /// The incomplete PRD's files must remain untouched.
    #[test]
    fn test_multi_prd_only_complete_prd_archived() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        setup_two_prd_metadata_with_tasks(&conn);

        // Register PA's and PB's files in prd_files
        insert_prd_file(&conn, 1, "project-a.json", "task_list");
        insert_prd_file(&conn, 2, "project-b.json", "task_list");
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("project-a.json"), "{}").unwrap();
        fs::write(tasks_dir.join("project-b.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();

        // PA should be archived
        assert!(
            !result.archived.is_empty(),
            "At least one file should be archived"
        );
        let archived_sources: Vec<&str> =
            result.archived.iter().map(|a| a.source.as_str()).collect();
        assert!(
            archived_sources.iter().any(|s| s.contains("project-a")),
            "project-a.json should be archived"
        );

        // PB file must remain (incomplete)
        assert!(
            tasks_dir.join("project-b.json").exists(),
            "project-b.json must not be moved (PB is incomplete)"
        );
    }

    /// Two PRDs, both complete: files for each PRD archived to their own folders.
    #[test]
    fn test_multi_prd_both_complete_archived_to_separate_folders() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (1, 'project-a', 'feat/branch-a', 'PA')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (2, 'project-b', 'feat/branch-b', 'PB')",
            [],
        )
        .unwrap();
        // Both PA and PB complete
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('PA-001', 'PA Task', 1, 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('PB-001', 'PB Task', 1, 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) \
             VALUES (1, 'project-a.json', 'task_list')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) \
             VALUES (2, 'project-b.json', 'task_list')",
            [],
        )
        .unwrap();
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("project-a.json"), "{}").unwrap();
        fs::write(tasks_dir.join("project-b.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();

        // Both files should be archived
        assert_eq!(
            result.archived.len(),
            2,
            "Both PRD files should be archived"
        );

        // Each should go to a separate folder (branch-a vs branch-b)
        let dests: Vec<&str> = result
            .archived
            .iter()
            .map(|a| a.destination.as_str())
            .collect();
        assert!(
            dests.iter().any(|d| d.contains("branch-a")),
            "project-a should archive to branch-a folder"
        );
        assert!(
            dests.iter().any(|d| d.contains("branch-b")),
            "project-b should archive to branch-b folder"
        );

        // Both source files should be gone
        assert!(!tasks_dir.join("project-a.json").exists());
        assert!(!tasks_dir.join("project-b.json").exists());
    }

    /// A PRD with NULL task_prefix is skipped (can't scope by prefix).
    #[test]
    fn test_multi_prd_null_task_prefix_skipped() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        // PRD with no task_prefix
        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (1, 'legacy-project', 'feat/legacy', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('L-001', 'Legacy Task', 1, 'done')",
            [],
        )
        .unwrap();
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("legacy-project.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();

        // The NULL-prefix PRD should be skipped — file must NOT be moved
        assert!(
            tasks_dir.join("legacy-project.json").exists(),
            "PRD with NULL task_prefix must not be archived"
        );
        // Result should contain a skip reason, not an archive entry for this file
        let archived_sources: Vec<&str> =
            result.archived.iter().map(|a| a.source.as_str()).collect();
        assert!(
            !archived_sources
                .iter()
                .any(|s| s.contains("legacy-project")),
            "legacy-project.json must not appear in archived list"
        );
    }

    /// No PRD metadata: returns empty result with informative message.
    #[test]
    fn test_multi_prd_no_metadata_returns_empty_with_message() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        drop(conn);

        let result = run_archive(dir.path(), false).unwrap();

        assert!(result.archived.is_empty());
        assert_eq!(result.tasks_cleared, 0);
        assert!(
            result.message.contains("No PRD metadata") || result.message.contains("no PRD"),
            "Message should indicate no PRD metadata found, got: {}",
            result.message
        );
    }

    /// dry_run=true: no files are moved, no DB rows are deleted.
    #[test]
    fn test_multi_prd_dry_run_no_changes() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        setup_two_prd_metadata_with_tasks(&conn);

        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) \
             VALUES (1, 'project-a.json', 'task_list')",
            [],
        )
        .unwrap();
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("project-a.json"), "{}").unwrap();
        fs::write(tasks_dir.join("project-b.json"), "{}").unwrap();

        let result = run_archive(dir.path(), true).unwrap();

        assert!(result.dry_run, "dry_run flag must be true in result");

        // Files must NOT be moved
        assert!(
            tasks_dir.join("project-a.json").exists(),
            "project-a.json must not be moved in dry_run"
        );
        assert!(
            tasks_dir.join("project-b.json").exists(),
            "project-b.json must not be moved in dry_run"
        );

        // DB must not be touched: PA tasks still exist
        let conn = crate::db::open_connection(dir.path()).unwrap();
        let pa_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'PA-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(pa_count, 2, "PA tasks must not be deleted in dry_run");
    }

    /// progress.txt is never moved — it stays in place even after a successful archive.
    #[test]
    fn test_multi_prd_progress_txt_never_moved() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (1, 'project-a', 'feat/branch-a', 'PA')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('PA-001', 'PA Task', 1, 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) \
             VALUES (1, 'project-a.json', 'task_list')",
            [],
        )
        .unwrap();
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("project-a.json"), "{}").unwrap();
        fs::write(tasks_dir.join("progress.txt"), "## PA-001\n- Done.\n---\n").unwrap();

        let result = run_archive(dir.path(), false).unwrap();

        // Archive should succeed
        assert!(
            !result.archived.is_empty(),
            "Archive should produce results"
        );

        // progress.txt must remain in tasks/
        assert!(
            tasks_dir.join("progress.txt").exists(),
            "progress.txt must never be moved to the archive"
        );

        // progress.txt must NOT appear in the archived list
        let archived_sources: Vec<&str> =
            result.archived.iter().map(|a| a.source.as_str()).collect();
        assert!(
            !archived_sources.iter().any(|s| s.contains("progress.txt")),
            "progress.txt must not appear in archived items"
        );
    }

    /// Known-bad discriminator: a naive `DELETE FROM tasks` without a WHERE
    /// clause destroys all PRDs. This test documents the catastrophic outcome
    /// to confirm the scoped `clear_prd_data_for_prefix` must never do this.
    #[test]
    fn test_discriminator_unscoped_delete_destroys_other_prds() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());
        setup_two_prds(&conn);

        // Simulate the naive (bad) global delete
        conn.execute("DELETE FROM tasks", []).unwrap();

        // PB tasks are gone — this is the outcome the scoped function must prevent
        let pb_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'PB-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            pb_count, 0,
            "Naive DELETE FROM tasks destroys all PRDs — \
             this is the anti-pattern clear_prd_data_for_prefix must avoid"
        );
    }

    // -----------------------------------------------------------------------
    // NEW TESTS: three-PRD scenario, learnings-on-skip-all, JSON serialization,
    // archive folder naming end-to-end
    // -----------------------------------------------------------------------

    /// Three PRDs: one complete (PA), one incomplete (PB), one with NULL
    /// task_prefix (PC). Verifies:
    ///   - Only PA is archived
    ///   - PB is skipped with "Not fully completed" reason
    ///   - PC is skipped with "No task prefix" reason
    ///   - prds_archived.len() == 1, prds_skipped.len() == 2
    #[test]
    fn test_three_prds_complete_incomplete_no_prefix() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        // PA: complete
        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (1, 'project-a', 'feat/branch-a', 'PA')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('PA-001', 'PA Task', 1, 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, 'project-a.json', 'task_list')",
            [],
        )
        .unwrap();

        // PB: incomplete
        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (2, 'project-b', 'feat/branch-b', 'PB')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('PB-001', 'PB Task', 1, 'in_progress')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (2, 'project-b.json', 'task_list')",
            [],
        )
        .unwrap();

        // PC: NULL task_prefix
        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (3, 'project-c', 'feat/branch-c', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (3, 'project-c.json', 'task_list')",
            [],
        )
        .unwrap();
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("project-a.json"), "{}").unwrap();
        fs::write(tasks_dir.join("project-b.json"), "{}").unwrap();
        fs::write(tasks_dir.join("project-c.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();

        // Only PA archived
        assert_eq!(result.prds_archived.len(), 1, "Only PA should be archived");
        assert_eq!(result.prds_archived[0].task_prefix, "PA");

        // PB and PC skipped
        assert_eq!(result.prds_skipped.len(), 2, "PB and PC should be skipped");

        let skip_reasons: Vec<&str> = result
            .prds_skipped
            .iter()
            .map(|s| s.reason.as_str())
            .collect();
        assert!(
            skip_reasons
                .iter()
                .any(|r| r.contains("Not fully completed")),
            "PB skip reason should mention not completed"
        );
        assert!(
            skip_reasons.iter().any(|r| r.contains("No task prefix")),
            "PC skip reason should mention no task prefix"
        );

        // PA file moved, PB and PC files remain
        assert!(
            !tasks_dir.join("project-a.json").exists(),
            "PA file should be archived"
        );
        assert!(
            tasks_dir.join("project-b.json").exists(),
            "PB file must stay"
        );
        assert!(
            tasks_dir.join("project-c.json").exists(),
            "PC file must stay"
        );
    }

    /// When ALL PRDs are skipped (none archived), learnings must NOT be extracted.
    #[test]
    fn test_learnings_not_extracted_when_all_prds_skipped() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (1, 'project-a', 'main', 'PA')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('PA-001', 'Incomplete', 1, 'todo')",
            [],
        )
        .unwrap();
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(
            tasks_dir.join("progress.txt"),
            "## PA-001\n- **Learnings:** Should not be extracted.\n---\n",
        )
        .unwrap();

        let result = run_archive(dir.path(), false).unwrap();

        assert_eq!(result.prds_archived.len(), 0, "No PRDs archived");
        assert_eq!(
            result.learnings_extracted, 0,
            "Learnings must not be extracted when nothing archived"
        );

        // learnings.md must NOT be created
        assert!(
            !tasks_dir.join("learnings.md").exists(),
            "learnings.md must not be created when nothing archived"
        );
    }

    /// JSON serialization round-trip for ArchiveResult and its nested types.
    #[test]
    fn test_archive_result_json_serialization() {
        let result = ArchiveResult {
            archived: vec![ArchivedItem {
                source: "project-a.json".to_string(),
                destination: "archive/2026-03-04-branch-a/project-a.json".to_string(),
            }],
            learnings_extracted: 2,
            tasks_cleared: 5,
            dry_run: false,
            message: "Archived 1 PRD(s), 1 file(s).".to_string(),
            prds_archived: vec![PrdArchiveSummary {
                prd_id: 1,
                project: "project-a".to_string(),
                task_prefix: "PA".to_string(),
                archive_folder: "2026-03-04-branch-a".to_string(),
                files_archived: 1,
                tasks_cleared: 5,
            }],
            prds_skipped: vec![PrdSkipReason {
                prd_id: 2,
                project: "project-b".to_string(),
                reason: "Not fully completed".to_string(),
            }],
        };

        let json = serde_json::to_string(&result).expect("serialization must succeed");

        // Key fields present in JSON
        assert!(json.contains("\"archived\""));
        assert!(json.contains("\"learnings_extracted\":2"));
        assert!(json.contains("\"tasks_cleared\":5"));
        assert!(json.contains("\"dry_run\":false"));
        assert!(json.contains("\"prds_archived\""));
        assert!(json.contains("\"prds_skipped\""));
        assert!(json.contains("\"task_prefix\":\"PA\""));
        assert!(json.contains("\"prd_id\":2"));
        assert!(json.contains("\"reason\":\"Not fully completed\""));
    }

    /// Archive folder naming: feat/ prefix stripped, ralph/ stripped, no prefix unchanged.
    #[test]
    fn test_archive_folder_naming_various_branch_prefixes() {
        // feat/ stripped
        assert!(
            strip_branch_prefix("feat/my-feature") == "my-feature",
            "feat/ should be stripped"
        );
        // ralph/ stripped
        assert!(
            strip_branch_prefix("ralph/my-feature") == "my-feature",
            "ralph/ should be stripped"
        );
        // no prefix: returned as-is
        assert!(
            strip_branch_prefix("main") == "main",
            "branch without prefix should be returned as-is"
        );
        // empty branch → archive_folder_name uses date only
        assert!(
            strip_branch_prefix("") == "",
            "empty branch should return empty string"
        );
    }

    /// End-to-end: archive folder uses branch_name with feat/ stripped.
    #[test]
    fn test_archive_folder_uses_stripped_branch_name() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (1, 'project-a', 'feat/cool-feature', 'PA')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('PA-001', 'Task', 1, 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, 'project-a.json', 'task_list')",
            [],
        )
        .unwrap();
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("project-a.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();

        assert_eq!(result.prds_archived.len(), 1);
        let folder = &result.prds_archived[0].archive_folder;
        assert!(
            folder.contains("cool-feature"),
            "archive folder should contain stripped branch name 'cool-feature', got: {}",
            folder
        );
        assert!(
            !folder.contains("feat/"),
            "archive folder must not contain 'feat/' prefix, got: {}",
            folder
        );
    }

    /// End-to-end: archive folder uses date only when branch_name is empty.
    #[test]
    fn test_archive_folder_date_only_when_no_branch() {
        let dir = TempDir::new().unwrap();
        let conn = setup_db(dir.path());

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) \
             VALUES (1, 'project-a', '', 'PA')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('PA-001', 'Task', 1, 'done')",
            [],
        )
        .unwrap();
        drop(conn);

        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("project-a.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();

        assert_eq!(result.prds_archived.len(), 1);
        let folder = &result.prds_archived[0].archive_folder;
        // Should be just a date like "2026-03-04"
        assert!(
            !folder.ends_with('-'),
            "archive folder must not end with hyphen when branch is empty, got: {}",
            folder
        );
    }

    /// A completed PRD with no discoverable files on disk should still have its
    /// DB data cleared (tasks + prd_metadata row deleted).
    #[test]
    fn test_run_archive_completed_prd_no_files_clears_db() {
        let dir = TempDir::new().unwrap();

        let conn = setup_db(dir.path());

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) VALUES (1, 'ghost-project', 'main', 'GP')",
            [],
        )
        .unwrap();

        // All tasks done, but no files exist on disk
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('GP-001', 'Done task', 1, 'done')",
            [],
        )
        .unwrap();

        drop(conn);

        // No files created on disk — prd_items will be empty
        let result = run_archive(dir.path(), false).unwrap();

        // PRD should still be reported as archived (zero files is fine)
        assert_eq!(result.prds_archived.len(), 1);
        assert_eq!(result.prds_archived[0].files_archived, 0);
        assert_eq!(result.prds_archived[0].tasks_cleared, 1);
        assert_eq!(result.tasks_cleared, 1);

        // DB data must be cleared even though no files were moved
        let conn = crate::db::open_connection(dir.path()).unwrap();
        let task_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id LIKE 'GP-%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            task_count, 0,
            "DB tasks should be cleared for completed PRD with no files"
        );

        let meta_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix = 'GP'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            meta_count, 0,
            "prd_metadata row should be deleted for completed PRD with no files"
        );
    }
}
