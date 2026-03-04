//! PRD metadata and pass reconciliation: read PRD state from DB, update JSON files,
//! and synchronise task completion status between the PRD JSON and the task database.

use std::path::Path;

use rusqlite::Connection;

use crate::commands::complete as complete_cmd;
use crate::db::prefix::prefix_and;
use crate::loop_engine::output_parsing::strip_task_prefix;
use crate::TaskMgrResult;

/// PRD metadata read from the database.
pub(crate) struct PrdMetadata {
    pub(crate) branch_name: Option<String>,
    pub(crate) task_count: usize,
    pub(crate) external_git_repo: Option<String>,
    pub(crate) task_prefix: Option<String>,
    /// Default model for all tasks in this PRD. Read from `prd_metadata.default_model`.
    pub(crate) default_model: Option<String>,
}

/// Read branch name, task count, external_git_repo, task_prefix, and default_model
/// from prd_metadata and tasks tables.
///
/// When `task_prefix` is provided, queries `WHERE task_prefix = ?` to select the
/// matching PRD row. Falls back to `LIMIT 1 ORDER BY id ASC` when no prefix is given.
pub(crate) fn read_prd_metadata(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<PrdMetadata> {
    let (branch_name, external_git_repo, task_prefix, default_model): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = if let Some(prefix) = task_prefix {
        conn.query_row(
            "SELECT branch_name, external_git_repo, task_prefix, default_model FROM prd_metadata WHERE task_prefix = ?1",
            rusqlite::params![prefix],
            |row| {
                Ok((
                    row.get("branch_name")?,
                    row.get("external_git_repo")?,
                    row.get("task_prefix")?,
                    row.get("default_model")?,
                ))
            },
        )
        .unwrap_or((None, None, None, None))
    } else {
        conn.query_row(
            "SELECT branch_name, external_git_repo, task_prefix, default_model FROM prd_metadata ORDER BY id ASC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get("branch_name")?,
                    row.get("external_git_repo")?,
                    row.get("task_prefix")?,
                    row.get("default_model")?,
                ))
            },
        )
        .unwrap_or((None, None, None, None))
    };

    let (tc_pfx_clause, tc_pfx_param) = prefix_and(task_prefix.as_deref());
    let tc_sql = format!(
        "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant') {tc_pfx_clause}"
    );
    let tc_params: Vec<&dyn rusqlite::types::ToSql> = match &tc_pfx_param {
        Some(p) => vec![p],
        None => vec![],
    };
    let task_count: usize = conn
        .query_row(&tc_sql, tc_params.as_slice(), |row| row.get::<_, i64>(0))
        .map(|c| c as usize)
        .unwrap_or(0);

    Ok(PrdMetadata {
        branch_name,
        task_count,
        external_git_repo,
        task_prefix,
        default_model,
    })
}

/// Update a task's `passes` field in the PRD JSON file.
///
/// Reads the PRD, finds the task by ID, updates `passes`, and writes back atomically.
/// Also tries the base ID (prefix stripped) since PRD JSON has unprefixed IDs while DB has prefixed IDs.
pub(crate) fn update_prd_task_passes(
    prd_path: &Path,
    task_id: &str,
    passes: bool,
    task_prefix: Option<&str>,
) -> crate::TaskMgrResult<()> {
    use std::fs;

    // Read the PRD file
    let content =
        fs::read_to_string(prd_path).map_err(|e| crate::TaskMgrError::IoErrorWithContext {
            file_path: prd_path.display().to_string(),
            operation: "reading PRD file".to_string(),
            source: e,
        })?;

    // Parse as generic JSON Value to preserve structure
    let mut prd: serde_json::Value = serde_json::from_str(&content)?;

    // Try full ID first, then base ID (prefix-stripped) since PRD JSON stores unprefixed IDs
    let base_id = strip_task_prefix(task_id, task_prefix);

    // Find and update the task in userStories
    let updated = if let Some(stories) = prd.get_mut("userStories").and_then(|v| v.as_array_mut()) {
        let mut found = false;
        for story in stories.iter_mut() {
            let story_id = story.get("id").and_then(|v| v.as_str());
            if story_id == Some(task_id) || story_id == Some(base_id) {
                story["passes"] = serde_json::Value::Bool(passes);
                found = true;
                break;
            }
        }
        found
    } else {
        false
    };

    if !updated {
        return Err(crate::TaskMgrError::NotFound {
            resource_type: "Task in PRD".to_string(),
            id: task_id.to_string(),
        });
    }

    // Write back atomically
    let tmp_path = prd_path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(&prd)?;
    fs::write(&tmp_path, &json).map_err(|e| crate::TaskMgrError::IoErrorWithContext {
        file_path: tmp_path.display().to_string(),
        operation: "writing temp PRD file".to_string(),
        source: e,
    })?;
    fs::rename(&tmp_path, prd_path).map_err(|e| crate::TaskMgrError::IoErrorWithContext {
        file_path: prd_path.display().to_string(),
        operation: "renaming temp PRD file".to_string(),
        source: e,
    })?;

    Ok(())
}

