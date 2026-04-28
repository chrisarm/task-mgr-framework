//! Task selection algorithm for the next command.
//!
//! This module implements the smart task selection algorithm that considers:
//! - Task priority
//! - File locality (overlap with --after-files)
//!
//! # Performance
//!
//! The algorithm is optimized for PRDs with 100-200 tasks, achieving sub-5ms
//! performance through separate simple queries and in-memory scoring.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::Serialize;

use crate::TaskMgrResult;
use crate::db::prefix::{prefix_and, prefix_and_col, prefix_where_col};
use crate::loop_engine::calibrate;
use crate::models::Task;

/// Scoring weights for task selection
pub const FILE_OVERLAP_SCORE: i32 = 10;
pub const PRIORITY_BASE: i32 = 1000;

/// A scored task candidate for selection.
#[derive(Debug, Clone, Serialize)]
pub struct ScoredTask {
    /// The task being scored
    pub task: Task,
    /// Files this task touches
    pub files: Vec<String>,
    /// Total calculated score
    pub total_score: i32,
    /// Breakdown of how the score was calculated
    pub score_breakdown: ScoreBreakdown,
}

/// Breakdown of score calculation for debugging/transparency.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreBreakdown {
    /// Score from priority (1000 - priority)
    pub priority_score: i32,
    /// Score from file overlap with --after-files
    pub file_score: i32,
    /// Number of files that overlapped
    pub file_overlap_count: i32,
}

/// Result of the task selection algorithm.
#[derive(Debug, Clone, Serialize)]
pub struct SelectionResult {
    /// The selected task (if any eligible tasks exist)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<ScoredTask>,
    /// Reason for selection (or why no task was selected)
    pub selection_reason: String,
    /// Total number of eligible tasks considered
    pub eligible_count: usize,
    /// Top 5 candidates with scoring (for verbose output)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_candidates: Vec<ScoredTask>,
}

