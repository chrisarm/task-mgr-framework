//! Database import operations for the init command.
//!
//! This module contains all functions for inserting, updating, and deleting
//! task data in the SQLite database.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;

use crate::db::prefix::make_like_pattern;
use crate::models::TaskStatus;
use crate::TaskMgrResult;

use super::output::DryRunDeletePreview;
use super::parse::{PrdFile, PrdUserStory};

/// Drop existing data from the database.
///
/// When `task_prefix` is `Some(prefix)`, only data belonging to that PRD prefix
/// is deleted (tasks, relationships, files, and the matching prd_metadata row).
/// Learnings, runs, and other PRDs are preserved.
///
/// When `task_prefix` is `None`, all data is wiped (legacy global-force behavior).
pub fn drop_existing_data(conn: &Connection, task_prefix: Option<&str>) -> TaskMgrResult<()> {
    match task_prefix {
        Some(prefix) => {
            let pattern = make_like_pattern(prefix);
            // Delete child tables before parent (FK ordering)
            conn.execute(
                "DELETE FROM task_relationships WHERE task_id LIKE ? ESCAPE '\\'",
                [&pattern],
            )?;
            conn.execute(
                "DELETE FROM task_files WHERE task_id LIKE ? ESCAPE '\\'",
                [&pattern],
            )?;
            conn.execute("DELETE FROM tasks WHERE id LIKE ? ESCAPE '\\'", [&pattern])?;
            // prd_files must be removed before prd_metadata (FK ordering)
            conn.execute(
                "DELETE FROM prd_files WHERE prd_id = \
                 (SELECT id FROM prd_metadata WHERE task_prefix = ?)",
                [prefix],
            )?;
            conn.execute("DELETE FROM prd_metadata WHERE task_prefix = ?", [prefix])?;
        }
        None => {
            // Global wipe — preserve nothing (legacy behavior).
            // Drop in correct order due to foreign keys.
            conn.execute("DELETE FROM learning_tags", [])?;
            conn.execute("DELETE FROM learnings", [])?;
            conn.execute("DELETE FROM run_tasks", [])?;
            conn.execute("DELETE FROM runs", [])?;
            conn.execute("DELETE FROM task_relationships", [])?;
            conn.execute("DELETE FROM task_files", [])?;
            conn.execute("DELETE FROM tasks", [])?;
            // prd_files may not exist in pre-v6 databases
            let _ = conn.execute("DELETE FROM prd_files", []);
            conn.execute("DELETE FROM prd_metadata", [])?;
            // Reset global_state but don't delete the row
            conn.execute(
                "UPDATE global_state SET iteration_counter = 0, last_task_id = NULL, last_run_id = NULL",
                [],
            )?;
        }
    }
    Ok(())
}

/// Get a preview of what would be deleted in dry-run mode with --force.
///
/// When `task_prefix` is `Some`, counts only rows belonging to that prefix.
/// Learnings and runs are always reported as 0 in scoped mode (they are never deleted).
/// When `task_prefix` is `None`, counts all rows (global wipe preview).
pub fn get_delete_preview(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<DryRunDeletePreview> {
    match task_prefix {
        Some(prefix) => {
            let pattern = make_like_pattern(prefix);
            let tasks: usize = conn.query_row(
                "SELECT COUNT(*) FROM tasks WHERE id LIKE ? ESCAPE '\\'",
                [&pattern],
                |row| row.get(0),
            )?;
            let files: usize = conn.query_row(
                "SELECT COUNT(*) FROM task_files WHERE task_id LIKE ? ESCAPE '\\'",
                [&pattern],
                |row| row.get(0),
            )?;
            let relationships: usize = conn.query_row(
                "SELECT COUNT(*) FROM task_relationships WHERE task_id LIKE ? ESCAPE '\\'",
                [&pattern],
                |row| row.get(0),
            )?;
            Ok(DryRunDeletePreview {
                tasks,
                files,
                relationships,
                learnings: 0,
                runs: 0,
            })
        }
        None => {
            let tasks: usize =
                conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
            let files: usize =
                conn.query_row("SELECT COUNT(*) FROM task_files", [], |row| row.get(0))?;
            let relationships: usize =
                conn.query_row("SELECT COUNT(*) FROM task_relationships", [], |row| {
                    row.get(0)
                })?;
            let learnings: usize =
                conn.query_row("SELECT COUNT(*) FROM learnings", [], |row| row.get(0))?;
            let runs: usize = conn.query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))?;
            Ok(DryRunDeletePreview {
                tasks,
                files,
                relationships,
                learnings,
                runs,
            })
        }
    }
}

