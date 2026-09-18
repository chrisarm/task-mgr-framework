//! Database import operations for the init command.
//!
//! This module contains all functions for inserting, updating, and deleting
//! task data in the SQLite database.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::db::prefix::make_like_pattern;
use crate::db::soft_archive::{
    archive_prd_by_prefix, archive_unprefixed_live_tasks, drop_prd_rows,
};
use crate::models::TaskStatus;

use super::output::{ArchivePrefixPreview, DryRunArchivePreview, DryRunDeletePreview};
use super::parse::{PrdFile, PrdUserStory};

/// Plan for prefix-scoped `--force`: soft-archive union, never hard-delete tasks.
#[derive(Debug, Clone, Default)]
pub struct ForceArchivePlan {
    /// Prefixed names to soft-archive via `{prefix}-%` LIKE (identity ∪ about-to-apply).
    pub prefixes: BTreeSet<String>,
    /// `prd_metadata.id` rows with `task_prefix IS NULL` from path identity.
    pub null_prd_ids: Vec<i64>,
    /// Whether the union includes the NULL / unprefixed identity.
    pub includes_null: bool,
}

impl ForceArchivePlan {
    /// True when this hatch should soft-archive rather than legacy global wipe.
    #[must_use]
    pub fn is_scoped(&self) -> bool {
        !self.prefixes.is_empty() || self.includes_null
    }
}

/// Soft-archive every prefix in `plan` (no `DELETE FROM tasks`, no file moves).
///
/// Returns total tasks soft-archived across all union members.
pub fn force_union_archive(conn: &Connection, plan: &ForceArchivePlan) -> TaskMgrResult<usize> {
    let mut total = 0usize;

    // Preserve-prefix set for NULL unprefixed archiving: all Some() union members
    // (and any other live PRD prefixes) so we do not touch sibling PRDs.
    let mut preserve: Vec<String> = plan.prefixes.iter().cloned().collect();
    if plan.includes_null {
        // Also preserve every other registered non-NULL prefix not already in the union.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT task_prefix FROM prd_metadata WHERE task_prefix IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            let p = row?;
            if !preserve.contains(&p) {
                preserve.push(p);
            }
        }
        // Before archiving prefixed members, unprefixed archive must run against
        // the pre-drop preserve set. Prefixed archive below may drop metadata.
        total += archive_unprefixed_live_tasks(conn, &preserve)?;
        drop_prd_rows(conn, &plan.null_prd_ids)?;
    }

    for prefix in &plan.prefixes {
        total += archive_prd_by_prefix(conn, prefix, None)?;
    }

    Ok(total)
}

/// Dry-run counts for each union prefix (live tasks only).
pub fn get_archive_preview(
    conn: &Connection,
    plan: &ForceArchivePlan,
) -> TaskMgrResult<DryRunArchivePreview> {
    let mut prefixes = Vec::new();

    if plan.includes_null {
        // Count live tasks not matching any preserve prefix (same scope as archive).
        let mut preserve: Vec<String> = plan.prefixes.iter().cloned().collect();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT task_prefix FROM prd_metadata WHERE task_prefix IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            let p = row?;
            if !preserve.contains(&p) {
                preserve.push(p);
            }
        }
        let patterns: Vec<String> = preserve.iter().map(|p| make_like_pattern(p)).collect();
        let mut sql = String::from("SELECT COUNT(*) FROM tasks WHERE archived_at IS NULL");
        for _ in &patterns {
            sql.push_str(" AND id NOT LIKE ? ESCAPE '\\'");
        }
        let mut q = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = patterns
            .iter()
            .map(|p| p as &dyn rusqlite::types::ToSql)
            .collect();
        let tasks: i64 = q.query_row(params.as_slice(), |row| row.get(0))?;
        prefixes.push(ArchivePrefixPreview {
            prefix: None,
            tasks: tasks as usize,
        });
    }

    for prefix in &plan.prefixes {
        let pattern = make_like_pattern(prefix);
        let tasks: usize = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE id LIKE ? ESCAPE '\\' AND archived_at IS NULL",
            [&pattern],
            |row| Ok(row.get::<_, i64>(0)? as usize),
        )?;
        prefixes.push(ArchivePrefixPreview {
            prefix: Some(prefix.clone()),
            tasks,
        });
    }

    Ok(DryRunArchivePreview { prefixes })
}

