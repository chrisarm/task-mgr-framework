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
use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use crate::TaskMgrResult;
use crate::db::prefix::{prefix_and, prefix_and_col, prefix_where_col};
use crate::loop_engine::calibrate;
use crate::models::Task;

/// Scoring weights for task selection
pub const FILE_OVERLAP_SCORE: i32 = 10;
pub const PRIORITY_BASE: i32 = 1000;

/// Task-ID prefixes that the loop spawns as ad-hoc fixup children.
///
/// A milestone-class candidate whose acceptance_criteria text references one of
/// these prefixes (with trailing dash, e.g. `REFACTOR-N-001`, `REFACTOR-N-xxx`)
/// is deferred while a same-prefix sibling is still `todo`/`in_progress`.
/// Tasks whose own ID body matches one of these prefixes are exempt — sibling
/// fixups remain co-schedulable.
///
/// **AC writing convention** (PRD authors): `mentioned_fixup_prefixes` matches
/// `token.starts_with("{prefix}-")` after splitting AC text on non-`[A-Z0-9-]`
/// chars. So the guard fires when the prefix appears as a **standalone token**
/// (`REFACTOR-N-xxx`, `CODE-FIX-001`, slash-separated lists like
/// `CODE-FIX/WIRE-FIX/IMPL-FIX/REFACTOR-N`). Writing the fully task-prefixed
/// form like `cbd7d081-REFACTOR-N-xxx` tokenizes as one token starting with
/// `cbd7d081-` and **silently bypasses the guard**. If you're authoring an AC
/// that should defer a milestone, use the bare prefix.
const SPAWNED_FIXUP_PREFIXES: &[&str] = &["REFACTOR-N", "CODE-FIX", "WIRE-FIX", "IMPL-FIX"];

/// Task-ID prefixes that are treated as "buildy" — they touch shared build
/// infrastructure (Cargo.lock, lockfiles, generated config) and so must
/// contend for a single synthetic shared-infra slot per parallel wave (FEAT-003).
///
/// **Superset relationship**: This list is a strict superset of
/// `SPAWNED_FIXUP_PREFIXES`. The fixup prefixes are buildy because they
/// originate from CODE-REVIEW spawns that often touch lockfiles or shared
/// build state; raw `FEAT` and `REFACTOR` (without `-N`) are buildy because
/// new feature work typically pulls dependencies. If you add a new ad-hoc
/// spawn prefix to `SPAWNED_FIXUP_PREFIXES`, also add it here so the
/// shared-infra slot semantics stay consistent.
///
/// Tasks whose id does NOT match a buildy prefix (TEST, CLARIFY, DOCS,
/// MILESTONE, CODE-REVIEW, HUMAN-REVIEW, etc.) parallelize freely — they
/// only contend on real path overlap.
pub(crate) const BUILDY_TASK_PREFIXES: &[&str] = &[
    "FEAT",
    "REFACTOR",
    "REFACTOR-N",
    "CODE-FIX",
    "WIRE-FIX",
    "IMPL-FIX",
];

/// Baseline list of build-system files that act as a single shared-infra slot
/// in `select_parallel_group` (FEAT-003). Match is by basename across ANY
/// path in a task's `touchesFiles` (so `examples/foo/Cargo.lock` matches as
/// readily as `Cargo.lock`).
///
/// Covers Rust / Python / JavaScript / Go ecosystems out-of-the-box.
/// Per-project (`ProjectConfig::implicit_overlap_files`) and per-PRD
/// (`PrdFile::implicit_overlap_files`) entries EXTEND this baseline rather
/// than replacing it, so users opt IN to extra shared-infra files without
/// losing the language defaults.
pub(crate) const IMPLICIT_OVERLAP_FILES: &[&str] = &[
    // Rust
    "Cargo.lock",
    "Cargo.toml",
    // Python
    "uv.lock",
    "pyproject.toml",
    "requirements.txt",
    "requirements.lock",
    "Pipfile",
    "Pipfile.lock",
    "poetry.lock",
    // JavaScript / TypeScript
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "npm-shrinkwrap.json",
    "bun.lockb",
    // Go
    "go.mod",
    "go.sum",
    "go.work",
    "go.work.sum",
];

/// Synthetic claim token added to `used_files` in `select_parallel_group`
/// when a task is recognized as a shared-infra-claimer. NOT a real path —
/// the leading underscores guarantee no real `touchesFiles` entry collides.
const SHARED_INFRA_TOKEN: &str = "__shared_infra__";

/// True iff `id` contains the literal `{prefix}-` token at a `-`-bounded
/// position (start-of-id OR following a `-`).
///
/// Examples (prefix = `CODE-FIX`):
/// - `CODE-FIX-001` → true (starts with `CODE-FIX-`)
/// - `PRD-A-CODE-FIX-001` → true (contains `-CODE-FIX-`)
/// - `CODE-FIXTURE-1` → false (the trailing `-` boundary is mandatory)
fn id_body_matches_prefix(id: &str, prefix: &str) -> bool {
    let needle = format!("{prefix}-");
    id.starts_with(&needle) || id.contains(&format!("-{needle}"))
}