/// Check if the database is fresh (no tasks).
pub fn is_fresh_database(conn: &Connection) -> TaskMgrResult<bool> {
    let count: i32 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
    Ok(count == 0)
}

/// Get existing task IDs from the database.
pub fn get_existing_task_ids(conn: &Connection) -> TaskMgrResult<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT id FROM tasks")?;
    let ids = stmt.query_map([], |row| row.get(0))?;
    let mut result = HashSet::new();
    for id in ids {
        result.insert(id?);
    }
    Ok(result)
}

/// Insert or update PRD metadata keyed by `task_prefix`.
///
/// Uses `ON CONFLICT(task_prefix) DO UPDATE` so calling this twice with the
/// same prefix updates the existing row rather than creating a duplicate.
///
/// Returns the row id of the upserted row (new or existing).
pub fn insert_prd_metadata(
    conn: &Connection,
    prd: &PrdFile,
    raw_json: Option<&str>,
) -> TaskMgrResult<i64> {
    let priority_philosophy = prd
        .priority_philosophy
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let global_acceptance = prd
        .global_acceptance_criteria
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let review_guidelines = prd
        .review_guidelines
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    conn.execute(
        r#"INSERT INTO prd_metadata
           (project, branch_name, description, priority_philosophy,
            global_acceptance_criteria, review_guidelines, raw_json,
            external_git_repo, task_prefix, default_model, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
           ON CONFLICT(task_prefix) DO UPDATE SET
               project = excluded.project,
               branch_name = excluded.branch_name,
               description = excluded.description,
               priority_philosophy = excluded.priority_philosophy,
               global_acceptance_criteria = excluded.global_acceptance_criteria,
               review_guidelines = excluded.review_guidelines,
               raw_json = excluded.raw_json,
               external_git_repo = excluded.external_git_repo,
               default_model = excluded.default_model,
               updated_at = excluded.updated_at"#,
        rusqlite::params![
            prd.project,
            prd.branch_name,
            prd.description,
            priority_philosophy,
            global_acceptance,
            review_guidelines,
            raw_json,
            prd.external_git_repo,
            prd.task_prefix,
            prd.model,
        ],
    )?;

    // last_insert_rowid() returns 0 on ON CONFLICT DO UPDATE (no new row).
    // Query the actual id back by task_prefix to handle both insert and upsert.
    let prd_id: i64 = match &prd.task_prefix {
        Some(prefix) => conn.query_row(
            "SELECT id FROM prd_metadata WHERE task_prefix = ?1",
            [prefix],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "SELECT id FROM prd_metadata WHERE task_prefix IS NULL",
            [],
            |row| row.get(0),
        )?,
    };
    Ok(prd_id)
}

/// Insert a task into the database.
pub fn insert_task(conn: &Connection, story: &PrdUserStory) -> TaskMgrResult<()> {
    // Map passes boolean to TaskStatus
    let status = if story.passes {
        TaskStatus::Done
    } else {
        TaskStatus::Todo
    };

    // Serialize acceptance criteria as JSON array
    let acceptance_criteria = serde_json::to_string(&story.acceptance_criteria)?;

    // Serialize review_scope if present
    let review_scope = story
        .review_scope
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    conn.execute(
        r#"INSERT INTO tasks
           (id, title, description, priority, status, notes, acceptance_criteria,
            review_scope, severity, source_review, model, difficulty, escalation_note)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        rusqlite::params![
            story.id,
            story.title,
            story.description,
            story.priority,
            status.as_db_str(),
            story.notes,
            acceptance_criteria,
            review_scope,
            story.severity,
            story.source_review,
            story.model,
            story.difficulty,
            story.escalation_note,
        ],
    )?;

    Ok(())
}

/// Insert a task file into the database.
pub fn insert_task_file(conn: &Connection, task_id: &str, file_path: &str) -> TaskMgrResult<()> {
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES (?, ?)",
        [task_id, file_path],
    )?;
    Ok(())
}

/// Insert a task relationship into the database.
pub fn insert_relationship(
    conn: &Connection,
    task_id: &str,
    related_id: &str,
    rel_type: &str,
) -> TaskMgrResult<()> {
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?, ?, ?)",
        [task_id, related_id, rel_type],
    )?;
    Ok(())
}

