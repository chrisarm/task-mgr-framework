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
use crate::TaskMgrResult;

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
/// Scans the DB for fully-completed PRDs (all tasks done), moves their
/// associated files to `tasks/archive/YYYY-MM-DD-<branch>/`, and extracts
/// learnings from `progress.txt` into `tasks/learnings.md`.
pub fn run_archive(dir: &Path, dry_run: bool) -> TaskMgrResult<ArchiveResult> {
    let conn = open_connection(dir)?;

    // Get project info from prd_metadata
    let project_info = get_project_info(&conn)?;
    let Some(info) = project_info else {
        return Ok(ArchiveResult {
            archived: Vec::new(),
            learnings_extracted: 0,
            tasks_cleared: 0,
            dry_run,
            message: "No PRD metadata found in database.".to_string(),
        });
    };

    // Check if PRD is fully completed
    if !is_prd_completed(&conn)? {
        return Ok(ArchiveResult {
            archived: Vec::new(),
            learnings_extracted: 0,
            tasks_cleared: 0,
            dry_run,
            message: format!(
                "PRD '{}' is not fully completed. Only completed PRDs can be archived.",
                info.project
            ),
        });
    }

    // Derive archive folder name from branch
    let branch_slug = strip_branch_prefix(&info.branch.unwrap_or_default());
    let date_str = Local::now().format("%Y-%m-%d").to_string();
    let archive_folder_name = if branch_slug.is_empty() {
        date_str.clone()
    } else {
        format!("{}-{}", date_str, branch_slug)
    };

    let tasks_dir = dir.join("tasks");
    let archive_dir = tasks_dir.join("archive").join(&archive_folder_name);

    // Extract learnings from progress.txt BEFORE moving files
    let progress_path = tasks_dir.join("progress.txt");
    let learnings_count = if progress_path.exists() {
        let learnings = extract_learnings_from_progress(&progress_path)?;
        if !learnings.is_empty() && !dry_run {
            append_learnings_to_file(&tasks_dir.join("learnings.md"), &learnings)?;
        }
        learnings.len()
    } else {
        0
    };

    // Discover files to archive (prefer prd_files table, fall back to project name)
    let files_to_archive = discover_archivable_files(&conn, &tasks_dir, &info.project)?;

    let mut archived_items = Vec::new();
    for source in &files_to_archive {
        let file_name = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let dest = archive_dir.join(&file_name);

        archived_items.push(ArchivedItem {
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

    // Count tasks before clearing (for reporting)
    let task_count: usize = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .map_err(crate::TaskMgrError::DatabaseError)?;

    // Clear task data from DB after archiving files (preserving learnings)
    if !dry_run && !archived_items.is_empty() {
        clear_task_data(&conn)?;
    }

    let action = if dry_run { "Would archive" } else { "Archived" };
    let clear_action = if dry_run { "Would clear" } else { "Cleared" };
    let message = if archived_items.is_empty() {
        format!("No archivable files found for project '{}'.", info.project)
    } else {
        format!(
            "{} {} file(s) to archive/{}. {} learning(s) extracted. {} {} task(s) from database.",
            action,
            archived_items.len(),
            archive_folder_name,
            learnings_count,
            clear_action,
            task_count
        )
    };

    Ok(ArchiveResult {
        archived: archived_items,
        learnings_extracted: learnings_count,
        tasks_cleared: task_count,
        dry_run,
        message,
    })
}

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

/// Project info from prd_metadata.
struct PrdInfo {
    project: String,
    branch: Option<String>,
}

/// Get project info from prd_metadata table.
fn get_project_info(conn: &rusqlite::Connection) -> TaskMgrResult<Option<PrdInfo>> {
    let mut stmt = conn
        .prepare("SELECT project, branch_name FROM prd_metadata WHERE id = 1")
        .map_err(crate::TaskMgrError::DatabaseError)?;

    let result = stmt
        .query_row([], |row| {
            Ok(PrdInfo {
                project: row.get(0)?,
                branch: row.get(1)?,
            })
        })
        .optional()
        .map_err(crate::TaskMgrError::DatabaseError)?;

    Ok(result)
}

/// Check if all tasks in the PRD are in a terminal state.
///
/// A PRD is archivable when no tasks are `todo`, `in_progress`, or `blocked`.
/// Terminal states: `done`, `skipped`, `irrelevant`.
fn is_prd_completed(conn: &rusqlite::Connection) -> TaskMgrResult<bool> {
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .map_err(crate::TaskMgrError::DatabaseError)?;

    if total == 0 {
        return Ok(false);
    }

    let non_terminal: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE status IN ('todo', 'in_progress', 'blocked')",
            [],
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
/// Always includes `progress.txt` if it exists (not tracked in prd_files).
fn discover_archivable_files(
    conn: &rusqlite::Connection,
    tasks_dir: &Path,
    project: &str,
) -> TaskMgrResult<Vec<PathBuf>> {
    let mut files = Vec::new();

    // Try prd_files table first (v6+ databases)
    let prd_file_paths = query_prd_files(conn);

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

    // Always include progress.txt (not tracked in prd_files)
    let progress_path = tasks_dir.join("progress.txt");
    if progress_path.exists() {
        files.push(progress_path);
    }

    Ok(files)
}

/// Query the prd_files table for file paths. Returns empty vec if table doesn't exist.
fn query_prd_files(conn: &rusqlite::Connection) -> Vec<String> {
    let result: Result<Vec<String>, rusqlite::Error> = (|| {
        let mut stmt = conn.prepare("SELECT file_path FROM prd_files WHERE prd_id = 1")?;
        let paths = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(paths)
    })();

    result.unwrap_or_default()
}

/// Clear task data from the database, preserving learnings.
///
/// Deletes from: run_tasks, runs, task_relationships, task_files, tasks,
/// prd_files, prd_metadata. Resets global_state counters.
/// Preserves: learnings, learning_tags.
fn clear_task_data(conn: &rusqlite::Connection) -> TaskMgrResult<()> {
    conn.execute("DELETE FROM run_tasks", [])
        .map_err(crate::TaskMgrError::DatabaseError)?;
    conn.execute("DELETE FROM runs", [])
        .map_err(crate::TaskMgrError::DatabaseError)?;
    conn.execute("DELETE FROM task_relationships", [])
        .map_err(crate::TaskMgrError::DatabaseError)?;
    conn.execute("DELETE FROM task_files", [])
        .map_err(crate::TaskMgrError::DatabaseError)?;
    conn.execute("DELETE FROM tasks", [])
        .map_err(crate::TaskMgrError::DatabaseError)?;
    // prd_files may not exist in pre-v6 databases
    let _ = conn.execute("DELETE FROM prd_files", []);
    conn.execute("DELETE FROM prd_metadata", [])
        .map_err(crate::TaskMgrError::DatabaseError)?;
    conn.execute(
        "UPDATE global_state SET iteration_counter = 0, last_task_id = NULL, last_run_id = NULL, updated_at = datetime('now') WHERE id = 1",
        [],
    )
    .map_err(crate::TaskMgrError::DatabaseError)?;
    Ok(())
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

// Import optional() for rusqlite
use rusqlite::OptionalExtension;

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

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        // Create some files
        fs::write(tasks_dir.join("my-project.json"), "{}").unwrap();
        fs::write(tasks_dir.join("my-project-prompt.md"), "# Prompt").unwrap();
        fs::write(tasks_dir.join("prd-my-project.md"), "# PRD").unwrap();
        fs::write(tasks_dir.join("progress.txt"), "# Progress").unwrap();
        fs::write(tasks_dir.join("unrelated.txt"), "other").unwrap();

        let files = discover_archivable_files(&conn, tasks_dir, "my-project").unwrap();
        assert_eq!(files.len(), 4);

        let filenames: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(filenames.contains(&"my-project.json".to_string()));
        assert!(filenames.contains(&"my-project-prompt.md".to_string()));
        assert!(filenames.contains(&"prd-my-project.md".to_string()));
        assert!(filenames.contains(&"progress.txt".to_string()));
        assert!(!filenames.contains(&"unrelated.txt".to_string()));
    }

    #[test]
    fn test_discover_archivable_files_none_exist() {
        let dir = TempDir::new().unwrap();
        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        let files = discover_archivable_files(&conn, dir.path(), "nonexistent").unwrap();
        assert!(files.is_empty());
    }

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
        };

        assert_eq!(result.archived.len(), 1);
        assert_eq!(result.learnings_extracted, 5);
        assert!(result.dry_run);
        assert_eq!(result.archived[0].source, "a.json");
        assert_eq!(result.archived[0].destination, "archive/dir/a.json");
    }

    #[test]
    fn test_run_archive_no_metadata() {
        let dir = TempDir::new().unwrap();

        // Create DB with schema but no metadata
        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();
        drop(conn);

        let result = run_archive(dir.path(), false).unwrap();
        assert!(result.archived.is_empty());
        assert!(result.message.contains("No PRD metadata"));
    }

    #[test]
    fn test_run_archive_incomplete_prd() {
        let dir = TempDir::new().unwrap();

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        // Insert metadata
        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name) VALUES (1, 'test-project', 'feat/my-feature')",
            [],
        )
        .unwrap();

        // Insert a task that is NOT done
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('FEAT-001', 'Test task', 1, 'todo')",
            [],
        )
        .unwrap();

        drop(conn);

        let result = run_archive(dir.path(), false).unwrap();
        assert!(result.archived.is_empty());
        assert!(result.message.contains("not fully completed"));
    }

    #[test]
    fn test_run_archive_dry_run_completed_prd() {
        let dir = TempDir::new().unwrap();

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name) VALUES (1, 'test-project', 'feat/my-feature')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('FEAT-001', 'Test task', 1, 'done')",
            [],
        )
        .unwrap();

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

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name) VALUES (1, 'test-project', 'ralph/test-branch')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('FEAT-001', 'Done task', 1, 'done')",
            [],
        )
        .unwrap();

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

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name) VALUES (1, 'test-project', 'main')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('FEAT-001', 'Done', 1, 'done')",
            [],
        )
        .unwrap();

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

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name) VALUES (1, 'test-project', 'main')",
            [],
        )
        .unwrap();
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

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name) VALUES (1, 'test-project', 'main')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('FEAT-001', 'Done', 1, 'done')",
            [],
        )
        .unwrap();

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

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        // Insert prd_files entries (simulating what init would do)
        conn.execute(
            "INSERT INTO prd_metadata (id, project) VALUES (1, 'model-selection')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, 'prd-model-phase1.json', 'task_list')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, 'prd-model-phase1-prompt.md', 'prompt')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, 'prd-model-selection.md', 'prd')",
            [],
        )
        .unwrap();

        // Create the files on disk
        let tasks_dir = dir.path();
        fs::write(tasks_dir.join("prd-model-phase1.json"), "{}").unwrap();
        fs::write(tasks_dir.join("prd-model-phase1-prompt.md"), "# Prompt").unwrap();
        fs::write(tasks_dir.join("prd-model-selection.md"), "# PRD").unwrap();
        // Also create a project-name file that should NOT be found
        // (prd_files takes precedence over project-name guessing)
        fs::write(tasks_dir.join("model-selection.json"), "{}").unwrap();

        let files = discover_archivable_files(&conn, tasks_dir, "model-selection").unwrap();

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

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO prd_metadata (id, project, branch_name) VALUES (1, 'test-project', 'main')",
            [],
        )
        .unwrap();

        // Mix of done, skipped, and irrelevant — all terminal
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('T-001', 'Done', 1, 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('T-002', 'Skipped', 2, 'skipped')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('T-003', 'Irrelevant', 3, 'irrelevant')",
            [],
        )
        .unwrap();

        // PRD should be considered completed
        assert!(is_prd_completed(&conn).unwrap());

        drop(conn);

        // Create tasks dir with files
        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("test-project.json"), "{}").unwrap();

        let result = run_archive(dir.path(), false).unwrap();
        assert_eq!(result.tasks_cleared, 3);
        assert!(!result.archived.is_empty());
    }

    #[test]
    fn test_skipped_task_blocks_archive_when_mixed_with_todo() {
        let dir = TempDir::new().unwrap();

        let mut conn = crate::db::open_connection(dir.path()).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::migrations::run_migrations(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO prd_metadata (id, project) VALUES (1, 'test-project')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('T-001', 'Done', 1, 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('T-002', 'Todo', 2, 'todo')",
            [],
        )
        .unwrap();

        // PRD should NOT be considered completed (todo task remains)
        assert!(!is_prd_completed(&conn).unwrap());
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
}