/// True iff `id` matches any [`BUILDY_TASK_PREFIXES`] entry via the same
/// token-aware boundary check used by the soft-dep guard. Reuses
/// [`id_body_matches_prefix`] verbatim — no parallel matcher.
///
/// Critically handles UUID-prefixed task ids like `cbd7d081-FEAT-001` (a
/// naive `id.starts_with("FEAT")` check would silently miss them).
pub(crate) fn id_has_buildy_prefix(id: &str) -> bool {
    BUILDY_TASK_PREFIXES
        .iter()
        .any(|p| id_body_matches_prefix(id, p))
}

/// True iff any path in `files` has a basename that appears in `implicit_set`.
/// Falls back to the full path string when `Path::file_name()` returns None
/// (e.g. inputs like `"."` or `""`); the implicit set never contains those
/// sentinels so the lookup is harmless and saves a None-branch in callers.
fn has_implicit_overlap(files: &[String], implicit_set: &HashSet<&str>) -> bool {
    files.iter().any(|f| {
        let basename = Path::new(f.as_str())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(f.as_str());
        implicit_set.contains(basename)
    })
}

/// Apply `claims_shared_infra` precedence to the implicit-detection result.
/// `Some(true)` forces the claim; `Some(false)` opts out completely; `None`
/// falls through to `via_path || via_prefix`.
fn resolve_shared_infra_claim(
    explicit_override: Option<bool>,
    via_path: bool,
    via_prefix: bool,
) -> bool {
    match explicit_override {
        Some(true) => true,
        Some(false) => false,
        None => via_path || via_prefix,
    }
}

/// True iff `task.id` itself matches any fixup prefix (sibling fixups are
/// co-schedulable and must never self-block).
fn task_is_self_fixup(task: &Task) -> bool {
    SPAWNED_FIXUP_PREFIXES
        .iter()
        .any(|prefix| id_body_matches_prefix(&task.id, prefix))
}

/// Tokenize each AC string on non-`[A-Z0-9-]` chars and return every fixup
/// prefix whose `{prefix}-` needle appears as a token. Needles are computed
/// once per call, not once per token.
fn mentioned_fixup_prefixes(task: &Task) -> Vec<&'static str> {
    let needles: Vec<String> = SPAWNED_FIXUP_PREFIXES
        .iter()
        .map(|p| format!("{p}-"))
        .collect();
    let mut mentioned: Vec<&'static str> = Vec::new();
    for ac in &task.acceptance_criteria {
        for token in ac.split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-'))
        {
            for (prefix, needle) in SPAWNED_FIXUP_PREFIXES.iter().copied().zip(needles.iter()) {
                if token.starts_with(needle.as_str()) && !mentioned.contains(&prefix) {
                    mentioned.push(prefix);
                }
            }
        }
    }
    mentioned
}

/// Collect every active ID (excluding `self_id`) that matches any of the given
/// fixup prefixes via [`id_body_matches_prefix`].
fn find_active_blockers_for_prefixes(
    prefixes: &[&str],
    active_ids: &HashSet<String>,
    self_id: &str,
) -> Vec<String> {
    let mut blockers: Vec<String> = active_ids
        .iter()
        .filter(|id| id.as_str() != self_id)
        .filter(|id| prefixes.iter().any(|p| id_body_matches_prefix(id, p)))
        .cloned()
        .collect();
    blockers.sort();
    blockers
}

/// Return the active sibling IDs that block `task` per the soft-dep filter,
/// or an empty vec if `task` is not blocked.
fn find_blocking_active_fixups(task: &Task, active_ids: &HashSet<String>) -> Vec<String> {
    if task_is_self_fixup(task) {
        return Vec::new();
    }
    let mentioned = mentioned_fixup_prefixes(task);
    if mentioned.is_empty() {
        return Vec::new();
    }
    find_active_blockers_for_prefixes(&mentioned, active_ids, &task.id)
}

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