/// Drop existing data from the database.
///
/// When `task_prefix` is `Some(prefix)`, run_tasks, key_decisions, and runs are
/// soft-archived (UPDATE SET archived_at) to preserve history. Tasks and their
/// metadata are hard-deleted so their IDs are clean for reimport.
/// Learnings and other PRDs are untouched.
///
/// When `task_prefix` is `None`, all data is wiped (legacy global-force behavior).
pub fn drop_existing_data(conn: &Connection, task_prefix: Option<&str>) -> TaskMgrResult<()> {
    match task_prefix {
        Some(prefix) => {
            let pattern = make_like_pattern(prefix);

            // Archive run_tasks, key_decisions, and runs for this prefix (preserve history).
            crate::db::soft_archive::soft_archive_by_prefix(conn, prefix)?;

            // Disable FK enforcement so archived run_tasks survive the tasks hard-delete.
            // (run_tasks.task_id has ON DELETE CASCADE, which would wipe archived rows if FK is on.)
            // FK is always re-enabled after deletes, even on error.
            // Disable FK enforcement so archived run_tasks survive the tasks hard-delete.
            // PRAGMA foreign_keys cannot be changed inside a transaction (SQLite constraint),
            // so it must be set before/after the transaction.
            conn.pragma_update(None, "foreign_keys", "OFF")?;
            let delete_result = (|| -> TaskMgrResult<()> {
                let tx = conn.unchecked_transaction()?;
                // Delete child tables before parent (FK ordering)
                tx.execute(
                    "DELETE FROM task_relationships WHERE task_id LIKE ? ESCAPE '\\'",
                    [&pattern],
                )?;
                tx.execute(
                    "DELETE FROM task_files WHERE task_id LIKE ? ESCAPE '\\'",
                    [&pattern],
                )?;
                tx.execute("DELETE FROM tasks WHERE id LIKE ? ESCAPE '\\'", [&pattern])?;
                // prd_files must be removed before prd_metadata (FK ordering)
                tx.execute(
                    "DELETE FROM prd_files WHERE prd_id = \
                     (SELECT id FROM prd_metadata WHERE task_prefix = ?)",
                    [prefix],
                )?;
                tx.execute("DELETE FROM prd_metadata WHERE task_prefix = ?", [prefix])?;
                tx.commit()?;
                Ok(())
            })();
            conn.pragma_update(None, "foreign_keys", "ON")?;
            delete_result?;
        }
        None => {
            // Global wipe — preserve nothing (legacy behavior).
            // Drop in correct order due to foreign keys.
            conn.execute("DELETE FROM learning_tags", [])?;
            conn.execute("DELETE FROM learnings", [])?;
            conn.execute("DELETE FROM key_decisions", [])?;
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
                "SELECT COUNT(*) FROM tasks WHERE id LIKE ? ESCAPE '\\' AND archived_at IS NULL",
                [&pattern],
                |row| Ok(row.get::<_, i64>(0)? as usize),
            )?;
            let files: usize = conn.query_row(
                "SELECT COUNT(*) FROM task_files WHERE task_id LIKE ? ESCAPE '\\'",
                [&pattern],
                |row| Ok(row.get::<_, i64>(0)? as usize),
            )?;
            let relationships: usize = conn.query_row(
                "SELECT COUNT(*) FROM task_relationships WHERE task_id LIKE ? ESCAPE '\\'",
                [&pattern],
                |row| Ok(row.get::<_, i64>(0)? as usize),
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
            let tasks: usize = conn.query_row(
                "SELECT COUNT(*) FROM tasks WHERE archived_at IS NULL",
                [],
                |row| Ok(row.get::<_, i64>(0)? as usize),
            )?;
            let files: usize = conn.query_row("SELECT COUNT(*) FROM task_files", [], |row| {
                Ok(row.get::<_, i64>(0)? as usize)
            })?;
            let relationships: usize =
                conn.query_row("SELECT COUNT(*) FROM task_relationships", [], |row| {
                    Ok(row.get::<_, i64>(0)? as usize)
                })?;
            let learnings: usize = conn.query_row("SELECT COUNT(*) FROM learnings", [], |row| {
                Ok(row.get::<_, i64>(0)? as usize)
            })?;
            // Count ALL runs (including archived) since the global wipe DELETE FROM runs
            // removes everything. Tasks are filtered by archived_at IS NULL because
            // archived tasks are invisible to users and the count reflects "active" state.
            let runs: usize = conn.query_row("SELECT COUNT(*) FROM runs", [], |row| {
                Ok(row.get::<_, i64>(0)? as usize)
            })?;
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

/// Check if the database is fresh (no active tasks).
pub fn is_fresh_database(conn: &Connection) -> TaskMgrResult<bool> {
    let count: i32 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE archived_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(count == 0)
}

/// Get all existing task IDs from the database, including soft-archived rows.
///
/// Archived rows must be included so `init --append --update-existing` can
/// reconcile against them. If they were filtered out, the importer would
/// route incoming stories to INSERT and trip the UNIQUE constraint on the
/// physical row that still exists in the table. Callers in the update path
/// are expected to clear `archived_at` so the row is revived on re-import.
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
    // FR-002 hard break: a PRD-level `default_model` is still stored and
    // exported verbatim (this INSERT keeps the column), but model resolution
    // ignores it under the provider-first `models`/`routing` config. Warn once
    // per import (loop init / batch init / loop-run re-import) so the operator
    // isn't surprised when the value has no effect.
    if prd.model.is_some() {
        crate::output::warn(
            "PRD `default_model` is ignored under the models config; use models.anchor / \
             routing instead",
        );
    }
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
            external_git_repo, task_prefix, default_model, default_max_retries, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
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
               default_max_retries = excluded.default_max_retries,
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
            prd.default_max_retries,
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

/// Update an existing `prd_metadata` row by primary key.
///
/// Used by the sticky identity resolver when a path is already registered:
/// refreshes project fields / raw_json but **never** changes `task_prefix`
/// (prefix is frozen at first registration). Prefer this over
/// [`insert_prd_metadata`] for known identities so NULL-prefix rows cannot
/// mint a twin via `ON CONFLICT(task_prefix)` (UNIQUE allows multiple NULLs).
pub fn update_prd_metadata_by_id(
    conn: &Connection,
    prd_id: i64,
    prd: &PrdFile,
    raw_json: Option<&str>,
) -> TaskMgrResult<()> {
    if prd.model.is_some() {
        crate::output::warn(
            "PRD `default_model` is ignored under the models config; use models.anchor / \
             routing instead",
        );
    }
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

    let updated = conn.execute(
        r#"UPDATE prd_metadata SET
               project = ?1,
               branch_name = ?2,
               description = ?3,
               priority_philosophy = ?4,
               global_acceptance_criteria = ?5,
               review_guidelines = ?6,
               raw_json = ?7,
               external_git_repo = ?8,
               default_model = ?9,
               default_max_retries = ?10,
               updated_at = datetime('now')
           WHERE id = ?11"#,
        rusqlite::params![
            prd.project,
            prd.branch_name,
            prd.description,
            priority_philosophy,
            global_acceptance,
            review_guidelines,
            raw_json,
            prd.external_git_repo,
            prd.model,
            prd.default_max_retries,
            prd_id,
        ],
    )?;
    if updated == 0 {
        return Err(crate::TaskMgrError::NotFound {
            resource_type: "prd_metadata".to_string(),
            id: prd_id.to_string(),
        });
    }
    Ok(())
}

/// Serialized fields shared between insert_task and update_task.
struct TaskSerializedFields {
    acceptance_criteria: String,
    review_scope: Option<String>,
    required_tests: Option<String>,
    max_retries: i32,
}

/// Serialize and resolve the fields that insert_task and update_task both need.
///
/// Precedence for max_retries: story.max_retries > prd_default_max_retries > 3.
fn prepare_task_fields(
    story: &PrdUserStory,
    prd_default_max_retries: Option<i32>,
) -> TaskMgrResult<TaskSerializedFields> {
    let acceptance_criteria = serde_json::to_string(&story.acceptance_criteria)?;

    let review_scope = story
        .review_scope
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    let required_tests = if story.required_tests.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&story.required_tests)?)
    };

    let max_retries = story
        .max_retries
        .unwrap_or_else(|| prd_default_max_retries.unwrap_or(3));

    Ok(TaskSerializedFields {
        acceptance_criteria,
        review_scope,
        required_tests,
        max_retries,
    })
}