/// Mark a task as done in the DB and update the PRD JSON.
///
/// Consolidates the repeated pattern of complete + PRD update used by
/// git-check, output-scan, and already-complete detection paths.
pub(crate) fn mark_task_done(
    conn: &mut Connection,
    task_id: &str,
    run_id: &str,
    commit_hash: Option<&str>,
    prd_path: &Path,
    task_prefix: Option<&str>,
) -> Result<(), crate::TaskMgrError> {
    let task_ids = [task_id.to_string()];
    complete_cmd::complete(conn, &task_ids, Some(run_id), commit_hash, false)?;
    if let Err(e) = update_prd_task_passes(prd_path, task_id, true, task_prefix) {
        eprintln!("Warning: failed to update PRD for task {}: {}", task_id, e);
    }
    Ok(())
}

/// Reconcile non-done tasks against PRD passes status.
///
/// If a task is `todo` or `in_progress` in the DB but has `passes: true` in the PRD JSON,
/// it was completed but the DB was never updated (e.g., rate limit interrupted
/// git detection, or a previous loop exit reset it). Mark it `done` to prevent
/// infinite re-selection loops.
pub(crate) fn reconcile_passes_with_db(
    conn: &Connection,
    prd_path: &Path,
    task_prefix: Option<&str>,
) {
    use std::fs;

    // Get all todo/in_progress task IDs from the DB, scoped to this PRD's prefix.
    let (rpdb_pfx_clause, rpdb_pfx_param) = prefix_and(task_prefix);
    let rpdb_sql =
        format!("SELECT id FROM tasks WHERE status IN ('todo', 'in_progress') {rpdb_pfx_clause}");
    let mut stmt = match conn.prepare(&rpdb_sql) {
        Ok(s) => s,
        Err(_) => return,
    };
    let rpdb_params: Vec<&dyn rusqlite::types::ToSql> = match &rpdb_pfx_param {
        Some(p) => vec![p],
        None => vec![],
    };
    let candidate_ids: Vec<String> = stmt
        .query_map(rpdb_params.as_slice(), |row| row.get(0))
        .ok()
        .map(|rows| {
            rows.filter_map(|r: rusqlite::Result<String>| r.ok())
                .collect()
        })
        .unwrap_or_default();

    if candidate_ids.is_empty() {
        return;
    }

    // Read PRD and build a set of task IDs with passes: true
    let content = match fs::read_to_string(prd_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let prd: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return,
    };

    let stories = match prd.get("userStories").and_then(|v| v.as_array()) {
        Some(s) => s,
        None => return,
    };

    // Build passing_ids with full prefixed IDs: prepend task prefix to each PRD story ID
    let passing_ids: std::collections::HashSet<String> = stories
        .iter()
        .filter(|s| s.get("passes").and_then(|v| v.as_bool()) == Some(true))
        .filter_map(|s| s.get("id").and_then(|v| v.as_str()))
        .flat_map(|id| {
            let mut ids = vec![id.to_string()];
            if let Some(pfx) = task_prefix {
                ids.push(format!("{}-{}", pfx, id));
            }
            ids
        })
        .collect();

    // Loop until convergence: marking a task done may unblock dependents that also have
    // passes: true. A single pass can miss them if checked before their dependency.
    loop {
        let mut updated_count = 0;
        for task_id in &candidate_ids {
            // Re-check status — may have been marked done in a previous pass
            let still_open: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM tasks WHERE id = ? AND status IN ('todo', 'in_progress')",
                    [task_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);
            if !still_open {
                continue;
            }

            if passing_ids.contains(task_id.as_str()) {
                if !complete_cmd::are_dependencies_satisfied(conn, task_id) {
                    continue;
                }
                if let Ok(1) = conn.execute(
                    "UPDATE tasks SET status = 'done', completed_at = datetime('now') WHERE id = ? AND status IN ('todo', 'in_progress')",
                    [task_id.as_str()],
                ) {
                    eprintln!(
                        "Reconciled task {} as done (passes: true in PRD but was not done in DB)",
                        task_id
                    );
                    updated_count += 1;
                }
            }
        }
        if updated_count == 0 {
            break;
        }
    }
}