/// Select the next task to work on using the smart selection algorithm.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `after_files` - Files modified in the previous iteration (for locality scoring)
/// * `task_prefix` - Optional prefix to scope selection to a single PRD
///
/// # Returns
///
/// Returns a `SelectionResult` with the best task to work on, or None if no tasks are eligible.
///
/// # Algorithm
///
/// 1. Filter to eligible tasks: status='todo' and all dependsOn tasks are done/irrelevant
/// 2. Score each task:
///    - priority_score = 1000 - priority (higher priority = higher score)
///    - file_score = 10 * count of files overlapping with after_files
///    - total_score = priority_score + file_score
/// 3. Order by total_score DESC, priority ASC
pub fn select_next_task(
    conn: &Connection,
    after_files: &[String],
    task_prefix: Option<&str>,
) -> TaskMgrResult<SelectionResult> {
    // Get IDs of tasks that are done or irrelevant (satisfy dependencies)
    let completed_ids = get_completed_task_ids(conn, task_prefix)?;

    // Get all todo tasks
    let todo_tasks = get_todo_tasks(conn, task_prefix)?;

    // Get all relationships
    let dependencies = get_relationships_by_type(conn, "dependsOn", task_prefix)?;

    // Get task files
    let task_files = get_all_task_files(conn, task_prefix)?;

    // Filter to eligible tasks (all dependencies satisfied)
    let eligible_tasks: Vec<Task> = todo_tasks
        .into_iter()
        .filter(|task| {
            let task_deps = dependencies
                .get(&task.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            task_deps
                .iter()
                .all(|dep_id| completed_ids.contains(dep_id))
        })
        .collect();

    if eligible_tasks.is_empty() {
        return Ok(SelectionResult {
            task: None,
            selection_reason: "No eligible tasks found - all tasks are either complete, blocked by dependencies, or in a non-todo state".to_string(),
            eligible_count: 0,
            top_candidates: Vec::new(),
        });
    }

    // Load dynamic weights (falls back to defaults if not calibrated)
    let weights = calibrate::load_dynamic_weights(conn);

    // Convert after_files to a set for O(1) lookup
    let after_files_set: HashSet<&str> = after_files.iter().map(String::as_str).collect();

    // Score each eligible task
    let mut scored_tasks: Vec<ScoredTask> = eligible_tasks
        .into_iter()
        .map(|task| {
            let files = task_files.get(&task.id).cloned().unwrap_or_default();

            // Calculate file overlap score
            let file_overlap_count = files
                .iter()
                .filter(|f| after_files_set.contains(f.as_str()))
                .count() as i32;
            let file_score = file_overlap_count * weights.file_overlap;

            // Calculate priority score (higher priority = lower number = higher score)
            let priority_score = weights.priority_base - task.priority;

            // Total score: priority + file overlap only
            let total_score = priority_score + file_score;

            ScoredTask {
                task,
                files,
                total_score,
                score_breakdown: ScoreBreakdown {
                    priority_score,
                    file_score,
                    file_overlap_count,
                },
            }
        })
        .collect();

    // Sort by total_score DESC, then by priority ASC (as tiebreaker)
    scored_tasks.sort_by(|a, b| {
        b.total_score
            .cmp(&a.total_score)
            .then_with(|| a.task.priority.cmp(&b.task.priority))
    });

    let eligible_count = scored_tasks.len();

    // Keep top 5 candidates for verbose output
    let top_candidates: Vec<ScoredTask> = scored_tasks.iter().take(5).cloned().collect();

    // Get the top task
    let top_task = scored_tasks.into_iter().next();

    match top_task {
        Some(task) => {
            let selection_reason = format!(
                "Selected task {} with score {} (priority: {}, file_overlap: {})",
                task.task.id,
                task.total_score,
                task.score_breakdown.priority_score,
                task.score_breakdown.file_score,
            );

            Ok(SelectionResult {
                task: Some(task),
                selection_reason,
                eligible_count,
                top_candidates,
            })
        }
        None => Ok(SelectionResult {
            task: None,
            selection_reason: "No eligible tasks found".to_string(),
            eligible_count: 0,
            top_candidates: Vec::new(),
        }),
    }
}

/// Get IDs of tasks that are done or irrelevant (can satisfy dependencies).
fn get_completed_task_ids(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<HashSet<String>> {
    let (prefix_clause, prefix_param) = prefix_and(task_prefix);
    let sql = format!(
        "SELECT id FROM tasks WHERE status IN ('done', 'irrelevant') AND archived_at IS NULL {prefix_clause}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let ids: Result<HashSet<String>, rusqlite::Error> = if let Some(pattern) = prefix_param {
        stmt.query_map([pattern], |row| row.get(0))?.collect()
    } else {
        stmt.query_map([], |row| row.get(0))?.collect()
    };
    Ok(ids?)
}

/// Get all tasks with status='todo'.
fn get_todo_tasks(conn: &Connection, task_prefix: Option<&str>) -> TaskMgrResult<Vec<Task>> {
    let (prefix_clause, prefix_param) = prefix_and(task_prefix);
    let sql = format!(
        "SELECT id, title, description, priority, status, notes, \
         acceptance_criteria, review_scope, severity, source_review, \
         created_at, updated_at, started_at, completed_at, \
         last_error, error_count, \
         blocked_at_iteration, skipped_at_iteration, \
         model, difficulty, escalation_note, \
         requires_human, human_review_timeout \
         FROM tasks WHERE status = 'todo' AND archived_at IS NULL {prefix_clause} ORDER BY priority ASC"
    );
    let mut stmt = conn.prepare(&sql)?;

    let map_err = |e: crate::TaskMgrError| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    };

    let tasks: Result<Vec<Task>, rusqlite::Error> = if let Some(pattern) = prefix_param {
        stmt.query_map([pattern], |row| Task::try_from(row).map_err(map_err))?
            .collect()
    } else {
        stmt.query_map([], |row| Task::try_from(row).map_err(map_err))?
            .collect()
    };

    Ok(tasks?)
}

/// Get all relationships of a specific type, grouped by task_id.
fn get_relationships_by_type(
    conn: &Connection,
    rel_type: &str,
    task_prefix: Option<&str>,
) -> TaskMgrResult<HashMap<String, Vec<String>>> {
    let (prefix_clause, prefix_param) = prefix_and_col("task_id", task_prefix);
    let sql = format!(
        "SELECT task_id, related_id FROM task_relationships WHERE rel_type = ? {prefix_clause}"
    );
    let mut stmt = conn.prepare(&sql)?;

    let rows: Result<Vec<(String, String)>, rusqlite::Error> = if let Some(pattern) = prefix_param {
        stmt.query_map(rusqlite::params![rel_type, pattern], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect()
    } else {
        stmt.query_map([rel_type], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect()
    };

    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, related_id) in rows? {
        map.entry(task_id).or_default().push(related_id);
    }

    Ok(map)
}

/// Get all task files, grouped by task_id.
fn get_all_task_files(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<HashMap<String, Vec<String>>> {
    let (prefix_clause, prefix_param) = prefix_where_col("task_id", task_prefix);
    let sql = format!("SELECT task_id, file_path FROM task_files {prefix_clause}");
    let mut stmt = conn.prepare(&sql)?;

    let rows: Result<Vec<(String, String)>, rusqlite::Error> = if let Some(pattern) = prefix_param {
        stmt.query_map([pattern], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect()
    } else {
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect()
    };

    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, file_path) in rows? {
        map.entry(task_id).or_default().push(file_path);
    }

    Ok(map)
}

/// Select up to `max_slots` non-conflicting tasks for parallel execution.
///
/// # Algorithm
///
/// 1. Score all eligible tasks identically to `select_next_task`.
/// 2. Sort by total_score DESC, priority ASC.
/// 3. Greedy pass: accept each candidate unless any of its files appear in the
///    set of files already claimed by an accepted task.
/// 4. Tasks with zero `touchesFiles` entries have no conflicts and are always
///    eligible.
/// 5. Stop once `max_slots` tasks are accepted.
///
/// The returned group is ordered by total_score descending.
pub fn select_parallel_group(
    conn: &Connection,
    after_files: &[String],
    task_prefix: Option<&str>,
    max_slots: usize,
) -> TaskMgrResult<Vec<ScoredTask>> {
    if max_slots == 0 {
        return Ok(Vec::new());
    }

    let completed_ids = get_completed_task_ids(conn, task_prefix)?;
    let todo_tasks = get_todo_tasks(conn, task_prefix)?;
    let dependencies = get_relationships_by_type(conn, "dependsOn", task_prefix)?;
    let task_files = get_all_task_files(conn, task_prefix)?;

    let eligible_tasks: Vec<Task> = todo_tasks
        .into_iter()
        .filter(|task| {
            let task_deps = dependencies
                .get(&task.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            task_deps
                .iter()
                .all(|dep_id| completed_ids.contains(dep_id))
        })
        .collect();

    if eligible_tasks.is_empty() {
        return Ok(Vec::new());
    }

    let weights = calibrate::load_dynamic_weights(conn);
    let after_files_set: HashSet<&str> = after_files.iter().map(String::as_str).collect();

    let mut scored_tasks: Vec<ScoredTask> = eligible_tasks
        .into_iter()
        .map(|task| {
            let files = task_files.get(&task.id).cloned().unwrap_or_default();

            let file_overlap_count = files
                .iter()
                .filter(|f| after_files_set.contains(f.as_str()))
                .count() as i32;
            let file_score = file_overlap_count * weights.file_overlap;
            let priority_score = weights.priority_base - task.priority;
            let total_score = priority_score + file_score;

            ScoredTask {
                task,
                files,
                total_score,
                score_breakdown: ScoreBreakdown {
                    priority_score,
                    file_score,
                    file_overlap_count,
                },
            }
        })
        .collect();

    scored_tasks.sort_by(|a, b| {
        b.total_score
            .cmp(&a.total_score)
            .then_with(|| a.task.priority.cmp(&b.task.priority))
    });

    // Greedy selection: borrow file slices from task_files (not candidate) so
    // candidate can be moved into group while used_files retains its borrows.
    let mut group: Vec<ScoredTask> = Vec::new();
    let mut used_files: HashSet<&str> = HashSet::new();

    for candidate in scored_tasks {
        if group.len() >= max_slots {
            break;
        }
        let files = task_files
            .get(&candidate.task.id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        if !files.is_empty() && files.iter().any(|f| used_files.contains(f.as_str())) {
            continue;
        }
        for f in files {
            used_files.insert(f.as_str());
        }
        group.push(candidate);
    }

    Ok(group)
}

/// Format selection result as human-readable text.
pub fn format_text(result: &SelectionResult) -> String {
    let mut output = String::new();

    match &result.task {
        Some(task) => {
            output.push_str(&format!(
                "Next Task: {} - {}\n",
                task.task.id, task.task.title
            ));
            output.push_str(&format!("{}\n\n", "=".repeat(60)));

            output.push_str(&format!("Priority: {}\n", task.task.priority));
            output.push_str(&format!("Score:    {}\n", task.total_score));

            output.push_str("\nScore Breakdown:\n");
            output.push_str(&format!(
                "  Priority:    {:+}\n",
                task.score_breakdown.priority_score
            ));
            output.push_str(&format!(
                "  File Overlap: {:+} ({} file(s))\n",
                task.score_breakdown.file_score, task.score_breakdown.file_overlap_count
            ));

            if !task.files.is_empty() {
                output.push_str("\nTouches Files:\n");
                for file in &task.files {
                    output.push_str(&format!("  - {}\n", file));
                }
            }

            if let Some(ref desc) = task.task.description {
                output.push_str(&format!("\nDescription:\n  {}\n", desc));
            }

            output.push_str(&format!("\nEligible Tasks: {}", result.eligible_count));
        }
        None => {
            output.push_str("No tasks available for selection.\n\n");
            output.push_str(&result.selection_reason);
        }
    }

    output
}