/// Insert all relationships for a task. Returns the number of relationships inserted.
pub fn insert_task_relationships(conn: &Connection, story: &PrdUserStory) -> TaskMgrResult<usize> {
    let mut count = 0;
    for dep in &story.depends_on {
        insert_relationship(conn, &story.id, dep, "dependsOn")?;
        count += 1;
    }
    for syn in &story.synergy_with {
        insert_relationship(conn, &story.id, syn, "synergyWith")?;
        count += 1;
    }
    for batch in &story.batch_with {
        insert_relationship(conn, &story.id, batch, "batchWith")?;
        count += 1;
    }
    for conflict in &story.conflicts_with {
        insert_relationship(conn, &story.id, conflict, "conflictsWith")?;
        count += 1;
    }
    Ok(count)
}

/// Update an existing task in the database.
pub fn update_task(conn: &Connection, story: &PrdUserStory) -> TaskMgrResult<()> {
    // Serialize acceptance criteria as JSON array
    let acceptance_criteria = serde_json::to_string(&story.acceptance_criteria)?;

    // Serialize review_scope if present
    let review_scope = story
        .review_scope
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    // Note: We don't update status from passes here - the task may have been
    // completed in the DB since the JSON was written. We only update metadata.
    conn.execute(
        r#"UPDATE tasks SET
           title = ?, description = ?, priority = ?, notes = ?,
           acceptance_criteria = ?, review_scope = ?, severity = ?,
           source_review = ?, model = ?, difficulty = ?, escalation_note = ?,
           updated_at = datetime('now')
           WHERE id = ?"#,
        rusqlite::params![
            story.title,
            story.description,
            story.priority,
            story.notes,
            acceptance_criteria,
            review_scope,
            story.severity,
            story.source_review,
            story.model,
            story.difficulty,
            story.escalation_note,
            story.id,
        ],
    )?;

    Ok(())
}

/// Delete all task files for a task.
pub fn delete_task_files(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    conn.execute("DELETE FROM task_files WHERE task_id = ?", [task_id])?;
    Ok(())
}

/// Delete all relationships for a task.
pub fn delete_task_relationships(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    conn.execute(
        "DELETE FROM task_relationships WHERE task_id = ?",
        [task_id],
    )?;
    Ok(())
}

/// Insert a PRD file record into the prd_files table.
pub fn insert_prd_file(
    conn: &Connection,
    prd_id: i64,
    file_path: &str,
    file_type: &str,
) -> TaskMgrResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO prd_files (prd_id, file_path, file_type) VALUES (?, ?, ?)",
        rusqlite::params![prd_id, file_path, file_type],
    )?;
    Ok(())
}

/// Register all files associated with a PRD in the prd_files table.
///
/// Records:
/// 1. The task list JSON file as `task_list` type
/// 2. The derived prompt file (`<stem>-prompt.md`) as `prompt` type if it exists
/// 3. The PRD markdown file from `prd.prd_file` as `prd` type if set
///
/// All paths are stored relative to the tasks directory.
pub fn register_prd_files(
    conn: &Connection,
    prd_id: i64,
    json_path: &Path,
    prd: &PrdFile,
    tasks_dir: &Path,
) -> TaskMgrResult<()> {
    // Store the JSON task list path (relative to tasks dir)
    let json_relative = json_path
        .strip_prefix(tasks_dir)
        .unwrap_or(json_path)
        .to_string_lossy();
    insert_prd_file(conn, prd_id, &json_relative, "task_list")?;

    // Derive prompt file path: <stem>-prompt.md
    if let Some(stem) = json_path.file_stem() {
        let prompt_name = format!("{}-prompt.md", stem.to_string_lossy());
        let prompt_path = json_path.with_file_name(&prompt_name);
        if prompt_path.exists() {
            let prompt_relative = prompt_path
                .strip_prefix(tasks_dir)
                .unwrap_or(&prompt_path)
                .to_string_lossy();
            insert_prd_file(conn, prd_id, &prompt_relative, "prompt")?;
        }
    }

    // Store PRD markdown file if specified
    if let Some(ref prd_file) = prd.prd_file {
        insert_prd_file(conn, prd_id, prd_file, "prd")?;
    }

    Ok(())
}
