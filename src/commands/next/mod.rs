//! Smart task selection for the next command.
//!
//! This module implements the `next` command's task selection algorithm that
//! considers file locality, dependencies, and priority scoring.
//! It also provides the main `next()` entry point that integrates task selection,
//! claiming, and learnings retrieval.
//!
//! # Performance Characteristics
//!
//! The task selection algorithm is optimized for PRDs with 100-200 tasks. Benchmarks
//! show consistent performance well under the 50ms target:
//!
//! | Scenario                      | Time    | Notes                                |
//! |-------------------------------|---------|--------------------------------------|
//! | 200 tasks (mixed status)      | ~3.5ms  | 120 todo, 50 done, 30 other         |
//! | 200 tasks (worst case)        | ~4.2ms  | All todo with chain dependencies    |
//! | 200 tasks with many files     | ~6.3ms  | 10 files/task, 50 after_files       |
//!
//! ## Architecture Decision: Separate Queries vs Single JOIN
//!
//! The selection algorithm uses multiple simple queries instead of a single complex
//! JOIN query. This approach was chosen because:
//!
//! 1. **Readability**: Each query has a clear, single purpose
//! 2. **Flexibility**: In-memory processing allows complex scoring logic
//! 3. **Performance**: With proper indexes, individual queries are <1ms total
//! 4. **Maintainability**: Easier to test and modify scoring weights
//!
//! A single SQL-based approach would require:
//! - Complex CTEs for dependency checking
//! - GROUP BY with JSON aggregation for files/relationships
//! - Custom SQL functions for scoring
//!
//! The current approach achieves sub-5ms performance, making SQL optimization unnecessary.
//!
//! ## Index Usage
//!
//! The queries rely on these indexes (see `db/schema.rs`):
//!
//! - `idx_tasks_status_priority`: For `SELECT ... WHERE status IN (...) ORDER BY priority`
//! - `idx_task_relationships_type_taskid`: Covering index for relationship queries
//! - `idx_task_files_task_id`: For file lookups (full scan is acceptable since we need all)
//!
//! ## Scaling Considerations
//!
//! For PRDs significantly larger than 200 tasks, consider:
//! - Pagination with LIMIT/OFFSET for eligible task fetching
//! - SQL-based pre-filtering if many tasks have unmet dependencies
//! - Caching relationship maps if they rarely change
//!
//! Current performance is adequate for typical AI agent loop usage where PRDs
//! contain 50-200 tasks and iterations run serially.
//!
//! # Security Considerations
//!
//! ## JSON Output and Shell Safety
//!
//! This module outputs JSON containing task data that may be consumed by shell scripts.
//! The JSON includes user-provided content from PRD files:
//!
//! - Task descriptions, titles, and notes
//! - Acceptance criteria
//! - File paths from `touchesFiles`
//! - Learning content (failure messages, patterns, workarounds)
//!
//! **Important:** The output is JSON-serialized via serde, which properly escapes
//! special characters. However, consuming shell scripts must handle this JSON safely:
//!
//! - **DO NOT** embed JSON values directly into shell commands using backticks or `$()`
//! - **DO NOT** use `eval` on JSON content
//! - **DO** use `jq` to extract values and store in variables
//! - **DO** quote variables when passing to commands
//!
//! The shell scripts in `scripts/` follow these guidelines. The JSON content is passed
//! to `claude -p` as a string argument, not executed as a shell command.
//!
//! ## Trust Model
//!
//! PRD files are considered trusted input - they are provided by the user and imported
//! via the `init` command. While `touchesFiles` paths are validated for traversal,
//! the text content (descriptions, notes, criteria) is passed through without
//! sanitization because:
//!
//! 1. The content is never executed as shell commands by task-mgr
//! 2. The content is serialized as JSON (properly escaped)
//! 3. The consuming scripts use proper JSON parsing (jq)
//! 4. The final consumer (Claude) treats it as text, not commands

pub mod decay;
pub mod output;
pub mod selection;

#[cfg(test)]
mod tests;

use std::path::Path;

use rusqlite::Connection;

use crate::TaskMgrError;
use crate::TaskMgrResult;
use crate::db::open_and_migrate as open_connection;
use crate::learnings::recall::{RecallParams, recall_learnings};

// Re-export public types
pub use decay::{DecayWarning, apply_decay, find_decay_warnings};
pub use output::{
    CandidateSummary, ClaimMetadata, LearningSummaryOutput, NextResult, NextTaskOutput,
    ScoreOutput, SelectionMetadata, build_task_output, format_next_text, format_next_verbose,
};
pub use selection::{
    FILE_OVERLAP_SCORE, PRIORITY_BASE, ScoreBreakdown, ScoredTask, SelectionResult, format_text,
    select_next_task,
};

