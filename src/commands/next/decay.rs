//! Decay management for blocked and skipped tasks.
//!
//! This module implements automatic decay of tasks that have been blocked or skipped
//! for too long. Tasks are reset to 'todo' status after exceeding a configurable
//! iteration threshold.
//!
//! # Behavior
//!
//! - **Blocked tasks**: Reset to 'todo' after `threshold` iterations since blocking
//! - **Skipped tasks**: Reset to 'todo' after `threshold` iterations since skipping
//! - **Irrelevant tasks**: Never decay (they are permanently marked as not needed)
//!
//! # Audit Trail
//!
//! When a task decays, an audit note is appended to the task's notes field
//! documenting the automatic reset.

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Apply automatic decay to blocked/skipped tasks that have exceeded the threshold.
///
/// Tasks that have been blocked or skipped for longer than `threshold` iterations
/// are automatically reset to 'todo' status. Irrelevant tasks do NOT decay.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `threshold` - Number of iterations after which a blocked/skipped task decays
/// * `verbose` - If true, log verbose information to stderr
///
/// # Returns
///
/// A vector of (task_id, previous_status) tuples for tasks that were decayed.
pub fn apply_decay(
    conn: &Connection,
    threshold: i64,
    verbose: bool,
) -> TaskMgrResult<Vec<(String, String)>> {
    if threshold <= 0 {
        return Ok(Vec::new());
    }

    // Get the current global iteration
    let current_iteration: i64 = conn
        .query_row(
            "SELECT iteration_counter FROM global_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if verbose {
        eprintln!(
            "[verbose] Checking for decayed tasks (threshold: {} iterations, current: {})",
            threshold, current_iteration
        );
    }

    // Find tasks that need to decay:
    // - blocked tasks where (current_iteration - blocked_at_iteration) >= threshold
    // - skipped tasks where (current_iteration - skipped_at_iteration) >= threshold
    let mut stmt = conn.prepare(
        r#"
        SELECT id, status, blocked_at_iteration, skipped_at_iteration
        FROM tasks
        WHERE (
            (status = 'blocked' AND blocked_at_iteration IS NOT NULL
             AND (?1 - blocked_at_iteration) >= ?2)
            OR
            (status = 'skipped' AND skipped_at_iteration IS NOT NULL
             AND (?1 - skipped_at_iteration) >= ?2)
        )
        ORDER BY id
        "#,
    )?;

    let tasks_to_decay: Vec<(String, String)> = stmt
        .query_map([current_iteration, threshold], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if tasks_to_decay.is_empty() {
        return Ok(Vec::new());
    }

    // Build audit note for decayed tasks
    let audit_note = format!(
        "[DECAY] Auto-reset from blocked/skipped to todo after {} iterations (threshold: {})",
        current_iteration, threshold
    );

    // Reset decayed tasks back to todo
    for (task_id, _old_status) in &tasks_to_decay {
        // Get current notes
        let current_notes: Option<String> = conn
            .query_row("SELECT notes FROM tasks WHERE id = ?", [task_id], |row| {
                row.get(0)
            })
            .ok();

        // Build new notes with audit message
        let new_notes = match current_notes {
            Some(existing) if !existing.is_empty() => format!("{}\n\n{}", existing, audit_note),
            _ => audit_note.clone(),
        };

        // Reset task: status -> todo, clear decay iteration columns
        conn.execute(
            r#"
            UPDATE tasks
            SET status = 'todo',
                blocked_at_iteration = NULL,
                skipped_at_iteration = NULL,
                notes = ?,
                updated_at = datetime('now')
            WHERE id = ?
            "#,
            rusqlite::params![new_notes, task_id],
        )?;
    }

    Ok(tasks_to_decay)
}

/// Find tasks that are approaching decay (for doctor command warnings).
///
/// Returns tasks that are within `warning_threshold` iterations of decaying.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `decay_threshold` - The decay threshold in iterations
/// * `warning_threshold` - Number of iterations before decay to start warning
///
/// # Returns
///
/// A vector of `DecayWarning` structs for tasks approaching decay.
pub fn find_decay_warnings(
    conn: &Connection,
    decay_threshold: i64,
    warning_threshold: i64,
) -> TaskMgrResult<Vec<DecayWarning>> {
    if decay_threshold <= 0 {
        return Ok(Vec::new());
    }

    // Get the current global iteration
    let current_iteration: i64 = conn
        .query_row(
            "SELECT iteration_counter FROM global_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // Find tasks approaching decay (but not yet decayed)
    let mut stmt = conn.prepare(
        r#"
        SELECT id, title, status, blocked_at_iteration, skipped_at_iteration
        FROM tasks
        WHERE (
            (status = 'blocked' AND blocked_at_iteration IS NOT NULL
             AND (?1 - blocked_at_iteration) >= (?2 - ?3)
             AND (?1 - blocked_at_iteration) < ?2)
            OR
            (status = 'skipped' AND skipped_at_iteration IS NOT NULL
             AND (?1 - skipped_at_iteration) >= (?2 - ?3)
             AND (?1 - skipped_at_iteration) < ?2)
        )
        ORDER BY id
        "#,
    )?;

    let warnings: Vec<DecayWarning> = stmt
        .query_map(
            [current_iteration, decay_threshold, warning_threshold],
            |row| {
                let status: String = row.get(2)?;
                let blocked_at: Option<i64> = row.get(3)?;
                let skipped_at: Option<i64> = row.get(4)?;

                let at_iteration = match status.as_str() {
                    "blocked" => blocked_at.unwrap_or(0),
                    "skipped" => skipped_at.unwrap_or(0),
                    _ => 0,
                };

                let iterations_since = current_iteration - at_iteration;
                let iterations_until_decay = decay_threshold - iterations_since;

                Ok(DecayWarning {
                    task_id: row.get(0)?,
                    task_title: row.get(1)?,
                    status,
                    at_iteration,
                    iterations_since,
                    iterations_until_decay,
                })
            },
        )?
        .filter_map(|r| r.ok())
        .collect();

    Ok(warnings)
}

/// Warning about a task approaching decay.
#[derive(Debug, Clone)]
pub struct DecayWarning {
    /// Task ID
    pub task_id: String,
    /// Task title
    pub task_title: String,
    /// Current status (blocked or skipped)
    pub status: String,
    /// Iteration when the task was blocked/skipped
    pub at_iteration: i64,
    /// How many iterations since it was blocked/skipped
    pub iterations_since: i64,
    /// How many iterations until it will decay
    pub iterations_until_decay: i64,
}