/// Score, filter, and sort all eligible todo tasks.
///
/// Shared by `select_next_task` and `select_parallel_group`. Returns tasks
/// sorted by total_score DESC, priority ASC — the callers diverge only in how
/// they pick from this ordered list.
///
/// Filters applied (in order): formal `dependsOn` deps complete, then the
/// soft-dep guard (see [`find_blocking_active_fixups`]) which defers
/// milestone candidates whose AC text references a same-prefix active sibling.
fn build_scored_candidates(
    conn: &Connection,
    after_files: &[String],
    task_prefix: Option<&str>,
) -> TaskMgrResult<Vec<ScoredTask>> {
    let completed_ids = get_completed_task_ids(conn, task_prefix)?;
    let active_ids = get_active_task_ids(conn, task_prefix)?;
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
        .filter(|task| {
            let blockers = find_blocking_active_fixups(task, &active_ids);
            if blockers.is_empty() {
                return true;
            }
            eprintln!(
                "Deferring {}: AC references active fixup task(s): {}",
                task.id,
                blockers.join(", ")
            );
            false
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

    Ok(scored_tasks)
}

/// Select the next task to work on using the smart selection algorithm.
///
/// # Algorithm
///
/// 1. Filter to eligible tasks: status='todo' and all dependsOn tasks are done/irrelevant
/// 2. Score each task: priority_score + file_overlap_score
/// 3. Return the highest-scored task
pub fn select_next_task(
    conn: &Connection,
    after_files: &[String],
    task_prefix: Option<&str>,
) -> TaskMgrResult<SelectionResult> {
    let scored_tasks = build_scored_candidates(conn, after_files, task_prefix)?;

    if scored_tasks.is_empty() {
        return Ok(SelectionResult {
            task: None,
            selection_reason: "No eligible tasks found - all tasks are either complete, blocked by dependencies, or in a non-todo state".to_string(),
            eligible_count: 0,
            top_candidates: Vec::new(),
        });
    }

    let eligible_count = scored_tasks.len();
    let top_candidates: Vec<ScoredTask> = scored_tasks.iter().take(5).cloned().collect();
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

/// Get IDs of tasks that are currently active (todo or in_progress) and not
/// archived. Used by the soft-dep filter to defer milestone candidates whose
/// AC references a still-active fixup sibling.
fn get_active_task_ids(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<HashSet<String>> {
    let (prefix_clause, prefix_param) = prefix_and(task_prefix);
    let sql = format!(
        "SELECT id FROM tasks WHERE status IN ('todo', 'in_progress') AND archived_at IS NULL {prefix_clause}"
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
         requires_human, human_review_timeout, \
         claims_shared_infra \
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
/// 3. Greedy pass: accept each candidate unless any file it claims appears in
///    the set of files already claimed by an accepted task.
/// 4. Tasks with zero `touchesFiles` entries AND no synthetic shared-infra
///    claim have no conflicts and are always eligible.
/// 5. Stop once `max_slots` tasks are accepted.
///
/// # Implicit shared-infra detection (FEAT-003)
///
/// In addition to a candidate's `touchesFiles`, a synthetic
/// [`SHARED_INFRA_TOKEN`] is added to the claim set when ANY of:
///
/// - **(a)** a file in `candidate.files` has a basename in
///   [`IMPLICIT_OVERLAP_FILES`] ∪ `extra_implicit_overlap_files` (both are
///   matched by basename — full paths like `examples/foo/Cargo.lock` count).
/// - **(b)** the candidate's id matches a [`BUILDY_TASK_PREFIXES`] entry via
///   [`id_has_buildy_prefix`].
/// - **(c)** the candidate's `claims_shared_infra` field is `Some(true)`.
///
/// `claims_shared_infra: Some(false)` overrides BOTH (a) and (b) — explicit
/// opt-out for tasks the operator knows are safe to parallelize.
/// `claims_shared_infra: None` falls through to (a) ∨ (b).
///
/// The synthetic claim lives ONLY in the in-memory `used_files` set; a task's
/// persisted `touchesFiles` is never mutated.
///
/// # Arguments
///
/// * `extra_implicit_overlap_files` — Project- and PRD-level extensions to the
///   baseline implicit list. Pass `&[]` when no extensions apply.
///
/// The returned group is ordered by total_score descending.
pub fn select_parallel_group(
    conn: &Connection,
    after_files: &[String],
    task_prefix: Option<&str>,
    max_slots: usize,
    extra_implicit_overlap_files: &[String],
) -> TaskMgrResult<Vec<ScoredTask>> {
    if max_slots == 0 {
        return Ok(Vec::new());
    }

    let scored_tasks = build_scored_candidates(conn, after_files, task_prefix)?;

    if scored_tasks.is_empty() {
        return Ok(Vec::new());
    }

    // Union of baseline + project-config + PRD extensions, computed once per call.
    let implicit_set: HashSet<&str> = IMPLICIT_OVERLAP_FILES
        .iter()
        .copied()
        .chain(extra_implicit_overlap_files.iter().map(String::as_str))
        .collect();

    let mut group: Vec<ScoredTask> = Vec::new();
    let mut used_files: HashSet<String> = HashSet::new();

    for candidate in scored_tasks {
        if group.len() >= max_slots {
            break;
        }

        // Compute the candidate's full claim set: real files + (optionally) the
        // synthetic shared-infra token. `via_path` is computed before `via_prefix`
        // so an opt-out (`Some(false)`) costs only the path check.
        let via_path = has_implicit_overlap(&candidate.files, &implicit_set);
        let via_prefix = id_has_buildy_prefix(&candidate.task.id);
        let claims_infra =
            resolve_shared_infra_claim(candidate.task.claims_shared_infra, via_path, via_prefix);

        // Conflict check on the COMBINED claim set so the synthetic token
        // serializes buildy tasks with otherwise-disjoint files.
        let real_overlap = candidate
            .files
            .iter()
            .any(|f| used_files.contains(f.as_str()));
        let infra_overlap = claims_infra && used_files.contains(SHARED_INFRA_TOKEN);
        if real_overlap || infra_overlap {
            continue;
        }

        used_files.extend(candidate.files.iter().cloned());
        if claims_infra {
            used_files.insert(SHARED_INFRA_TOKEN.to_string());
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
