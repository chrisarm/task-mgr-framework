//! Health check functions for the doctor command.
//!
//! These functions detect various database inconsistencies:
//! - Stale in_progress tasks without active runs
//! - Active runs without proper end
//! - Orphaned relationships referencing non-existent tasks
//! - Tasks completed in git history but not marked done in DB

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Find tasks that are in_progress but have no active run tracking them.
///
/// A task is considered stale if:
/// 1. Its status is 'in_progress'
/// 2. There is no run_tasks entry with status='started' for this task in any active run
pub fn find_stale_in_progress_tasks(conn: &Connection) -> TaskMgrResult<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT t.id, t.title
        FROM tasks t
        WHERE t.status = 'in_progress'
        AND t.archived_at IS NULL
        AND NOT EXISTS (
            SELECT 1 FROM run_tasks rt
            JOIN runs r ON rt.run_id = r.run_id
            WHERE rt.task_id = t.id
            AND rt.status = 'started'
            AND r.status = 'active'
        )
        ORDER BY t.id
        "#,
    )?;

    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Find runs that are still in 'active' status but appear abandoned.
///
/// A run is considered abandoned if:
/// 1. Its status is 'active'
/// 2. It has no ended_at timestamp
pub fn find_active_runs_without_end(conn: &Connection) -> TaskMgrResult<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT run_id, started_at
        FROM runs
        WHERE status = 'active'
        AND ended_at IS NULL
        ORDER BY started_at
        "#,
    )?;

    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Find relationships where related_id references a non-existent task.
///
/// Note: We intentionally don't have a foreign key on related_id to allow
/// importing tasks with forward references, so we check this manually.
pub fn find_orphaned_relationships(
    conn: &Connection,
) -> TaskMgrResult<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT tr.task_id, tr.related_id, tr.rel_type
        FROM task_relationships tr
        WHERE NOT EXISTS (
            SELECT 1 FROM tasks t WHERE t.id = tr.related_id AND t.archived_at IS NULL
        )
        ORDER BY tr.task_id, tr.related_id
        "#,
    )?;

    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Parse task IDs from git log commit messages.
///
/// Looks for patterns like `[FEAT-001]`, `[US-001]`, `[FIX-001]` in commit messages.
/// Returns deduplicated task IDs in the order they first appeared.
pub fn parse_task_ids_from_git_log(dir: &Path) -> TaskMgrResult<Vec<String>> {
    let output = Command::new("git")
        .args(["log", "--oneline", "--format=%s", "-n", "200"])
        .current_dir(dir)
        .output();

    let output = match output {
        Ok(o) => o,
        Err(_) => return Ok(Vec::new()), // git not available or not a repo
    };

    if !output.status.success() {
        return Ok(Vec::new()); // not a git repo or other git error
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut seen = HashSet::new();
    let mut task_ids = Vec::new();

    for line in stdout.lines() {
        for cap in extract_bracketed_task_ids(line) {
            if seen.insert(cap.clone()) {
                task_ids.push(cap);
            }
        }
    }

    Ok(task_ids)
}

/// Extract bracketed task IDs from a commit message line.
///
/// Matches patterns like `[FEAT-001]`, `[US-001]`, `[TEST-INIT-001]`.
pub(crate) fn extract_bracketed_task_ids(line: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut start = 0;

    while let Some(open) = line[start..].find('[') {
        let open_abs = start + open;
        if let Some(close) = line[open_abs..].find(']') {
            let close_abs = open_abs + close;
            let candidate = &line[open_abs + 1..close_abs];
            if is_valid_task_id(candidate) {
                ids.push(candidate.to_string());
            }
            start = close_abs + 1;
        } else {
            break;
        }
    }

    ids
}

/// Check if a string looks like a valid task ID.
///
/// Valid: `FEAT-001`, `US-001`, `TEST-INIT-005`, `FIX-001`, `CODE-REVIEW-1`
/// Invalid: empty, no hyphen, lowercase, spaces, special chars
pub(crate) fn is_valid_task_id(s: &str) -> bool {
    if s.is_empty() || s.len() > 30 {
        return false;
    }

    if !s.contains('-') {
        return false;
    }

    s.chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
}

/// Find tasks that appear in git commit history but are not marked as done in the DB.
///
/// Cross-references task IDs found in git log `[TASK-ID]` patterns against
/// the tasks table. Returns (task_id, title, commit_message) for tasks that
/// exist in the DB with status != 'done' but have a matching commit.
pub fn find_git_reconciliation_tasks(
    conn: &Connection,
    dir: &Path,
) -> TaskMgrResult<Vec<(String, String, String)>> {
    let git_task_ids = parse_task_ids_from_git_log(dir)?;

    if git_task_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();

    for task_id in &git_task_ids {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT id, title FROM tasks WHERE id = ? AND status != 'done' AND archived_at IS NULL",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        if let Some((id, title)) = row {
            let commit_msg = get_commit_message_for_task(dir, &id);
            results.push((id, title, commit_msg));
        }
    }

    Ok(results)
}

/// Get the most recent commit message that references a task ID.
fn get_commit_message_for_task(dir: &Path, task_id: &str) -> String {
    let pattern = format!("[{}]", task_id);
    let output = Command::new("git")
        .args([
            "log",
            "--oneline",
            "--grep",
            &pattern,
            "-n",
            "1",
            "--fixed-strings",
        ])
        .current_dir(dir)
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}