/// Main entry point for the next command.
///
/// This function integrates:
/// 1. Task selection (via `select_next_task`)
/// 2. Task claiming (if `--claim` flag is provided)
/// 3. Learnings retrieval (for the selected task)
///
/// # Arguments
///
/// * `dir` - Directory containing the database
/// * `after_files` - Files modified in the previous iteration (for locality scoring)
/// * `claim` - Whether to claim the task (set status to in_progress)
/// * `run_id` - Optional run ID for tracking
/// * `verbose` - Whether to include verbose output (top 5 candidates with scoring)
///
/// # Returns
///
/// Returns a `NextResult` with the selected task, learnings, and metadata.
pub fn next(
    dir: &Path,
    after_files: &[String],
    claim: bool,
    run_id: Option<&str>,
    verbose: bool,
    task_prefix: Option<&str>,
) -> TaskMgrResult<NextResult> {
    // Open connection once and reuse for all operations
    let conn = open_connection(dir)?;

    // Step 1: Run task selection
    let selection = select_next_task(&conn, after_files, task_prefix)?;

    // Build top candidates for verbose output
    let top_candidates = if verbose {
        selection
            .top_candidates
            .iter()
            .map(|st| CandidateSummary {
                id: st.task.id.clone(),
                title: st.task.title.clone(),
                priority: st.task.priority,
                total_score: st.total_score,
                score: ScoreOutput {
                    total: st.total_score,
                    priority: st.score_breakdown.priority_score,
                    file_overlap: st.score_breakdown.file_score,
                    file_overlap_count: st.score_breakdown.file_overlap_count,
                },
            })
            .collect()
    } else {
        Vec::new()
    };

    // Return early if no task selected
    let Some(ref scored_task) = selection.task else {
        return Ok(NextResult {
            task: None,
            learnings: Vec::new(),
            selection: SelectionMetadata {
                reason: selection.selection_reason.clone(),
                eligible_count: selection.eligible_count,
            },
            claim: None,
            top_candidates,
        });
    };

    // Step 2: Claim task if requested
    let claim_metadata = if claim {
        Some(claim_task(&conn, &scored_task.task.id, run_id)?)
    } else {
        None
    };

    // Step 3: Retrieve relevant learnings (with graceful degradation on error)
    let learnings = retrieve_learnings_for_task(&conn, &scored_task.task.id);

    // Build task output
    let task_output = build_task_output(scored_task, claim);

    Ok(NextResult {
        task: Some(task_output),
        learnings,
        selection: SelectionMetadata {
            reason: selection.selection_reason.clone(),
            eligible_count: selection.eligible_count,
        },
        claim: claim_metadata,
        top_candidates,
    })
}

/// Claim a task by setting status to in_progress.
fn claim_task(
    conn: &Connection,
    task_id: &str,
    run_id: Option<&str>,
) -> TaskMgrResult<ClaimMetadata> {
    // If run_id provided, verify run exists and is active.
    // If the run was externally aborted (e.g., by `doctor --auto-fix`), proceed
    // without run linkage rather than crashing the loop.
    let mut effective_run_id = run_id;
    if let Some(rid) = run_id {
        let run_status: Result<String, _> =
            conn.query_row("SELECT status FROM runs WHERE run_id = ?1", [rid], |row| {
                row.get(0)
            });

        match run_status {
            Ok(status) if status != "active" => {
                eprintln!(
                    "Warning: run '{}' was externally changed to '{}'; continuing without run linkage",
                    rid, status
                );
                effective_run_id = None;
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(TaskMgrError::run_not_found(rid));
            }
            Err(e) => return Err(e.into()),
            Ok(_) => {} // Run exists and is active
        }
    }

    // Update task status to in_progress with optimistic locking
    // Only claim if task is still in 'todo' status to prevent race conditions
    let rows_affected = conn.execute(
        "UPDATE tasks SET status = 'in_progress', started_at = datetime('now'), updated_at = datetime('now') WHERE id = ?1 AND status = 'todo'",
        [task_id],
    )?;

    // If no rows affected, task was already claimed by another process
    if rows_affected == 0 {
        // Check if task exists and what its current status is
        let current_status: Result<String, _> =
            conn.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            });

        return match current_status {
            Ok(status) => Err(TaskMgrError::invalid_state(
                "task", task_id, "todo", &status,
            )),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(TaskMgrError::task_not_found(task_id)),
            Err(e) => Err(e.into()),
        };
    }

    // Link to run if run_id provided (uses effective_run_id which may be None
    // if the run was externally aborted)
    if let Some(rid) = effective_run_id {
        // Get current iteration for this run
        let current_iteration: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(iteration), 0) FROM run_tasks WHERE run_id = ?1",
                [rid],
                |row| row.get(0),
            )
            .unwrap_or(0);

        conn.execute(
            r#"
            INSERT INTO run_tasks (run_id, task_id, status, iteration, started_at)
            VALUES (?1, ?2, 'started', ?3, datetime('now'))
            "#,
            rusqlite::params![rid, task_id, current_iteration + 1],
        )?;
    }

    // Increment global iteration counter and update last_task_id
    conn.execute(
        "UPDATE global_state SET iteration_counter = iteration_counter + 1, last_task_id = ?1, updated_at = datetime('now') WHERE id = 1",
        [task_id],
    )?;

    // Get current iteration
    let iteration: i64 = conn.query_row(
        "SELECT iteration_counter FROM global_state WHERE id = 1",
        [],
        |row| row.get(0),
    )?;

    Ok(ClaimMetadata {
        claimed: true,
        run_id: run_id.map(String::from),
        iteration,
    })
}

/// Retrieve relevant learnings for a task with graceful error handling.
fn retrieve_learnings_for_task(conn: &Connection, task_id: &str) -> Vec<LearningSummaryOutput> {
    let recall_params = RecallParams {
        for_task: Some(task_id.to_string()),
        limit: 5,
        ..Default::default()
    };

    match recall_learnings(conn, recall_params) {
        Ok(result) => result
            .learnings
            .into_iter()
            .map(LearningSummaryOutput::from)
            .collect(),
        Err(e) => {
            // Log warning but don't fail the command
            eprintln!("Warning: failed to retrieve learnings: {}", e);
            Vec::new()
        }
    }
}