/// Insert a task into the database.
///
/// `prd_default_max_retries` is the PRD-level default used to resolve the per-task
/// `max_retries`. Precedence: story.max_retries > prd_default_max_retries > 3.
pub fn insert_task(
    conn: &Connection,
    story: &PrdUserStory,
    prd_default_max_retries: Option<i32>,
) -> TaskMgrResult<()> {
    // Map passes boolean to TaskStatus
    let status = if story.passes {
        TaskStatus::Done
    } else {
        TaskStatus::Todo
    };

    let TaskSerializedFields {
        acceptance_criteria,
        review_scope,
        required_tests,
        max_retries,
    } = prepare_task_fields(story, prd_default_max_retries)?;

    conn.execute(
        r#"INSERT INTO tasks
           (id, title, description, priority, status, notes, acceptance_criteria,
            review_scope, severity, source_review, model, difficulty, escalation_note,
            required_tests, max_retries, requires_human, human_review_timeout,
            claims_shared_infra)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
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
            required_tests,
            max_retries,
            story.requires_human.unwrap_or(false) as i32,
            story.human_review_timeout,
            story.claims_shared_infra.map(|b| b as i32),
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

/// Outcome of inserting a single task's relationships.
///
/// `had_deprecated` is set when the task carried any of the dropped relationship
/// types (`synergyWith` / `batchWith` / `conflictsWith`). Callers aggregate it
/// across an import and emit a single deprecation warning at the end, instead
/// of one per task, to keep stderr from flooding on large PRD upgrades.
#[derive(Debug, Default, Clone, Copy)]
pub struct RelationshipInsertResult {
    pub count: usize,
    pub had_deprecated: bool,
}

/// Insert all relationships for a task.
///
/// `synergyWith`, `batchWith`, and `conflictsWith` are deprecated; file-overlap
/// detection at runtime replaces them. They are silently ignored here, and the
/// returned `had_deprecated` flag tells the caller to emit a single
/// deprecation warning per import session.
pub fn insert_task_relationships(
    conn: &Connection,
    story: &PrdUserStory,
) -> TaskMgrResult<RelationshipInsertResult> {
    let had_deprecated = !story.synergy_with.is_empty()
        || !story.batch_with.is_empty()
        || !story.conflicts_with.is_empty();
    let mut count = 0;
    for dep in &story.depends_on {
        insert_relationship(conn, &story.id, dep, "dependsOn")?;
        count += 1;
    }
    Ok(RelationshipInsertResult {
        count,
        had_deprecated,
    })
}

/// Format the standardized deprecation warning emitted at most once per
/// import when any task in the batch carried deprecated relationship fields.
pub const DEPRECATED_RELATIONSHIPS_WARNING: &str = "warning: PRD contains deprecated relationship fields (synergyWith/batchWith/conflictsWith); \
     these are ignored — use touchesFiles for conflict detection";

/// Update an existing task in the database.
///
/// `prd_default_max_retries` is the PRD-level default used to resolve the per-task
/// `max_retries`. Precedence: story.max_retries > prd_default_max_retries > 3.
pub fn update_task(
    conn: &Connection,
    story: &PrdUserStory,
    prd_default_max_retries: Option<i32>,
) -> TaskMgrResult<()> {
    let TaskSerializedFields {
        acceptance_criteria,
        review_scope,
        required_tests,
        max_retries,
    } = prepare_task_fields(story, prd_default_max_retries)?;

    // Note: We don't update status from passes here - the task may have been
    // completed in the DB since the JSON was written. We only update metadata.
    //
    // archived_at is cleared so re-importing an archived PRD revives the rows.
    // Without this, the row would remain invisible to the loop's `next` query
    // even though metadata had just been refreshed from the JSON.
    conn.execute(
        r#"UPDATE tasks SET
           title = ?, description = ?, priority = ?, notes = ?,
           acceptance_criteria = ?, review_scope = ?, severity = ?,
           source_review = ?, model = ?, difficulty = ?, escalation_note = ?,
           required_tests = ?, max_retries = ?,
           requires_human = ?, human_review_timeout = ?,
           claims_shared_infra = ?,
           archived_at = NULL,
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
            required_tests,
            max_retries,
            story.requires_human.unwrap_or(false) as i32,
            story.human_review_timeout,
            story.claims_shared_infra.map(|b| b as i32),
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

/// Convert a live filesystem path to the canonical `prd_files.file_path` form.
///
/// **Contract:** stored `TEXT` is source-root-relative POSIX (forward slashes),
/// e.g. `tasks/foo.json`. Never strip against `.task-mgr/tasks`.
///
/// When `path` lies outside `source_root` (strip fails), this stores a stable
/// absolute form: `canonicalize(path)` when that succeeds, otherwise the
/// absolute path as given (or `source_root.join(path)` when still relative).
/// Never panics.
pub fn path_for_prd_files_storage(path: &Path, source_root: &Path) -> String {
    let to_posix = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let canon_root = std::fs::canonicalize(source_root).ok();

    // Absolute (or cwd-resolvable) path: canonicalize + strip source_root.
    let canon_path = std::fs::canonicalize(path).ok();
    if let (Some(cp), Some(cr)) = (&canon_path, &canon_root)
        && let Ok(rel) = cp.strip_prefix(cr)
    {
        return to_posix(rel);
    }

    // Relative caller path (e.g. CLI `tasks/foo.json`): resolve via source_root.
    if !path.is_absolute() {
        let joined = source_root.join(path);
        if let (Ok(cp), Some(cr)) = (std::fs::canonicalize(&joined), &canon_root)
            && let Ok(rel) = cp.strip_prefix(cr)
        {
            return to_posix(rel);
        }
        // Not on disk yet — store the relative POSIX form as given.
        return to_posix(path);
    }

    // Logical strip without canonicalize.
    if let Ok(rel) = path.strip_prefix(source_root) {
        return to_posix(rel);
    }
    if let (Some(cr), Ok(cp)) = (&canon_root, std::fs::canonicalize(path))
        && let Ok(rel) = cp.strip_prefix(cr)
    {
        return to_posix(rel);
    }

    // Outside source_root: stable absolute form; do not panic.
    if let Some(cp) = canon_path {
        return to_posix(&cp);
    }
    to_posix(path)
}

/// Resolve a stored `prd_files.file_path` to a live filesystem path.
///
/// Absolute stored paths (legacy rows) are used as-is then remapped into
/// `worktree_root`. Relative paths are joined to `source_root` then remapped.
/// This is the single read helper for archive discovery, `locate_prd_json`,
/// and any consumer that previously did `tasks_dir.join(stored)`.
///
/// Delegates path math to [`crate::git::remap_into_worktree`] (pin-19).
pub fn resolve_prd_file_path(stored: &Path, source_root: &Path, worktree_root: &Path) -> PathBuf {
    crate::git::remap_into_worktree(stored, source_root, worktree_root)
}

/// Register all files associated with a PRD in the `prd_files` table.
///
/// Records:
/// 1. The task list JSON file as `task_list` type
/// 2. The derived prompt file (`<stem>-prompt.md`) as `prompt` type if it exists
/// 3. The PRD markdown file from `prd.prd_file` as `prd` type if set
///
/// Task-list and prompt paths are stored as **source-root-relative POSIX**
/// via [`path_for_prd_files_storage`]. Do not join those stored values onto
/// `.task-mgr/tasks` — use [`resolve_prd_file_path`] on read.
pub fn register_prd_files(
    conn: &Connection,
    prd_id: i64,
    json_path: &Path,
    prd: &PrdFile,
    source_root: &Path,
) -> TaskMgrResult<()> {
    let json_relative = path_for_prd_files_storage(json_path, source_root);
    insert_prd_file(conn, prd_id, &json_relative, "task_list")?;

    // Derive prompt file path: <stem>-prompt.md
    if let Some(stem) = json_path.file_stem() {
        let prompt_name = format!("{}-prompt.md", stem.to_string_lossy());
        let prompt_path = json_path.with_file_name(&prompt_name);
        if prompt_path.exists() {
            let prompt_relative = path_for_prd_files_storage(&prompt_path, source_root);
            insert_prd_file(conn, prd_id, &prompt_relative, "prompt")?;
        }
    }

    // Store PRD markdown file if specified (already a project-relative string
    // from JSON metadata — keep as-is when relative; normalize when absolute).
    if let Some(ref prd_file) = prd.prd_file {
        let stored = if Path::new(prd_file).is_absolute() {
            path_for_prd_files_storage(Path::new(prd_file), source_root)
        } else {
            prd_file.replace('\\', "/")
        };
        insert_prd_file(conn, prd_id, &stored, "prd")?;
    }

    Ok(())
}

/// Find `prd_files` task_list rows whose pin-19 identity matches `live`.
///
/// Scans `prd_files` WHERE `file_type = 'task_list'`, joins `prd_metadata`, and
/// returns every `(prd_id, task_prefix)` where
/// [`crate::git::paths_identify`] is true. Zero hits → empty; one hit → that
/// pair; two or more → all hits (callers refuse / doctor). Does not `LIMIT 1`.
pub fn find_registered_task_lists(
    conn: &Connection,
    live: &Path,
    source_root: &Path,
    worktree_root: &Path,
) -> TaskMgrResult<Vec<(i64, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT pf.file_path, pm.id, pm.task_prefix
         FROM prd_files pf
         JOIN prd_metadata pm ON pm.id = pf.prd_id
         WHERE pf.file_type = 'task_list'",
    )?;
    let rows = stmt.query_map([], |row| {
        let file_path: String = row.get(0)?;
        let prd_id: i64 = row.get(1)?;
        let prefix: Option<String> = row.get(2)?;
        Ok((file_path, prd_id, prefix))
    })?;

    let mut hits = Vec::new();
    for row in rows {
        let (file_path, prd_id, prefix) = row?;
        let registered = PathBuf::from(file_path);
        if crate::git::paths_identify(live, &registered, source_root, worktree_root) {
            hits.push((prd_id, prefix));
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init::parse::PrdUserStory;
    use crate::db::{create_schema, open_connection, run_migrations};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    fn minimal_story(id: &str, requires_human: Option<bool>) -> PrdUserStory {
        PrdUserStory {
            id: id.to_string(),
            title: "Test Task".to_string(),
            description: None,
            priority: 1,
            passes: false,
            notes: None,
            acceptance_criteria: vec![],
            review_scope: None,
            severity: None,
            source_review: None,
            touches_files: vec![],
            depends_on: vec![],
            synergy_with: vec![],
            batch_with: vec![],
            conflicts_with: vec![],
            model: None,
            difficulty: None,
            escalation_note: None,
            required_tests: vec![],
            max_retries: None,
            requires_human,
            human_review_timeout: None,
            claims_shared_infra: None,
        }
    }

    /// insert_task stores requires_human=1 when story.requires_human = Some(true).
    /// Requires v15 DB column — ignored until FEAT task adds ALTER TABLE SQL.
    #[test]
    fn test_insert_task_stores_requires_human_true() {
        let (_temp_dir, conn) = setup_db();
        let story = minimal_story("US-001", Some(true));
        insert_task(&conn, &story, None).unwrap();

        let requires_human: i32 = conn
            .query_row(
                "SELECT requires_human FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            requires_human, 1,
            "requires_human must be stored as 1 when true"
        );
    }

    /// insert_task stores requires_human=0 when story.requires_human is None (absent).
    /// Requires v15 DB column — ignored until FEAT task adds ALTER TABLE SQL.
    #[test]
    fn test_insert_task_stores_requires_human_false_by_default() {
        let (_temp_dir, conn) = setup_db();
        let story = minimal_story("US-001", None);
        insert_task(&conn, &story, None).unwrap();

        let requires_human: i32 = conn
            .query_row(
                "SELECT requires_human FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            requires_human, 0,
            "requires_human must be 0 when absent in story"
        );
    }

    /// update_task preserves requires_human field (does not reset it to 0).
    /// Requires v15 DB column — ignored until FEAT task adds ALTER TABLE SQL.
    #[test]
    fn test_update_task_preserves_requires_human() {
        let (_temp_dir, conn) = setup_db();

        // Insert with requires_human=true
        let story = minimal_story("US-001", Some(true));
        insert_task(&conn, &story, None).unwrap();

        // Update without changing requires_human
        let updated_story = minimal_story("US-001", Some(true));
        update_task(&conn, &updated_story, None).unwrap();

        let requires_human: i32 = conn
            .query_row(
                "SELECT requires_human FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            requires_human, 1,
            "requires_human must still be 1 after update_task"
        );
    }

    /// Task::try_from reads requires_human correctly from a v15 DB row.
    /// Requires v15 DB column — ignored until FEAT task adds ALTER TABLE SQL.
    #[test]
    fn test_task_try_from_reads_requires_human_from_db() {
        use crate::models::Task;

        let (_temp_dir, conn) = setup_db();
        // Insert directly using raw SQL (after v15 column exists)
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, requires_human) \
             VALUES ('US-001', 'Test', 'todo', 1, 1)",
            [],
        )
        .unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT id, title, description, priority, status, notes, \
                 acceptance_criteria, review_scope, severity, source_review, \
                 created_at, updated_at, started_at, completed_at, \
                 last_error, error_count, requires_human, human_review_timeout \
                 FROM tasks WHERE id = 'US-001'",
            )
            .unwrap();

        let task = stmt
            .query_row([], |row| {
                Task::try_from(row).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .unwrap();

        assert!(task.requires_human);
        assert_eq!(task.human_review_timeout, None);
    }

    fn seed_prd_with_task_list(conn: &Connection, prefix: Option<&str>, file_path: &str) -> i64 {
        conn.execute(
            "INSERT INTO prd_metadata (project, task_prefix, updated_at)
             VALUES ('test', ?, datetime('now'))",
            rusqlite::params![prefix],
        )
        .unwrap();
        let prd_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |row| row.get(0))
            .unwrap();
        insert_prd_file(conn, prd_id, file_path, "task_list").unwrap();
        prd_id
    }

    #[test]
    fn find_registered_relative_and_absolute_live_identify() {
        let tmp = TempDir::new().unwrap();
        let source_raw = tmp.path().join("main");
        std::fs::create_dir_all(source_raw.join("tasks")).unwrap();
        let file = source_raw.join("tasks/foo.json");
        std::fs::write(&file, "{}").unwrap();
        let source = std::fs::canonicalize(&source_raw).unwrap();

        let (_db_tmp, conn) = setup_db();
        // Init-shaped relative path only — never bare basename.
        let prd_id = seed_prd_with_task_list(&conn, Some("abc12345"), "tasks/foo.json");

        let live = std::fs::canonicalize(&file).unwrap();
        let hits = find_registered_task_lists(&conn, &live, &source, &source).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, prd_id);
        assert_eq!(hits[0].1.as_deref(), Some("abc12345"));
    }

    #[test]
    fn find_registered_source_and_worktree_identify_as_one() {
        let tmp = TempDir::new().unwrap();
        let source_raw = tmp.path().join("main");
        let worktree_raw = tmp.path().join("wt");
        std::fs::create_dir_all(source_raw.join("tasks")).unwrap();
        std::fs::create_dir_all(worktree_raw.join("tasks")).unwrap();
        let src_file = source_raw.join("tasks/foo.json");
        let wt_file = worktree_raw.join("tasks/foo.json");
        std::fs::write(&src_file, "{}").unwrap();
        std::fs::write(&wt_file, "{}").unwrap();
        let source = std::fs::canonicalize(&source_raw).unwrap();
        let worktree = std::fs::canonicalize(&worktree_raw).unwrap();

        let (_db_tmp, conn) = setup_db();
        let prd_id = seed_prd_with_task_list(&conn, Some("wtprefix"), "tasks/foo.json");

        let live_wt = std::fs::canonicalize(&wt_file).unwrap();
        let hits = find_registered_task_lists(&conn, &live_wt, &source, &worktree).unwrap();
        assert_eq!(hits, vec![(prd_id, Some("wtprefix".into()))]);

        let live_src = std::fs::canonicalize(&src_file).unwrap();
        let hits_src = find_registered_task_lists(&conn, &live_src, &source, &worktree).unwrap();
        assert_eq!(hits_src, vec![(prd_id, Some("wtprefix".into()))]);
    }

    #[test]
    fn find_registered_different_dirs_same_basename_no_hit() {
        let tmp = TempDir::new().unwrap();
        let source_a = tmp.path().join("proj-a");
        let source_b = tmp.path().join("proj-b");
        std::fs::create_dir_all(source_a.join("tasks")).unwrap();
        std::fs::create_dir_all(source_b.join("tasks")).unwrap();
        std::fs::write(source_a.join("tasks/foo.json"), "{}").unwrap();
        let file_b = source_b.join("tasks/foo.json");
        std::fs::write(&file_b, "{}").unwrap();
        let source = std::fs::canonicalize(&source_a).unwrap();

        let (_db_tmp, conn) = setup_db();
        seed_prd_with_task_list(&conn, Some("prefix-a"), "tasks/foo.json");

        let live_b = std::fs::canonicalize(&file_b).unwrap();
        let hits = find_registered_task_lists(&conn, &live_b, &source, &source).unwrap();
        assert!(hits.is_empty(), "basename equality must not identify");
    }

    #[test]
    fn find_registered_returns_all_twins_no_limit_one() {
        let tmp = TempDir::new().unwrap();
        let source_raw = tmp.path().join("main");
        std::fs::create_dir_all(source_raw.join("tasks")).unwrap();
        let file = source_raw.join("tasks/foo.json");
        std::fs::write(&file, "{}").unwrap();
        let source = std::fs::canonicalize(&source_raw).unwrap();
        let abs = std::fs::canonicalize(&file).unwrap();

        let (_db_tmp, conn) = setup_db();
        // Twin rows: relative + absolute for the same live file (split-brain).
        let id1 = seed_prd_with_task_list(&conn, Some("twin-a"), "tasks/foo.json");
        let id2 = seed_prd_with_task_list(&conn, Some("twin-b"), abs.to_str().unwrap());

        let live = abs.clone();
        let mut hits = find_registered_task_lists(&conn, &live, &source, &source).unwrap();
        hits.sort_by_key(|(id, _)| *id);
        assert_eq!(hits.len(), 2, "twins must all be returned; callers refuse");
        assert_eq!(hits[0].0, id1);
        assert_eq!(hits[1].0, id2);
    }

    #[test]
    fn find_registered_zero_hits_empty() {
        let tmp = TempDir::new().unwrap();
        let source_raw = tmp.path().join("main");
        std::fs::create_dir_all(source_raw.join("tasks")).unwrap();
        let file = source_raw.join("tasks/other.json");
        std::fs::write(&file, "{}").unwrap();
        let source = std::fs::canonicalize(&source_raw).unwrap();

        let (_db_tmp, conn) = setup_db();
        seed_prd_with_task_list(&conn, Some("only-foo"), "tasks/foo.json");

        let live = std::fs::canonicalize(&file).unwrap();
        let hits = find_registered_task_lists(&conn, &live, &source, &source).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn path_for_prd_files_storage_relative_then_absolute_same_string() {
        let tmp = TempDir::new().unwrap();
        let source_raw = tmp.path().join("proj");
        std::fs::create_dir_all(source_raw.join("tasks")).unwrap();
        let file = source_raw.join("tasks/foo.json");
        std::fs::write(&file, "{}").unwrap();
        let source = std::fs::canonicalize(&source_raw).unwrap();
        let abs = std::fs::canonicalize(&file).unwrap();

        let from_rel = path_for_prd_files_storage(Path::new("tasks/foo.json"), &source);
        let from_abs = path_for_prd_files_storage(&abs, &source);
        assert_eq!(from_rel, "tasks/foo.json");
        assert_eq!(from_abs, "tasks/foo.json");
    }

    #[test]
    fn register_relative_then_absolute_one_canonical_row() {
        let tmp = TempDir::new().unwrap();
        let source_raw = tmp.path().join("proj");
        std::fs::create_dir_all(source_raw.join("tasks")).unwrap();
        let file = source_raw.join("tasks/foo.json");
        std::fs::write(&file, r#"{"project":"p","userStories":[]}"#).unwrap();
        // Prompt beside the JSON so both task_list + prompt register.
        std::fs::write(source_raw.join("tasks/foo-prompt.md"), "# p").unwrap();
        let source = std::fs::canonicalize(&source_raw).unwrap();
        let abs = std::fs::canonicalize(&file).unwrap();

        let (_db_tmp, conn) = setup_db();
        let prd = PrdFile {
            project: "p".into(),
            branch_name: None,
            description: None,
            priority_philosophy: None,
            global_acceptance_criteria: None,
            review_guidelines: None,
            user_stories: vec![],
            external_git_repo: None,
            task_prefix: Some("abc12345".into()),
            prd_file: None,
            model: None,
            default_max_retries: None,
            implicit_overlap_files: None,
        };
        let prd_id = insert_prd_metadata(&conn, &prd, None).unwrap();

        // First register via relative-shaped path under source_root.
        let rel_path = source.join("tasks/foo.json");
        register_prd_files(&conn, prd_id, &rel_path, &prd, &source).unwrap();
        // Second register via absolute path — INSERT OR IGNORE + same stored
        // string must not create a twin file_path row.
        register_prd_files(&conn, prd_id, &abs, &prd, &source).unwrap();

        let paths: Vec<String> = conn
            .prepare("SELECT file_path FROM prd_files WHERE prd_id = ? AND file_type = 'task_list'")
            .unwrap()
            .query_map([prd_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(paths, vec!["tasks/foo.json".to_string()]);

        let prompt_paths: Vec<String> = conn
            .prepare("SELECT file_path FROM prd_files WHERE prd_id = ? AND file_type = 'prompt'")
            .unwrap()
            .query_map([prd_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(prompt_paths, vec!["tasks/foo-prompt.md".to_string()]);
    }

    #[test]
    fn path_for_prd_files_storage_outside_source_root_is_absolute_stable() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("proj");
        let outside = tmp.path().join("other");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let file = outside.join("orphan.json");
        std::fs::write(&file, "{}").unwrap();
        let source = std::fs::canonicalize(&source).unwrap();
        let file = std::fs::canonicalize(&file).unwrap();

        let stored = path_for_prd_files_storage(&file, &source);
        assert!(
            Path::new(&stored).is_absolute(),
            "outside source_root must store absolute form, got {stored}"
        );
        assert_eq!(stored, file.to_string_lossy().replace('\\', "/"));
    }

    #[test]
    fn resolve_prd_file_path_joins_source_root_relative() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("proj");
        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(source.join("tasks")).unwrap();
        std::fs::create_dir_all(worktree.join("tasks")).unwrap();
        let got = resolve_prd_file_path(Path::new("tasks/foo.json"), &source, &worktree);
        assert_eq!(got, worktree.join("tasks/foo.json"));
    }

    #[test]
    fn register_prd_files_no_tasks_dir_strip_prefix() {
        // Grep-guard companion: storage must not use .task-mgr/tasks strip.
        let src = include_str!("import.rs");
        let start = src
            .find("pub fn register_prd_files(")
            .expect("register_prd_files present");
        let body = &src[start..];
        let end = body[1..]
            .find("\npub fn ")
            .map(|i| i + 1)
            .unwrap_or(body.len());
        let fn_body = &body[..end];
        assert!(
            !fn_body.contains("strip_prefix(tasks_dir)") && !fn_body.contains(".task-mgr/tasks"),
            "register_prd_files must not strip_prefix(.task-mgr/tasks)"
        );
    }
}
