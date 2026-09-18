//! Health check functions for the doctor command.
//!
//! These functions detect various database inconsistencies:
//! - Stale in_progress tasks without active runs
//! - Active runs without proper end
//! - Orphaned relationships referencing non-existent tasks
//! - Tasks completed in git history but not marked done in DB
//! - Path-identity twins (2+ prd_metadata rows for one task_list file)

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::commands::init::import::{find_registered_task_lists, resolve_prd_file_path};
use crate::db::LockGuard;
use crate::db::prefix::make_like_pattern;

/// One `prd_metadata` side of a path-identity twin cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathIdentityTwinSide {
    pub prd_id: i64,
    /// `None` when `prd_metadata.task_prefix` is NULL.
    pub prefix: Option<String>,
    /// Unarchived task count for `prefix-%`. `None` when prefix is NULL
    /// (cannot LIKE-scope — never auto-fix this side).
    pub live_task_count: Option<i64>,
}

/// A cluster of 2+ prd_ids whose task_list paths share pin-19 identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathIdentityTwin {
    pub sides: Vec<PathIdentityTwinSide>,
    /// Representative stored/live path string for the issue description.
    pub sample_path: String,
}

impl PathIdentityTwin {
    /// Entity id for JSON/text: `prefix_a|prefix_b|...` with NULL shown as `NULL`.
    pub fn entity_id(&self) -> String {
        self.sides
            .iter()
            .map(|s| s.prefix.as_deref().unwrap_or("NULL"))
            .collect::<Vec<_>>()
            .join("|")
    }

    /// Human-readable description including prefixes and live counts.
    pub fn description(&self) -> String {
        let sides_desc = self
            .sides
            .iter()
            .map(|s| {
                let pfx = s.prefix.as_deref().unwrap_or("NULL");
                match s.live_task_count {
                    Some(n) => format!("'{pfx}' ({n} unarchived)"),
                    None => format!("'{pfx}' (NULL prefix — live count unknown)"),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let autofix_note = match self.autofix_empty_sides().as_slice() {
            [] => {
                "Report only — no auto-fix (need exactly one empty prefixed side with another live side; never pick among live twins)."
                    .to_string()
            }
            empty => {
                let names: Vec<&str> = empty
                    .iter()
                    .map(|s| s.prefix.as_deref().unwrap_or("NULL"))
                    .collect();
                format!(
                    "Auto-fix can DELETE prd_metadata for empty side(s): {}.",
                    names.join(", ")
                )
            }
        };
        format!(
            "Path-identity twins for '{}': {}. {}",
            self.sample_path, sides_desc, autofix_note
        )
    }

    /// Sides safe to drop: `live_task_count == Some(0)`, and at least one other
    /// side has live tasks (`Some(n) where n > 0`). NULL-prefix and all-empty
    /// clusters never auto-fix. All-done / all-irrelevant still count as live.
    pub fn autofix_empty_sides(&self) -> Vec<&PathIdentityTwinSide> {
        let has_live = self
            .sides
            .iter()
            .any(|s| s.live_task_count.is_some_and(|c| c > 0));
        if !has_live {
            return Vec::new();
        }
        self.sides
            .iter()
            .filter(|s| s.live_task_count == Some(0))
            .collect()
    }
}

/// Count unarchived tasks whose id matches `prefix-%` (ESCAPE).
///
/// Returns `None` when `prefix` is `None` — NULL-prefix sides cannot be
/// LIKE-scoped and must never be treated as empty-enough to auto-fix.
pub fn count_unarchived_tasks_for_prefix(
    conn: &Connection,
    prefix: Option<&str>,
) -> TaskMgrResult<Option<i64>> {
    let Some(prefix) = prefix else {
        return Ok(None);
    };
    let pattern = make_like_pattern(prefix);
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE id LIKE ? ESCAPE '\\' AND archived_at IS NULL",
        rusqlite::params![pattern],
        |row| row.get(0),
    )?;
    Ok(Some(count))
}

/// Find clusters of 2+ `prd_metadata` rows whose task_list paths identify as one file.
///
/// Uses [`find_registered_task_lists`] (pin-19) — no `LIMIT 1`. Dedupes by sorted
/// prd_id set. Missing on-disk files yield no identity hit (paths_identify fails
/// closed) and are skipped.
pub fn find_path_identity_twins(
    conn: &Connection,
    source_root: &Path,
    worktree_root: &Path,
) -> TaskMgrResult<Vec<PathIdentityTwin>> {
    let mut stmt = conn.prepare(
        "SELECT pf.file_path, pm.id, pm.task_prefix
         FROM prd_files pf
         JOIN prd_metadata pm ON pm.id = pf.prd_id
         WHERE pf.file_type = 'task_list'
         ORDER BY pm.id",
    )?;
    let rows = stmt.query_map([], |row| {
        let file_path: String = row.get(0)?;
        let prd_id: i64 = row.get(1)?;
        let prefix: Option<String> = row.get(2)?;
        Ok((file_path, prd_id, prefix))
    })?;

    let mut seen_clusters: BTreeSet<Vec<i64>> = BTreeSet::new();
    let mut twins = Vec::new();

    for row in rows {
        let (file_path, _prd_id, _prefix) = row?;
        let stored = PathBuf::from(&file_path);
        let live = resolve_prd_file_path(&stored, source_root, worktree_root);
        // Identity requires a canonicalize-able live path; skip missing files.
        if !live.exists() {
            continue;
        }
        let hits = find_registered_task_lists(conn, &live, source_root, worktree_root)?;
        if hits.len() < 2 {
            continue;
        }

        let mut prd_ids: Vec<i64> = hits.iter().map(|(id, _)| *id).collect();
        prd_ids.sort_unstable();
        prd_ids.dedup();
        if prd_ids.len() < 2 {
            continue;
        }
        if !seen_clusters.insert(prd_ids.clone()) {
            continue;
        }

        // Preserve hit order but unique by prd_id for side building.
        let mut by_id: BTreeMap<i64, Option<String>> = BTreeMap::new();
        for (id, pfx) in hits {
            by_id.entry(id).or_insert(pfx);
        }

        let mut sides = Vec::with_capacity(by_id.len());
        for (prd_id, prefix) in by_id {
            let live_task_count = count_unarchived_tasks_for_prefix(conn, prefix.as_deref())?;
            sides.push(PathIdentityTwinSide {
                prd_id,
                prefix,
                live_task_count,
            });
        }

        twins.push(PathIdentityTwin {
            sides,
            sample_path: file_path,
        });
    }

    Ok(twins)
}

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
        AND archived_at IS NULL
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

/// Check whether any loop lock file in the DB directory is actively held.
///
/// Scans for `loop*.lock` files and attempts a non-blocking exclusive lock
/// on each. If any lock cannot be acquired, a loop is actively running and
/// its runs should NOT be auto-fixed by doctor.
///
/// Returns `true` if at least one loop lock is held by a live process.
pub fn has_active_loop_lock(dir: &Path) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("loop") && name_str.ends_with(".lock") {
            let path = entry.path();
            // If we can read a holder PID and it's still alive, the lock is active
            if let Some(info) = LockGuard::read_holder_info(&path)
                && is_pid_alive(info.pid)
            {
                return true;
            }
        }
    }

