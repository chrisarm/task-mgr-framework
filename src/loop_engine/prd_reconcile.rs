//! PRD metadata and pass reconciliation: read PRD state from DB, update JSON files,
//! and synchronise task completion status between the PRD JSON and the task database.

use std::path::Path;

use rusqlite::Connection;

use crate::commands::complete as complete_cmd;
use crate::commands::dependency_checker;
use crate::db::prefix::prefix_and;
use crate::loop_engine::claude;
use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::model::SONNET_MODEL;
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
        "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL {tc_pfx_clause}"
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
        format!("SELECT id FROM tasks WHERE status IN ('todo', 'in_progress') AND archived_at IS NULL {rpdb_pfx_clause}");
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
                    "SELECT COUNT(*) FROM tasks WHERE id = ? AND status IN ('todo', 'in_progress') AND archived_at IS NULL",
                    [task_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);
            if !still_open {
                continue;
            }

            if passing_ids.contains(task_id.as_str()) {
                if !dependency_checker::are_dependencies_satisfied(conn, task_id) {
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

/// Summary counts from a PRD mutation operation.
struct MutationStats {
    modified: usize,
    added: usize,
    irrelevant: usize,
}

/// Build the prompt sent to Claude for PRD task mutation.
fn build_mutation_prompt(human_feedback: &str, todo_tasks_json: &str) -> String {
    format!(
        r#"You are a task management assistant. A human reviewer has provided feedback after completing a checkpoint task.

## Human Feedback
{human_feedback}

## Remaining Todo Tasks (JSON)
{todo_tasks_json}

## Instructions
Analyze the feedback and suggest modifications to the remaining tasks. Output ONLY a JSON array of task modifications — no prose, no markdown fences, no explanation.

Format:
[
  {{"id": "TASK-ID", "action": "modify", "fields": {{"notes": "updated notes"}}}},
  {{"id": "TASK-ID", "action": "irrelevant"}},
  {{"id": "NEW-001", "action": "add", "title": "New task title", "description": "What needs to be done", "priority": 10}}
]

Rules:
- action "modify": update specific fields on an existing task (title, description, notes, priority, acceptanceCriteria)
- action "irrelevant": mark a task as no longer needed based on the feedback
- action "add": introduce a new task required by the feedback (assign an ID like NEW-001, NEW-002, etc.)
- Only include tasks that need changes — omit unchanged tasks
- For "modify": include only fields that should change
- Output ONLY the JSON array. If no changes are needed, output: []
"#
    )
}

/// Extract a JSON array from Claude's output.
///
/// Finds the first `[` and matching `]` in the output and attempts to parse
/// that substring as a JSON array. Returns `None` if no valid array is found.
fn parse_mutation_output(output: &str) -> Option<Vec<serde_json::Value>> {
    let start = output.find('[')?;
    // Find the last `]` after `start`
    let end = output[start..].rfind(']').map(|i| start + i)?;
    let json_str = &output[start..=end];
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    parsed.as_array().cloned()
}

/// Apply a list of modifications to the PRD JSON `Value` in-memory.
///
/// Returns the updated PRD value and mutation statistics, or an error string
/// if the PRD structure is invalid. Unknown actions are logged and skipped.
fn apply_modifications_to_prd(
    mut prd: serde_json::Value,
    modifications: &[serde_json::Value],
    task_prefix: Option<&str>,
) -> Result<(serde_json::Value, MutationStats), String> {
    let mut stats = MutationStats {
        modified: 0,
        added: 0,
        irrelevant: 0,
    };

    let stories = prd
        .get_mut("userStories")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| "PRD has no userStories array".to_string())?;

    for modification in modifications {
        let id = modification
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let action = modification
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        // Strip prefix so Claude-provided IDs (base or prefixed) both match PRD JSON entries.
        let base_id = strip_task_prefix(id, task_prefix);

        match action {
            "modify" => {
                let found = stories.iter_mut().find(|s| {
                    let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                    sid == id || sid == base_id
                });
                if let Some(story) = found {
                    if let Some(fields) = modification.get("fields").and_then(|v| v.as_object()) {
                        for (key, value) in fields {
                            match key.as_str() {
                                "title" | "description" | "notes" | "priority"
                                | "acceptanceCriteria" => {
                                    story[key] = value.clone();
                                }
                                _ => {
                                    eprintln!(
                                        "Warning: task mutation - field '{}' is not whitelisted and was skipped",
                                        key
                                    );
                                }
                            }
                        }
                        stats.modified += 1;
                    }
                } else {
                    eprintln!(
                        "Warning: task mutation - 'modify' target '{}' not found in PRD",
                        id
                    );
                }
            }
            "irrelevant" => {
                // Mark status field in PRD JSON so re-import picks it up.
                // PRD schema uses passes: true for done and no explicit "irrelevant" field,
                // but we add a "mutationStatus" hint for the DB sync step.
                let found = stories.iter_mut().find(|s| {
                    let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                    sid == id || sid == base_id
                });
                if let Some(story) = found {
                    story["mutationStatus"] = serde_json::Value::String("irrelevant".to_string());
                    stats.irrelevant += 1;
                } else {
                    eprintln!(
                        "Warning: task mutation - 'irrelevant' target '{}' not found in PRD",
                        id
                    );
                }
            }
            "add" => {
                let mut new_story = modification.clone();
                // Remove "action" — it's not a valid PRD task field
                if let Some(obj) = new_story.as_object_mut() {
                    obj.remove("action");
                    // Ensure required fields have sensible defaults
                    obj.entry("passes")
                        .or_insert(serde_json::Value::Bool(false));
                }
                stories.push(new_story);
                stats.added += 1;
            }
            _ => {
                eprintln!(
                    "Warning: task mutation - unknown action '{}' for task '{}'",
                    action, id
                );
            }
        }
    }

    Ok((prd, stats))
}

/// Apply mutation modifications directly to the database.
///
/// Handles "modify" (field updates), "irrelevant" (status change), and "add"
/// (new task insert). Errors are logged per-task but never propagate — the PRD
/// JSON is the source of truth and will sync the DB on the next iteration.
fn sync_mutations_to_db(
    conn: &Connection,
    modifications: &[serde_json::Value],
    task_prefix: Option<&str>,
) {
    for modification in modifications {
        let id = modification
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let action = modification
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        // Build the full DB id: prefix it if it's a base ID without prefix.
        let base_id = strip_task_prefix(id, task_prefix);
        let db_id = match task_prefix {
            Some(pfx) if base_id == id => format!("{pfx}-{id}"),
            _ => id.to_string(),
        };

        match action {
            "modify" => {
                if let Some(fields) = modification.get("fields").and_then(|v| v.as_object()) {
                    for (key, value) in fields {
                        let col = match key.as_str() {
                            "title" => "title",
                            "description" => "description",
                            "notes" => "notes",
                            "priority" => "priority",
                            "acceptanceCriteria" => "acceptance_criteria",
                            _ => continue,
                        };
                        let sql_val = if col == "acceptance_criteria" {
                            serde_json::to_string(value).unwrap_or_default()
                        } else if let Some(s) = value.as_str() {
                            s.to_string()
                        } else {
                            value.to_string()
                        };
                        let sql = format!(
                            "UPDATE tasks SET {col} = ?, updated_at = datetime('now') WHERE id = ?"
                        );
                        if let Err(e) = conn.execute(&sql, rusqlite::params![sql_val, db_id]) {
                            eprintln!(
                                "Warning: task mutation DB update failed for {}/{}: {}",
                                db_id, col, e
                            );
                        }
                    }
                }
            }
            "irrelevant" => {
                if let Err(e) = conn.execute(
                    "UPDATE tasks SET status = 'irrelevant', updated_at = datetime('now') \
                     WHERE id = ? AND status IN ('todo', 'in_progress')",
                    [db_id.as_str()],
                ) {
                    eprintln!(
                        "Warning: task mutation DB update failed marking {} irrelevant: {}",
                        db_id, e
                    );
                }
            }
            "add" => {
                let title = modification
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Untitled");
                let description = modification.get("description").and_then(|v| v.as_str());
                let priority = modification
                    .get("priority")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(50) as i32;
                let notes = modification.get("notes").and_then(|v| v.as_str());
                if let Err(e) = conn.execute(
                    "INSERT OR IGNORE INTO tasks \
                     (id, title, description, priority, status, notes) \
                     VALUES (?, ?, ?, ?, 'todo', ?)",
                    rusqlite::params![db_id, title, description, priority, notes],
                ) {
                    eprintln!(
                        "Warning: task mutation DB insert failed for {}: {}",
                        db_id, e
                    );
                }
            }
            _ => {} // already warned in apply_modifications_to_prd
        }
    }
}

/// Spawn a Claude call to process human review feedback and mutate downstream tasks.
///
/// Called after `handle_human_review` returns `true`. Reads remaining todo tasks
/// from the PRD JSON, builds a focused mutation prompt, spawns Claude, parses the
/// JSON-array response, and applies modifications atomically to the PRD file and
/// DB. A `.json.bak` backup is created before any writes.
///
/// Gracefully degrades on any failure (spawn, parse, write): logs the error and
/// returns without panicking. Session guidance from the review is still applied
/// to the running session regardless.
pub(crate) fn mutate_prd_from_feedback(
    prd_path: &Path,
    human_feedback: &str,
    conn: &Connection,
    task_prefix: Option<&str>,
    model: Option<&str>,
    permission_mode: &PermissionMode,
) {
    if human_feedback.trim().is_empty() {
        return;
    }

    // 1. Read and parse the PRD file
    let content = match std::fs::read_to_string(prd_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: task mutation skipped - could not read PRD: {}", e);
            return;
        }
    };
    let prd: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "Warning: task mutation skipped - could not parse PRD: {}",
                e
            );
            return;
        }
    };

    // 2. Collect remaining todo tasks (passes: false, not marked irrelevant)
    let todo_tasks: Vec<&serde_json::Value> = prd
        .get("userStories")
        .and_then(|v| v.as_array())
        .map(|stories| {
            stories
                .iter()
                .filter(|s| {
                    s.get("passes").and_then(|v| v.as_bool()) != Some(true)
                        && s.get("mutationStatus").and_then(|v| v.as_str()) != Some("irrelevant")
                })
                .collect()
        })
        .unwrap_or_default();

    if todo_tasks.is_empty() {
        eprintln!("Task mutation skipped: no todo tasks remaining in PRD.");
        return;
    }

    // 3. Build mutation prompt
    let todo_json = match serde_json::to_string_pretty(&todo_tasks) {
        Ok(j) => j,
        Err(e) => {
            eprintln!(
                "Warning: task mutation skipped - could not serialize tasks: {}",
                e
            );
            return;
        }
    };
    let prompt = build_mutation_prompt(human_feedback, &todo_json);

    // 4. Spawn Claude subprocess
    eprintln!("Running task mutation via Claude...");
    let effective_model = model.unwrap_or(SONNET_MODEL);
    let result = match claude::spawn_claude(
        &prompt,
        None,
        None,
        Some(effective_model),
        None,
        false,
        permission_mode,
        None,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: task mutation failed (spawn error): {}", e);
            return;
        }
    };

    if result.exit_code != 0 {
        eprintln!(
            "Warning: task mutation failed (Claude exit code {})",
            result.exit_code
        );
        return;
    }

    // 5. Parse Claude's output for a JSON array of modifications
    let modifications = match parse_mutation_output(&result.output) {
        Some(mods) => mods,
        None => {
            eprintln!(
                "Warning: task mutation skipped - could not parse Claude output as JSON array"
            );
            return;
        }
    };

    if modifications.is_empty() {
        eprintln!("Task mutation: no modifications requested by Claude.");
        return;
    }

    // 6. Create a backup of the PRD before mutating
    let bak_path = prd_path.with_extension("json.bak");
    if let Err(e) = std::fs::copy(prd_path, &bak_path) {
        eprintln!(
            "Warning: could not create PRD backup at {}: {} (continuing)",
            bak_path.display(),
            e
        );
    }

    // 7. Apply modifications to PRD JSON in-memory
    let (updated_prd, stats) = match apply_modifications_to_prd(prd, &modifications, task_prefix) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Warning: task mutation failed (apply modifications): {}", e);
            return;
        }
    };

    // 8. Write updated PRD atomically (temp file + rename)
    let tmp_path = prd_path.with_extension("json.tmp");
    let updated_json = match serde_json::to_string_pretty(&updated_prd) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Warning: task mutation failed (serialize): {}", e);
            return;
        }
    };
    if let Err(e) = std::fs::write(&tmp_path, &updated_json) {
        eprintln!(
            "Warning: task mutation failed (write temp file {}): {}",
            tmp_path.display(),
            e
        );
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, prd_path) {
        eprintln!(
            "Warning: task mutation failed (rename to {}): {}",
            prd_path.display(),
            e
        );
        return;
    }

    // 9. Sync mutations to DB directly (next iteration's hash-check will also re-import)
    sync_mutations_to_db(conn, &modifications, task_prefix);

    // 10. Log summary
    eprintln!(
        "Task mutation: {} tasks modified, {} added, {} marked irrelevant",
        stats.modified, stats.added, stats.irrelevant
    );
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
            let status = crate::loop_engine::test_utils::get_task_status(&conn, id);
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
        let p1_001_status = crate::loop_engine::test_utils::get_task_status(&conn, "P1-TASK-001");
        assert_eq!(
            p1_001_status, "done",
            "P1-TASK-001 should be reconciled to done"
        );

        // P1-TASK-002 (passes: false) should remain todo
        let p1_002_status = crate::loop_engine::test_utils::get_task_status(&conn, "P1-TASK-002");
        assert_eq!(p1_002_status, "todo", "P1-TASK-002 should remain todo");

        // P2-TASK-001 must NOT be touched (different prefix)
        let p2_status = crate::loop_engine::test_utils::get_task_status(&conn, "P2-TASK-001");
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

        let status = crate::loop_engine::test_utils::get_task_status(&conn, "TASK-001");
        assert_eq!(
            status, "done",
            "None prefix should reconcile matching tasks"
        );
    }

    // --- parse_mutation_output tests ---

    #[test]
    fn test_parse_mutation_output_plain_array() {
        let output = r#"[{"id":"FEAT-001","action":"modify","fields":{"notes":"updated"}}]"#;
        let result = parse_mutation_output(output);
        assert!(result.is_some());
        let mods = result.unwrap();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0]["id"], "FEAT-001");
    }

    #[test]
    fn test_parse_mutation_output_with_prose_before_and_after() {
        let output =
            "Here are the modifications:\n[{\"id\":\"X\",\"action\":\"irrelevant\"}]\nDone.";
        let result = parse_mutation_output(output);
        assert!(result.is_some());
        assert_eq!(result.unwrap()[0]["action"], "irrelevant");
    }

    #[test]
    fn test_parse_mutation_output_empty_array() {
        let output = "[]";
        let result = parse_mutation_output(output);
        assert!(result.is_some());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_parse_mutation_output_no_array_returns_none() {
        let output = "No changes needed.";
        assert!(parse_mutation_output(output).is_none());
    }

    #[test]
    fn test_parse_mutation_output_invalid_json_returns_none() {
        let output = "[not valid json";
        assert!(parse_mutation_output(output).is_none());
    }

    // --- apply_modifications_to_prd tests ---

    fn make_test_prd() -> serde_json::Value {
        serde_json::json!({
            "userStories": [
                {"id": "FEAT-001", "title": "Old title", "notes": "old notes", "passes": false},
                {"id": "FEAT-002", "title": "Another task", "passes": false}
            ]
        })
    }

    #[test]
    fn test_apply_modify_updates_fields() {
        let prd = make_test_prd();
        let modifications = vec![serde_json::json!({
            "id": "FEAT-001",
            "action": "modify",
            "fields": {"notes": "new notes", "title": "New title"}
        })];
        let (updated, stats) = apply_modifications_to_prd(prd, &modifications, None).unwrap();
        assert_eq!(stats.modified, 1);
        assert_eq!(stats.added, 0);
        assert_eq!(stats.irrelevant, 0);
        let stories = updated["userStories"].as_array().unwrap();
        assert_eq!(stories[0]["notes"], "new notes");
        assert_eq!(stories[0]["title"], "New title");
    }

    #[test]
    fn test_apply_irrelevant_sets_mutation_status() {
        let prd = make_test_prd();
        let modifications = vec![serde_json::json!({"id": "FEAT-002", "action": "irrelevant"})];
        let (updated, stats) = apply_modifications_to_prd(prd, &modifications, None).unwrap();
        assert_eq!(stats.irrelevant, 1);
        let stories = updated["userStories"].as_array().unwrap();
        assert_eq!(stories[1]["mutationStatus"], "irrelevant");
    }

    #[test]
    fn test_apply_add_inserts_new_story() {
        let prd = make_test_prd();
        let modifications = vec![serde_json::json!({
            "id": "NEW-001",
            "action": "add",
            "title": "Brand new task",
            "description": "Do something",
            "priority": 5
        })];
        let (updated, stats) = apply_modifications_to_prd(prd, &modifications, None).unwrap();
        assert_eq!(stats.added, 1);
        let stories = updated["userStories"].as_array().unwrap();
        assert_eq!(stories.len(), 3);
        let new_story = &stories[2];
        assert_eq!(new_story["id"], "NEW-001");
        assert_eq!(new_story["title"], "Brand new task");
        // "action" key must be removed, "passes" must default to false
        assert!(new_story.get("action").is_none());
        assert_eq!(new_story["passes"], false);
    }

    #[test]
    fn test_apply_modify_strips_prefix_for_matching() {
        // Claude returns a base ID; PRD JSON also has the base ID
        let prd = make_test_prd();
        let modifications = vec![serde_json::json!({
            "id": "abc123-FEAT-001",
            "action": "modify",
            "fields": {"notes": "prefix-stripped"}
        })];
        let (updated, stats) =
            apply_modifications_to_prd(prd, &modifications, Some("abc123")).unwrap();
        assert_eq!(stats.modified, 1);
        assert_eq!(updated["userStories"][0]["notes"], "prefix-stripped");
    }

    #[test]
    fn test_apply_modify_unknown_id_is_a_no_op() {
        let prd = make_test_prd();
        let modifications = vec![serde_json::json!({
            "id": "NONEXISTENT",
            "action": "modify",
            "fields": {"notes": "ignored"}
        })];
        let (_, stats) = apply_modifications_to_prd(prd, &modifications, None).unwrap();
        assert_eq!(stats.modified, 0);
    }

    #[test]
    fn test_apply_modify_non_whitelisted_fields_are_rejected() {
        let prd = make_test_prd();
        let modifications = vec![serde_json::json!({
            "id": "FEAT-001",
            "action": "modify",
            "fields": {
                "passes": true,
                "id": "HACKED",
                "requiresHuman": true,
                "mutationStatus": "pwned",
                "title": "Allowed title"
            }
        })];
        let (updated, stats) = apply_modifications_to_prd(prd, &modifications, None).unwrap();
        assert_eq!(stats.modified, 1);
        let story = &updated["userStories"][0];
        // Whitelisted field is applied
        assert_eq!(story["title"], "Allowed title");
        // Non-whitelisted fields are not applied
        assert_eq!(story["passes"], serde_json::Value::Bool(false));
        assert_eq!(story["id"], "FEAT-001");
        assert!(
            story.get("requiresHuman").is_none()
                || story["requiresHuman"] == serde_json::Value::Null
        );
        assert!(
            story.get("mutationStatus").is_none()
                || story["mutationStatus"] == serde_json::Value::Null
        );
    }

    // --- build_mutation_prompt tests ---

    #[test]
    fn test_build_mutation_prompt_contains_feedback_and_tasks() {
        let prompt = build_mutation_prompt("user said X", "[{\"id\":\"T-1\"}]");
        assert!(
            prompt.contains("user said X"),
            "Prompt must include feedback"
        );
        assert!(
            prompt.contains("[{\"id\":\"T-1\"}]"),
            "Prompt must include tasks JSON"
        );
        assert!(
            prompt.contains("modify"),
            "Prompt must describe modify action"
        );
        assert!(
            prompt.contains("irrelevant"),
            "Prompt must describe irrelevant action"
        );
        assert!(prompt.contains("add"), "Prompt must describe add action");
    }

    // --- guidance last_text tests ---

    #[test]
    fn test_guidance_last_text_returns_most_recent_entry() {
        use crate::loop_engine::guidance::SessionGuidance;
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "first".to_string());
        guidance.add(2, "second".to_string());
        assert_eq!(guidance.last_text(), Some("second"));
    }

    #[test]
    fn test_guidance_last_text_empty_returns_none() {
        use crate::loop_engine::guidance::SessionGuidance;
        let guidance = SessionGuidance::new();
        assert!(guidance.last_text().is_none());
    }

    // ─── Helpers for mutation-level tests ──────────────────────────────────────

    // Use the shared CLAUDE_BINARY_MUTEX from test_utils so that these tests
    // serialize against claude.rs tests that also set CLAUDE_BINARY.
    use crate::loop_engine::test_utils::{EnvGuard, CLAUDE_BINARY_MUTEX};

    /// Write a shell script to a predictable temp path that drains stdin and
    /// prints `json_output`. Caller must hold `CLAUDE_BINARY_MUTEX` for the
    /// duration of the test that sets `CLAUDE_BINARY` to this path.
    fn make_mock_claude_script(name: &str, json_output: &str) -> std::path::PathBuf {
        use std::io::Write as _;
        let path = std::env::temp_dir().join(format!("task_mgr_prd_reconcile_{name}.sh"));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, "cat > /dev/null").unwrap(); // drain stdin to avoid SIGPIPE
        writeln!(f, "cat << 'SCRIPT_END'").unwrap();
        writeln!(f, "{json_output}").unwrap();
        writeln!(f, "SCRIPT_END").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        path
    }

    /// Write a minimal one-task PRD JSON to `path` (passes: false).
    fn write_test_prd(path: &Path) {
        let prd = serde_json::json!({
            "userStories": [
                {"id": "TASK-001", "title": "Test task", "passes": false}
            ]
        });
        std::fs::write(path, serde_json::to_string_pretty(&prd).unwrap()).unwrap();
    }

    // ─── AC: prompt excludes done/blocked tasks ─────────────────────────────────

    #[test]
    fn test_mutation_prompt_excludes_done_and_blocked_tasks() {
        let stories = serde_json::json!([
            {"id": "DONE-001", "title": "Done",    "passes": true},
            {"id": "TODO-002", "title": "Active",  "passes": false},
            {"id": "IRRL-003", "title": "Blocked", "mutationStatus": "irrelevant", "passes": false}
        ]);
        // Mirror the filter from mutate_prd_from_feedback
        let todo_tasks: Vec<&serde_json::Value> = stories
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| {
                s.get("passes").and_then(|v| v.as_bool()) != Some(true)
                    && s.get("mutationStatus").and_then(|v| v.as_str()) != Some("irrelevant")
            })
            .collect();
        let todo_json = serde_json::to_string_pretty(&todo_tasks).unwrap();

        assert!(todo_json.contains("TODO-002"), "todo task must be included");
        assert!(
            !todo_json.contains("DONE-001"),
            "done task must be excluded"
        );
        assert!(
            !todo_json.contains("IRRL-003"),
            "irrelevant task must be excluded"
        );

        let prompt = build_mutation_prompt("important review feedback", &todo_json);
        assert!(
            prompt.contains("important review feedback"),
            "human feedback must appear in prompt"
        );
        assert!(
            prompt.contains("TODO-002"),
            "active task must appear in prompt"
        );
        assert!(
            !prompt.contains("DONE-001"),
            "done task must not appear in prompt"
        );
    }

    // ─── AC: atomic JSON write — no .json.tmp remains ──────────────────────────

    #[test]
    fn test_atomic_write_leaves_no_tmp_file() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");
        let tmp_path = prd_path.with_extension("json.tmp");

        std::fs::write(
            &prd_path,
            r#"{"project":"T","userStories":[{"id":"X-1","passes":false}]}"#,
        )
        .unwrap();

        update_prd_task_passes(&prd_path, "X-1", true, None).unwrap();

        assert!(
            !tmp_path.exists(),
            ".json.tmp must not remain after atomic write completes"
        );
        let content = std::fs::read_to_string(&prd_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["userStories"][0]["passes"], true);
    }

    // ─── AC: DB re-sync — sync_mutations_to_db ─────────────────────────────────

    #[test]
    fn test_sync_mutations_to_db_modify_updates_fields() {
        use crate::loop_engine::test_utils::setup_test_db;
        let (_tmp, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES ('TASK-001', 'Old', 'todo', 10)",
            [],
        )
        .unwrap();

        let mods = vec![serde_json::json!({
            "id": "TASK-001", "action": "modify",
            "fields": {"notes": "synced notes", "title": "Updated title"}
        })];
        sync_mutations_to_db(&conn, &mods, None);

        let title: String = conn
            .query_row("SELECT title FROM tasks WHERE id = 'TASK-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let notes: Option<String> = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'TASK-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "Updated title");
        assert_eq!(notes.as_deref(), Some("synced notes"));
    }

    #[test]
    fn test_sync_mutations_to_db_irrelevant_changes_status() {
        use crate::loop_engine::test_utils::setup_test_db;
        let (_tmp, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES ('TASK-002', 'T2', 'todo', 5)",
            [],
        )
        .unwrap();

        let mods = vec![serde_json::json!({"id": "TASK-002", "action": "irrelevant"})];
        sync_mutations_to_db(&conn, &mods, None);

        let status = crate::loop_engine::test_utils::get_task_status(&conn, "TASK-002");
        assert_eq!(status, "irrelevant");
    }

    #[test]
    fn test_sync_mutations_to_db_add_inserts_task() {
        use crate::loop_engine::test_utils::setup_test_db;
        let (_tmp, conn) = setup_test_db();

        let mods = vec![serde_json::json!({
            "id": "NEW-001", "action": "add",
            "title": "Brand new task", "description": "Do stuff", "priority": 7
        })];
        sync_mutations_to_db(&conn, &mods, None);

        let (title, status, priority): (String, String, i32) = conn
            .query_row(
                "SELECT title, status, priority FROM tasks WHERE id = 'NEW-001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Brand new task");
        assert_eq!(status, "todo");
        assert_eq!(priority, 7);
    }

    // ─── AC: .json.bak backup created before mutation write ─────────────────────

    #[test]
    fn test_backup_created_before_mutation() {
        use crate::loop_engine::config::PermissionMode;
        use crate::loop_engine::test_utils::setup_test_db;
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let script = make_mock_claude_script(
            "backup",
            r#"[{"id":"TASK-001","action":"modify","fields":{"notes":"mutated"}}]"#,
        );
        let _env = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());

        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("tasks.json");
        write_test_prd(&prd_path);

        let (_db, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES ('TASK-001', 'T', 'todo', 1)",
            [],
        )
        .unwrap();

        let mode = PermissionMode::Scoped {
            allowed_tools: None,
        };
        mutate_prd_from_feedback(&prd_path, "feedback", &conn, None, None, &mode);

        let bak_path = temp_dir.path().join("tasks.json.bak");
        assert!(
            bak_path.exists(),
            ".json.bak backup must be created before the mutation write"
        );
    }

    // ─── AC: graceful degradation — Claude returns invalid JSON ────────────────

    #[test]
    fn test_graceful_degradation_claude_invalid_json() {
        use crate::loop_engine::config::PermissionMode;
        use crate::loop_engine::test_utils::setup_test_db;
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let script = make_mock_claude_script("invalid_json", "not a json array at all");
        let _env = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());

        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("tasks.json");
        write_test_prd(&prd_path);
        let original = std::fs::read_to_string(&prd_path).unwrap();

        let (_db, conn) = setup_test_db();
        let mode = PermissionMode::Scoped {
            allowed_tools: None,
        };

        // Must not panic
        mutate_prd_from_feedback(&prd_path, "some feedback", &conn, None, None, &mode);

        let after = std::fs::read_to_string(&prd_path).unwrap();
        assert_eq!(
            original, after,
            "PRD must be unchanged when Claude outputs invalid JSON"
        );
    }

    // ─── AC: graceful degradation — Claude subprocess fails ────────────────────

    #[test]
    fn test_graceful_degradation_claude_subprocess_fails() {
        use crate::loop_engine::config::PermissionMode;
        use crate::loop_engine::test_utils::setup_test_db;
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // /usr/bin/false always exits with code 1 — simulates Claude failure
        let _env = EnvGuard::set("CLAUDE_BINARY", "/usr/bin/false");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("tasks.json");
        write_test_prd(&prd_path);
        let original = std::fs::read_to_string(&prd_path).unwrap();

        let (_db, conn) = setup_test_db();
        let mode = PermissionMode::Scoped {
            allowed_tools: None,
        };

        // Must not panic
        mutate_prd_from_feedback(&prd_path, "feedback here", &conn, None, None, &mode);

        let after = std::fs::read_to_string(&prd_path).unwrap();
        assert_eq!(
            original, after,
            "PRD must be unchanged when Claude subprocess exits non-zero"
        );
    }

    // ─── AC: mutation summary log message format ────────────────────────────────

    #[test]
    fn test_mutation_stats_match_summary_format() {
        // The log line produced in mutate_prd_from_feedback is:
        //   "Task mutation: {} tasks modified, {} added, {} marked irrelevant"
        // Verify apply_modifications_to_prd returns stats that would produce the
        // correct values for a mixed-action modification list.
        let prd = serde_json::json!({
            "userStories": [
                {"id": "T-001", "title": "Alpha", "passes": false},
                {"id": "T-002", "title": "Beta",  "passes": false},
                {"id": "T-003", "title": "Gamma", "passes": false}
            ]
        });
        let mods = vec![
            serde_json::json!({"id": "T-001", "action": "modify", "fields": {"notes": "x"}}),
            serde_json::json!({"id": "T-002", "action": "irrelevant"}),
            serde_json::json!({"id": "NEW-001", "action": "add", "title": "New", "priority": 10}),
        ];
        let (_, stats) = apply_modifications_to_prd(prd, &mods, None).unwrap();

        assert_eq!(stats.modified, 1, "modified count");
        assert_eq!(stats.added, 1, "added count");
        assert_eq!(stats.irrelevant, 1, "irrelevant count");

        // Verify summary string matches the format used in mutate_prd_from_feedback
        let summary = format!(
            "Task mutation: {} tasks modified, {} added, {} marked irrelevant",
            stats.modified, stats.added, stats.irrelevant
        );
        assert_eq!(
            summary,
            "Task mutation: 1 tasks modified, 1 added, 1 marked irrelevant"
        );
    }
}