/// Compute an MD5 hash of a file's contents.
///
/// Returns the hex-encoded hash, or an empty string if the file cannot be read.
/// An empty string means "unknown" — the next call will re-hash and detect any change.
pub(crate) fn hash_file(path: &Path) -> String {
    std::fs::read(path)
        .map(|bytes| format!("{:x}", md5::compute(&bytes)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- update_prd_task_passes tests ---

    #[test]
    fn test_update_prd_task_passes_sets_true() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");

        let prd = r#"{
            "project": "Test",
            "userStories": [
                {"id": "TASK-001", "title": "Test", "passes": false},
                {"id": "TASK-002", "title": "Other", "passes": false}
            ]
        }"#;
        std::fs::write(&prd_path, prd).unwrap();

        update_prd_task_passes(&prd_path, "TASK-001", true, None).unwrap();

        let content = std::fs::read_to_string(&prd_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let task = &parsed["userStories"][0];
        assert_eq!(task["passes"], true);
        // Other task unchanged
        assert_eq!(parsed["userStories"][1]["passes"], false);
    }

    #[test]
    fn test_update_prd_task_passes_not_found() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");

        let prd = r#"{"project": "Test", "userStories": []}"#;
        std::fs::write(&prd_path, prd).unwrap();

        let result = update_prd_task_passes(&prd_path, "NONEXISTENT", true, None);
        assert!(result.is_err());
    }

    // --- hash_file tests ---

    #[test]
    fn test_hash_file_returns_consistent_hash() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("test.json");
        std::fs::write(&path, r#"{"tasks": [1, 2, 3]}"#).unwrap();

        let hash1 = hash_file(&path);
        let hash2 = hash_file(&path);
        assert_eq!(hash1, hash2, "Same content should produce same hash");
        assert!(
            !hash1.is_empty(),
            "Hash should not be empty for readable file"
        );
    }

    #[test]
    fn test_hash_file_detects_change() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("test.json");

        std::fs::write(&path, r#"{"tasks": [1, 2]}"#).unwrap();
        let hash_before = hash_file(&path);

        std::fs::write(&path, r#"{"tasks": [1, 2, 3]}"#).unwrap();
        let hash_after = hash_file(&path);

        assert_ne!(
            hash_before, hash_after,
            "Different content should produce different hash"
        );
    }

    #[test]
    fn test_hash_file_missing_file_returns_empty() {
        let hash = hash_file(Path::new("/nonexistent/file.json"));
        assert!(hash.is_empty(), "Missing file should return empty string");
    }

    #[test]
    fn test_update_prd_passes_with_prefix() {
        // PRD has unprefixed "FIX-001", called with prefixed "aeb10a1f-FIX-001"
        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");

        let prd = r#"{
            "project": "Test",
            "userStories": [
                {"id": "FIX-001", "title": "Fix bug", "passes": false}
            ]
        }"#;
        std::fs::write(&prd_path, prd).unwrap();

        update_prd_task_passes(&prd_path, "aeb10a1f-FIX-001", true, Some("aeb10a1f")).unwrap();

        let content = std::fs::read_to_string(&prd_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["userStories"][0]["passes"], true,
            "Should update via base ID fallback"
        );
    }

    #[test]
    fn test_reconcile_passes_resolves_dependency_chains() {
        use crate::loop_engine::test_utils::setup_test_db;
        use std::io::Write;

        // Set up DB with A → B → C dependency chain, all todo
        let (temp_dir, conn) = setup_test_db();

        // Insert tasks in reverse order (C, B, A) to stress ordering
        for (id, status) in &[("C", "todo"), ("B", "todo"), ("A", "todo")] {
            conn.execute(
                "INSERT INTO tasks (id, title, status, priority) VALUES (?, 'Test', ?, 10)",
                rusqlite::params![id, status],
            )
            .unwrap();
        }
        // B depends on A, C depends on B
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('B', 'A', 'dependsOn')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('C', 'B', 'dependsOn')",
            [],
        ).unwrap();

        // Write PRD JSON with all three tasks having passes: true
        let prd_path = temp_dir.path().join("prd.json");
        let mut f = std::fs::File::create(&prd_path).unwrap();
        f.write_all(
            br#"{"userStories":[
                {"id":"A","passes":true},
                {"id":"B","passes":true},
                {"id":"C","passes":true}
            ]}"#,
        )
        .unwrap();

        reconcile_passes_with_db(&conn, &prd_path, None);

        // All three should be done
        for id in &["A", "B", "C"] {
            let status: String = conn
                .query_row("SELECT status FROM tasks WHERE id = ?", [id], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                status, "done",
                "Task {} should be done after reconciliation",
                id
            );
        }
    }

    #[test]
    fn test_reconcile_passes_with_db_only_marks_p1_tasks_done() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        // P1: both in_progress and todo; P2: one todo
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('P1-TASK-001', 'P1 in_progress', 'in_progress', 1),
             ('P1-TASK-002', 'P1 todo',        'todo',        2),
             ('P2-TASK-001', 'P2 todo',         'todo',        1);",
        )
        .unwrap();

        // PRD JSON where P1-TASK-001 has passes: true (using base ID TASK-001)
        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");
        let prd_json = serde_json::json!({
            "userStories": [
                {"id": "TASK-001", "passes": true},
                {"id": "TASK-002", "passes": false}
            ]
        });
        std::fs::write(&prd_path, prd_json.to_string()).unwrap();

        reconcile_passes_with_db(&conn, &prd_path, Some("P1"));

        // P1-TASK-001 (base id TASK-001, passes: true) should be done
        let p1_001_status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'P1-TASK-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            p1_001_status, "done",
            "P1-TASK-001 should be reconciled to done"
        );

        // P1-TASK-002 (passes: false) should remain todo
        let p1_002_status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'P1-TASK-002'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(p1_002_status, "todo", "P1-TASK-002 should remain todo");

        // P2-TASK-001 must NOT be touched (different prefix)
        let p2_status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'P2-TASK-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            p2_status, "todo",
            "P2-TASK-001 must not be affected by P1 reconciliation"
        );
    }

    #[test]
    fn test_reconcile_passes_with_db_none_prefix_marks_all_matching() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('TASK-001', 'Task 1', 'in_progress', 1),
             ('TASK-002', 'Task 2', 'todo',        2);",
        )
        .unwrap();

        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");
        let prd_json = serde_json::json!({
            "userStories": [
                {"id": "TASK-001", "passes": true},
                {"id": "TASK-002", "passes": false}
            ]
        });
        std::fs::write(&prd_path, prd_json.to_string()).unwrap();

        reconcile_passes_with_db(&conn, &prd_path, None);

        let status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'TASK-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "done",
            "None prefix should reconcile matching tasks"
        );
    }
}
