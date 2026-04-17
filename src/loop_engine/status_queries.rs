//! Data-fetching functions for the status dashboard.
//!
//! Queries the database for project metadata, task counts, pending tasks,
//! and distinct prefixes. Also reads `.deadline-*` files from the tasks directory.
//! All results are returned as types defined in `status.rs`.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::db::lock::LockGuard;
use crate::db::prefix::prefix_and;

use super::DEADLINE_FILE_PREFIX;
use super::status::{DashboardTaskCounts, DeadlineInfo, PendingTask, ProjectInfo};

/// PRD hints read from the JSON file before lock acquisition.
pub struct PrdHints {
    pub task_prefix: Option<String>,
    pub branch_name: Option<String>,
}

/// Read `taskPrefix` and `branchName` from a PRD JSON file in a single pass.
///
/// Returns `PrdHints { task_prefix: None, branch_name: None }` if the file is
/// unreadable or not valid JSON.
pub fn read_prd_hints(prd_path: &Path) -> PrdHints {
    let Some(content) = fs::read_to_string(prd_path).ok() else {
        return PrdHints {
            task_prefix: None,
            branch_name: None,
        };
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return PrdHints {
            task_prefix: None,
            branch_name: None,
        };
    };
    PrdHints {
        task_prefix: json
            .get("taskPrefix")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        branch_name: json
            .get("branchName")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

/// Read the `taskPrefix` field from a PRD JSON file.
pub fn read_task_prefix_from_prd(prd_path: &Path) -> Option<String> {
    read_prd_hints(prd_path).task_prefix
}

/// Read the `branchName` field from a PRD JSON file.
///
/// Returns `None` if the file is unreadable, not valid JSON, or the field is absent.
pub fn read_branch_name_from_prd(prd_path: &Path) -> Option<String> {
    read_prd_hints(prd_path).branch_name
}

/// Query project metadata from prd_metadata table.
///
/// When `task_prefix` is provided, queries `WHERE task_prefix = ?` to select the
/// matching PRD row. Falls back to `LIMIT 1 ORDER BY id ASC` when no prefix is given.
pub(crate) fn query_project_info(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<Option<ProjectInfo>> {
    let result = if let Some(prefix) = task_prefix {
        conn.query_row(
            "SELECT project, branch_name, description FROM prd_metadata WHERE task_prefix = ?1",
            rusqlite::params![prefix],
            |row| {
                Ok(ProjectInfo {
                    name: row.get(0)?,
                    branch: row.get(1)?,
                    description: row.get(2)?,
                })
            },
        )
    } else {
        conn.query_row(
            "SELECT project, branch_name, description FROM prd_metadata ORDER BY id ASC LIMIT 1",
            [],
            |row| {
                Ok(ProjectInfo {
                    name: row.get(0)?,
                    branch: row.get(1)?,
                    description: row.get(2)?,
                })
            },
        )
    };

    match result {
        Ok(info) => Ok(Some(info)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Query task counts grouped by status.
///
/// When `task_prefix` is provided, only counts tasks whose ID starts with `{prefix}-`.
pub(crate) fn query_dashboard_task_counts(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<DashboardTaskCounts> {
    let (and_clause, like_pattern) = prefix_and(task_prefix);

    let sql = format!(
        r#"
        SELECT
            COUNT(*) as total,
            COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0) as done,
            COALESCE(SUM(CASE WHEN status = 'todo' THEN 1 ELSE 0 END), 0) as todo,
            COALESCE(SUM(CASE WHEN status = 'in_progress' THEN 1 ELSE 0 END), 0) as in_progress,
            COALESCE(SUM(CASE WHEN status = 'blocked' THEN 1 ELSE 0 END), 0) as blocked,
            COALESCE(SUM(CASE WHEN status = 'skipped' THEN 1 ELSE 0 END), 0) as skipped,
            COALESCE(SUM(CASE WHEN status = 'irrelevant' THEN 1 ELSE 0 END), 0) as irrelevant
        FROM tasks
        WHERE archived_at IS NULL
        {and_clause}
        "#,
    );

    let mut stmt = conn.prepare(&sql)?;
    let counts = if let Some(ref pattern) = like_pattern {
        stmt.query_row(rusqlite::params![pattern], |row| {
            Ok(DashboardTaskCounts {
                total: row.get(0)?,
                done: row.get(1)?,
                todo: row.get(2)?,
                in_progress: row.get(3)?,
                blocked: row.get(4)?,
                skipped: row.get(5)?,
                irrelevant: row.get(6)?,
            })
        })?
    } else {
        stmt.query_row([], |row| {
            Ok(DashboardTaskCounts {
                total: row.get(0)?,
                done: row.get(1)?,
                todo: row.get(2)?,
                in_progress: row.get(3)?,
                blocked: row.get(4)?,
                skipped: row.get(5)?,
                irrelevant: row.get(6)?,
            })
        })?
    };

    Ok(counts)
}

/// Query pending tasks (todo + in_progress + blocked) ordered by priority.
///
/// When `task_prefix` is provided, only returns tasks whose ID starts with `{prefix}-`.
pub(crate) fn query_pending_tasks(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<Vec<PendingTask>> {
    let (and_clause, like_pattern) = prefix_and(task_prefix);

    let sql = format!(
        r#"
        SELECT id, title, priority, status
        FROM tasks
        WHERE status IN ('todo', 'in_progress', 'blocked') AND archived_at IS NULL
        {and_clause}
        ORDER BY priority ASC, id ASC
        "#,
    );

    let mut stmt = conn.prepare(&sql)?;
    let tasks = if let Some(ref pattern) = like_pattern {
        stmt.query_map(rusqlite::params![pattern], |row| {
            Ok(PendingTask {
                id: row.get(0)?,
                title: row.get(1)?,
                priority: row.get(2)?,
                status: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([], |row| {
            Ok(PendingTask {
                id: row.get(0)?,
                title: row.get(1)?,
                priority: row.get(2)?,
                status: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };

    Ok(tasks)
}

/// Query distinct task ID prefixes present in the tasks table.
///
/// A prefix is the part of a task ID before the first `-` separator.
/// Returns prefixes in sorted order.
pub(crate) fn query_distinct_prefixes(conn: &Connection) -> TaskMgrResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT SUBSTR(id, 1, INSTR(id, '-') - 1) FROM tasks WHERE INSTR(id, '-') > 0 AND archived_at IS NULL ORDER BY 1",
    )?;
    let prefixes = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(prefixes)
}

/// Read active loop locks and return the prefixes of all lock holders.
///
/// Scans for per-prefix lock files (`loop-{prefix}.lock`) as well as the
/// legacy global `loop.lock`. Returns an empty vec if no active locks found.
pub(crate) fn read_active_lock_prefixes(dir: &Path) -> Vec<String> {
    let mut prefixes = Vec::new();

    // Check per-prefix lock files: loop-*.lock
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("loop-")
                && name_str.ends_with(".lock")
                && let Some(info) = LockGuard::read_holder_info(&entry.path())
                && let Some(p) = info.prefix
            {
                prefixes.push(p);
            }
        }
    }

    // Check legacy global lock file
    let global_lock = dir.join("loop.lock");
    if let Some(info) = LockGuard::read_holder_info(&global_lock)
        && let Some(p) = info.prefix
        && !prefixes.contains(&p)
    {
        prefixes.push(p);
    }

    prefixes
}

/// Read deadline info from .deadline-* files in the tasks directory.
///
/// If a specific PRD file is provided, reads only its deadline.
/// Otherwise, scans for any .deadline-* files.
pub(crate) fn read_deadline_info(dir: &Path, prd_file: Option<&Path>) -> Option<DeadlineInfo> {
    let tasks_dir = dir.join("tasks");

    if let Some(prd) = prd_file {
        let basename = prd_basename_from_path(prd);
        return read_single_deadline(&tasks_dir, &basename);
    }

    // Scan for any .deadline-* file
    let entries = fs::read_dir(&tasks_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(DEADLINE_FILE_PREFIX) {
            let basename = name_str
                .trim_start_matches(DEADLINE_FILE_PREFIX)
                .to_string();
            if let Some(info) = read_single_deadline(&tasks_dir, &basename) {
                return Some(info);
            }
        }
    }

    None
}

/// Read a single deadline file and compute remaining time.
fn read_single_deadline(tasks_dir: &Path, basename: &str) -> Option<DeadlineInfo> {
    let path = tasks_dir.join(format!("{}{}", DEADLINE_FILE_PREFIX, basename));
    let content = fs::read_to_string(&path).ok()?;
    let deadline_epoch: u64 = content.trim().parse().ok()?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs();

    let (expired, seconds_remaining) = if now >= deadline_epoch {
        (true, 0u64)
    } else {
        (false, deadline_epoch - now)
    };

    let time_remaining = format_remaining(seconds_remaining);

    Some(DeadlineInfo {
        prd_basename: basename.to_string(),
        expired,
        seconds_remaining,
        time_remaining,
    })
}

/// Derive PRD basename from file path (strip directory and .json extension).
fn prd_basename_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Format remaining seconds as human-readable string.
pub(crate) fn format_remaining(seconds: u64) -> String {
    if seconds == 0 {
        return "expired".to_string();
    }

    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;

    if hours > 0 {
        format!("{}h {}m remaining", hours, minutes)
    } else if minutes > 0 {
        format!("{}m remaining", minutes)
    } else {
        format!("{}s remaining", seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::test_utils::{insert_prd_metadata, insert_task, setup_test_db};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;

    #[test]
    fn test_query_project_info_no_metadata() {
        let (_temp_dir, conn) = setup_test_db();
        let result = query_project_info(&conn, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_query_project_info_with_metadata() {
        let (_temp_dir, conn) = setup_test_db();
        insert_prd_metadata(&conn, "test-proj", Some("develop"), None);
        let result = query_project_info(&conn, None).unwrap();
        let info = result.unwrap();
        assert_eq!(info.name, "test-proj");
        assert_eq!(info.branch.unwrap(), "develop");
        assert!(info.description.is_none());
    }

    #[test]
    fn test_query_dashboard_task_counts_empty() {
        let (_temp_dir, conn) = setup_test_db();
        let counts = query_dashboard_task_counts(&conn, None).unwrap();
        assert_eq!(counts.total, 0);
        assert_eq!(counts.done, 0);
    }

    #[test]
    fn test_query_pending_tasks_ordering() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "B-002", "Task B", "todo", 20);
        insert_task(&conn, "A-001", "Task A", "todo", 10);
        insert_task(&conn, "C-003", "Task C", "in_progress", 10);

        let tasks = query_pending_tasks(&conn, None).unwrap();
        assert_eq!(tasks.len(), 3);
        // Priority 10 first, then by ID
        assert_eq!(tasks[0].id, "A-001");
        assert_eq!(tasks[1].id, "C-003");
        assert_eq!(tasks[2].id, "B-002");
    }

    #[test]
    fn test_query_pending_tasks_excludes_done_skipped_irrelevant() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "T-001", "Done", "done", 10);
        insert_task(&conn, "T-002", "Skipped", "skipped", 20);
        insert_task(&conn, "T-003", "Irrelevant", "irrelevant", 30);
        insert_task(&conn, "T-004", "Todo", "todo", 40);

        let tasks = query_pending_tasks(&conn, None).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "T-004");
    }

    #[test]
    fn test_query_task_counts_with_prefix_filter() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "abc123-FEAT-001", "A feat 1", "done", 10);
        insert_task(&conn, "abc123-FEAT-002", "A feat 2", "todo", 20);
        insert_task(&conn, "def456-FEAT-001", "B feat 1", "done", 10);
        insert_task(&conn, "def456-FEAT-002", "B feat 2", "done", 20);
        insert_task(&conn, "def456-FEAT-003", "B feat 3", "todo", 30);

        let all = query_dashboard_task_counts(&conn, None).unwrap();
        assert_eq!(all.total, 5);
        assert_eq!(all.done, 3);

        let a = query_dashboard_task_counts(&conn, Some("abc123")).unwrap();
        assert_eq!(a.total, 2);
        assert_eq!(a.done, 1);
        assert_eq!(a.todo, 1);

        let b = query_dashboard_task_counts(&conn, Some("def456")).unwrap();
        assert_eq!(b.total, 3);
        assert_eq!(b.done, 2);
        assert_eq!(b.todo, 1);
    }

    #[test]
    fn test_query_pending_tasks_with_prefix_filter() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "abc123-FEAT-001", "A todo", "todo", 10);
        insert_task(&conn, "def456-FEAT-001", "B todo", "todo", 10);
        insert_task(&conn, "def456-FEAT-002", "B blocked", "blocked", 20);

        let all = query_pending_tasks(&conn, None).unwrap();
        assert_eq!(all.len(), 3);

        let a = query_pending_tasks(&conn, Some("abc123")).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].id, "abc123-FEAT-001");

        let b = query_pending_tasks(&conn, Some("def456")).unwrap();
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn test_prefix_filter_nonexistent_returns_zero() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "abc123-FEAT-001", "A feat", "done", 10);

        let counts = query_dashboard_task_counts(&conn, Some("zzz999")).unwrap();
        assert_eq!(counts.total, 0);
    }

    #[test]
    fn test_format_remaining_expired() {
        assert_eq!(format_remaining(0), "expired");
    }

    #[test]
    fn test_format_remaining_seconds() {
        assert_eq!(format_remaining(30), "30s remaining");
    }

    #[test]
    fn test_format_remaining_minutes() {
        assert_eq!(format_remaining(300), "5m remaining");
    }

    #[test]
    fn test_format_remaining_hours_and_minutes() {
        assert_eq!(format_remaining(5400), "1h 30m remaining");
    }

    #[test]
    fn test_read_prd_hints_both_fields() {
        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("my-prd.json");
        fs::write(
            &prd_path,
            r#"{"branchName": "feat/my-branch", "taskPrefix": "abc123"}"#,
        )
        .unwrap();
        let hints = read_prd_hints(&prd_path);
        assert_eq!(hints.task_prefix, Some("abc123".to_string()));
        assert_eq!(hints.branch_name, Some("feat/my-branch".to_string()));
    }

    #[test]
    fn test_read_prd_hints_prefix_only() {
        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("my-prd.json");
        fs::write(&prd_path, r#"{"taskPrefix": "abc123"}"#).unwrap();
        let hints = read_prd_hints(&prd_path);
        assert_eq!(hints.task_prefix, Some("abc123".to_string()));
        assert_eq!(hints.branch_name, None);
    }

    #[test]
    fn test_read_prd_hints_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("nonexistent.json");
        let hints = read_prd_hints(&prd_path);
        assert_eq!(hints.task_prefix, None);
        assert_eq!(hints.branch_name, None);
    }

    #[test]
    fn test_read_prd_hints_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("bad.json");
        fs::write(&prd_path, "not json").unwrap();
        let hints = read_prd_hints(&prd_path);
        assert_eq!(hints.task_prefix, None);
        assert_eq!(hints.branch_name, None);
    }

    #[test]
    fn test_read_branch_name_from_prd_with_field() {
        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("my-prd.json");
        fs::write(
            &prd_path,
            r#"{"branchName": "feat/my-branch", "taskPrefix": "abc123"}"#,
        )
        .unwrap();
        assert_eq!(
            read_branch_name_from_prd(&prd_path),
            Some("feat/my-branch".to_string())
        );
    }

    #[test]
    fn test_read_branch_name_from_prd_without_field() {
        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("my-prd.json");
        fs::write(&prd_path, r#"{"taskPrefix": "abc123"}"#).unwrap();
        assert_eq!(read_branch_name_from_prd(&prd_path), None);
    }

    #[test]
    fn test_read_branch_name_from_prd_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("nonexistent.json");
        assert_eq!(read_branch_name_from_prd(&prd_path), None);
    }

    #[test]
    fn test_prd_basename_from_path() {
        assert_eq!(
            prd_basename_from_path(Path::new(".task-mgr/tasks/my-prd.json")),
            "my-prd"
        );
        assert_eq!(
            prd_basename_from_path(Path::new("/abs/path/task-mgr-final.json")),
            "task-mgr-final"
        );
    }

    #[test]
    fn test_prd_basename_no_extension() {
        assert_eq!(
            prd_basename_from_path(Path::new(".task-mgr/tasks/my-prd")),
            "my-prd"
        );
    }

    #[test]
    fn test_read_deadline_future() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();

        let future_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        fs::write(tasks_dir.join(".deadline-my-prd"), future_epoch.to_string()).unwrap();

        let result = read_deadline_info(temp_dir.path(), None).unwrap();
        assert_eq!(result.prd_basename, "my-prd");
        assert!(!result.expired);
        assert!(result.seconds_remaining > 3500);
    }

    #[test]
    fn test_read_deadline_expired() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();

        let past_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 10;
        fs::write(tasks_dir.join(".deadline-test"), past_epoch.to_string()).unwrap();

        let result = read_deadline_info(temp_dir.path(), None).unwrap();
        assert!(result.expired);
        assert_eq!(result.seconds_remaining, 0);
        assert_eq!(result.time_remaining, "expired");
    }

    #[test]
    fn test_read_active_lock_prefixes_empty_dir() {
        let temp_dir = TempDir::new().unwrap();
        let prefixes = read_active_lock_prefixes(temp_dir.path());
        assert!(prefixes.is_empty());
    }

    #[test]
    fn test_read_active_lock_prefixes_finds_per_prefix_locks() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Write per-prefix lock files with prefix metadata
        fs::write(
            dir.join("loop-abc123.lock"),
            "100@testhost\nprefix=abc123\n",
        )
        .unwrap();
        fs::write(
            dir.join("loop-def456.lock"),
            "200@testhost\nprefix=def456\n",
        )
        .unwrap();

        let prefixes = read_active_lock_prefixes(dir);
        assert_eq!(prefixes.len(), 2);
        assert!(prefixes.contains(&"abc123".to_string()));
        assert!(prefixes.contains(&"def456".to_string()));
    }

    #[test]
    fn test_read_active_lock_prefixes_includes_global_lock() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Write a global loop.lock with prefix metadata
        fs::write(dir.join("loop.lock"), "100@testhost\nprefix=global1\n").unwrap();

        let prefixes = read_active_lock_prefixes(dir);
        assert_eq!(prefixes.len(), 1);
        assert_eq!(prefixes[0], "global1");
    }

    #[test]
    fn test_read_active_lock_prefixes_no_duplicate_from_global() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Both per-prefix and global lock with same prefix
        fs::write(
            dir.join("loop-abc123.lock"),
            "100@testhost\nprefix=abc123\n",
        )
        .unwrap();
        fs::write(dir.join("loop.lock"), "100@testhost\nprefix=abc123\n").unwrap();

        let prefixes = read_active_lock_prefixes(dir);
        assert_eq!(prefixes.len(), 1, "should deduplicate: {:?}", prefixes);
        assert_eq!(prefixes[0], "abc123");
    }

    // -----------------------------------------------------------------------
    // Acceptance-criteria tests: archived tasks excluded from queries
    // -----------------------------------------------------------------------

    /// query_dashboard_task_counts returns 0 for a prefix after all its tasks
    /// are soft-archived.
    #[test]
    fn test_query_task_counts_returns_zero_for_archived_prefix() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "PA-001", "Task 1", "done", 10);
        insert_task(&conn, "PA-002", "Task 2", "todo", 20);

        // Soft-archive all PA tasks
        conn.execute(
            "UPDATE tasks SET archived_at = datetime('now') WHERE id LIKE 'PA-%'",
            [],
        )
        .unwrap();

        let counts = query_dashboard_task_counts(&conn, Some("PA")).unwrap();
        assert_eq!(
            counts.total, 0,
            "Archived tasks must not appear in total count"
        );
        assert_eq!(counts.done, 0, "Archived done tasks must not appear");
        assert_eq!(counts.todo, 0, "Archived todo tasks must not appear");
    }

    /// query_pending_tasks must not return tasks whose archived_at IS NOT NULL.
    #[test]
    fn test_query_pending_tasks_skips_archived_tasks() {
        let (_temp_dir, conn) = setup_test_db();
        insert_task(&conn, "PA-001", "Active todo", "todo", 10);
        insert_task(&conn, "PA-002", "Archived todo", "todo", 20);

        // Soft-archive PA-002
        conn.execute(
            "UPDATE tasks SET archived_at = datetime('now') WHERE id = 'PA-002'",
            [],
        )
        .unwrap();

        let pending = query_pending_tasks(&conn, None).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "Only non-archived pending tasks must be returned"
        );
        assert_eq!(pending[0].id, "PA-001");
    }
}