    false
}

/// Check if a process with the given PID is still alive.
fn is_pid_alive(pid: u32) -> bool {
    // kill(pid, 0) checks for process existence without sending a signal
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Find relationships where related_id references a non-existent or archived task.
///
/// Note: We intentionally don't have a foreign key on related_id to allow
/// importing tasks with forward references, so we check this manually.
///
/// Relationships pointing to soft-archived tasks are also reported since
/// the archive command hard-deletes task_relationships for the archived
/// prefix. Cross-prefix relationships (e.g., PA-001 depends on PB-001
/// where PB is archived) will surface here as expected.
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

/// Collect local and remote branch ref short names via a single `git for-each-ref`.
///
/// Returns `None` when git is unavailable or `dir` is not a repo.
fn list_local_and_remote_branches(dir: &Path) -> Option<HashSet<String>> {
    let output = Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ])
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.contains(" -> "))
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
    )
}

/// Find active PRDs whose recorded branch does not exist locally or on any remote.
///
/// Returns `(task_prefix, project, branch_name)` for PRDs that still have at least
/// one unarchived task and whose `branch_name` is absent from `refs/heads` and
/// `refs/remotes`.
pub fn find_orphan_branch_prds(
    conn: &Connection,
    dir: &Path,
) -> TaskMgrResult<Vec<(String, String, String)>> {
    let branch_refs = match list_local_and_remote_branches(dir) {
        Some(refs) => refs,
        None => return Ok(Vec::new()),
    };

    let mut stmt = conn.prepare(
        r#"
        SELECT task_prefix, project, branch_name
        FROM prd_metadata
        WHERE branch_name IS NOT NULL
        AND branch_name != ''
        AND task_prefix IS NOT NULL
        AND EXISTS (
            SELECT 1
            FROM tasks
            WHERE id LIKE prd_metadata.task_prefix || '-%' ESCAPE '\'
            AND archived_at IS NULL
        )
        ORDER BY task_prefix
        "#,
    )?;

    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;

    let mut results = Vec::new();
    for row in rows {
        let (task_prefix, project, branch_name): (String, String, String) = row?;
        let branch_exists = branch_refs.contains(&branch_name)
            || branch_refs.iter().any(|entry| {
                entry
                    .split_once('/')
                    .is_some_and(|(_, stripped)| stripped == branch_name)
            });

        if !branch_exists {
            results.push((task_prefix, project, branch_name));
        }
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
