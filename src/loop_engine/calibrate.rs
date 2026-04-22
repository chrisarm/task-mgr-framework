/// Adaptive selection weight calibration.
///
/// After each completed run, analyzes historical task outcomes against scoring
/// dimensions (file_overlap, synergy, priority). Stores updated weights in the
/// `global_state` table. Weights are bounded at 0.5x-2.0x of default constants
/// to prevent runaway calibration from noisy data.
///
/// Minimum 10 completed tasks required before calibration adjusts weights.
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::TaskMgrResult;
use crate::commands::next::selection::{
    CONFLICT_PENALTY, FILE_OVERLAP_SCORE, PRIORITY_BASE, SYNERGY_BONUS,
};
use crate::db::prefix::prefix_and;
use crate::loop_engine::calibrate_math::{
    adjust_weight, clamp_negative_weight, clamp_weight, compute_correlation,
};

/// Minimum number of completed tasks before calibration adjusts weights.
const MIN_TASKS_FOR_CALIBRATION: usize = 10;

/// Dynamic selection weights, adjustable by calibration.
///
/// Each weight corresponds to a scoring dimension in the task selector.
/// Defaults match the hardcoded constants in `selection.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelectionWeights {
    pub file_overlap: i32,
    pub synergy: i32,
    pub conflict: i32,
    pub priority_base: i32,
}

impl Default for SelectionWeights {
    fn default() -> Self {
        Self {
            file_overlap: FILE_OVERLAP_SCORE,
            synergy: SYNERGY_BONUS,
            conflict: CONFLICT_PENALTY,
            priority_base: PRIORITY_BASE,
        }
    }
}

impl SelectionWeights {
    /// Clamp all weights to within [0.5x, 2.0x] of their default values.
    pub fn clamp_to_bounds(&mut self) {
        let defaults = SelectionWeights::default();

        self.file_overlap = clamp_weight(self.file_overlap, defaults.file_overlap);
        self.synergy = clamp_weight(self.synergy, defaults.synergy);
        self.conflict = clamp_negative_weight(self.conflict, defaults.conflict);
        self.priority_base = clamp_weight(self.priority_base, defaults.priority_base);
    }
}

/// Load dynamic selection weights from the global_state table.
///
/// Falls back to default weights if:
/// - No weights stored yet
/// - Stored JSON is malformed
/// - Database error occurs
pub fn load_dynamic_weights(conn: &Connection) -> SelectionWeights {
    match load_weights_from_db(conn) {
        Ok(Some(weights)) => weights,
        Ok(None) => SelectionWeights::default(),
        Err(e) => {
            eprintln!(
                "Warning: failed to load dynamic weights, using defaults: {}",
                e
            );
            SelectionWeights::default()
        }
    }
}

/// Internal: load weights JSON from global_state table.
fn load_weights_from_db(conn: &Connection) -> TaskMgrResult<Option<SelectionWeights>> {
    // Check if a JSON value column exists - for now use last_task_id as storage
    // A proper implementation would add a dedicated column or key-value table
    // For TDD, define the contract here; FEAT-024 will implement storage properly
    let result: Result<String, _> = conn.query_row(
        "SELECT last_task_id FROM global_state WHERE id = 1",
        [],
        |row| row.get(0),
    );

    match result {
        Ok(json_str) => {
            if json_str.starts_with('{') {
                match serde_json::from_str::<SelectionWeights>(&json_str) {
                    Ok(mut weights) => {
                        weights.clamp_to_bounds();
                        Ok(Some(weights))
                    }
                    Err(_) => Ok(None), // Malformed JSON, fall back to defaults
                }
            } else {
                Ok(None) // Not JSON (it's a task ID), fall back to defaults
            }
        }
        Err(_) => Ok(None), // No row or NULL, fall back to defaults
    }
}

/// Recalibrate selection weights based on historical task outcomes.
///
/// Analyzes completed tasks with score breakdown data to find correlations
/// between scoring dimensions and task success. Requires at least
/// `MIN_TASKS_FOR_CALIBRATION` (10) completed tasks.
///
/// Called after `run::end()` with Completed status.
pub fn recalibrate_weights(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<SelectionWeights> {
    let completed_count = count_completed_tasks(conn, task_prefix)?;

    if completed_count < MIN_TASKS_FOR_CALIBRATION {
        return Ok(SelectionWeights::default());
    }

    // Analyze correlations and compute adjusted weights
    let weights = compute_calibrated_weights(conn, task_prefix)?;

    // Store updated weights
    store_weights(conn, &weights)?;

    Ok(weights)
}

/// Count completed tasks that have score breakdown data.
fn count_completed_tasks(conn: &Connection, task_prefix: Option<&str>) -> TaskMgrResult<usize> {
    let (prefix_clause, prefix_param) = prefix_and(task_prefix);
    let sql = format!(
        "SELECT COUNT(*) FROM tasks WHERE status = 'done' AND archived_at IS NULL {prefix_clause}"
    );
    let count: i64 = match prefix_param {
        Some(ref p) => conn.query_row(&sql, rusqlite::params![p], |row| row.get(0))?,
        None => conn.query_row(&sql, [], |row| row.get(0))?,
    };
    Ok(count as usize)
}

/// A task's historical outcome paired with its scoring dimension values.
pub(crate) struct TaskOutcome {
    /// true if the task completed on its first attempt in any run
    pub(crate) first_try_success: bool,
    /// Number of files this task touches that overlapped with prior iteration files
    pub(crate) file_overlap_count: f64,
    /// Number of synergy relationships to completed tasks
    pub(crate) synergy_count: f64,
    /// Number of conflict relationships to completed tasks
    pub(crate) conflict_count: f64,
}

/// Compute calibrated weights from historical data.
///
/// Simple correlation analysis: for each scoring dimension, compute whether
/// tasks with higher dimension scores tended to complete on the first try
/// (vs requiring retries or failing). Uses point-biserial correlation
/// (mean difference of dimension values between success/failure groups).
fn compute_calibrated_weights(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<SelectionWeights> {
    let outcomes = load_task_outcomes(conn, task_prefix)?;

    if outcomes.is_empty() {
        return Ok(SelectionWeights::default());
    }

    let defaults = SelectionWeights::default();

    // Compute mean dimension values for success vs failure groups
    let file_corr = compute_correlation(&outcomes, |o| o.file_overlap_count);
    let synergy_corr = compute_correlation(&outcomes, |o| o.synergy_count);
    let conflict_corr = compute_correlation(&outcomes, |o| o.conflict_count);

    // Adjust weights: scale default by (1 + correlation * ADJUSTMENT_FACTOR)
    // Positive correlation → increase weight; negative → decrease
    const ADJUSTMENT_FACTOR: f64 = 0.5;

    let mut weights = SelectionWeights {
        file_overlap: adjust_weight(defaults.file_overlap, file_corr, ADJUSTMENT_FACTOR),
        synergy: adjust_weight(defaults.synergy, synergy_corr, ADJUSTMENT_FACTOR),
        conflict: adjust_weight(defaults.conflict, conflict_corr, ADJUSTMENT_FACTOR),
        priority_base: defaults.priority_base, // Priority base is not calibrated
    };

    weights.clamp_to_bounds();
    Ok(weights)
}

/// Load historical task outcomes from the run_tasks table.
///
/// For each unique task that appears in run_tasks, determines:
/// - Whether it completed on its first attempt (first_try_success)
/// - Its scoring dimension values (file overlap, synergy, conflict counts)
fn load_task_outcomes(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<Vec<TaskOutcome>> {
    // Get task attempt history: for each task, count attempts and check if first was successful
    let (prefix_clause, prefix_param) = match task_prefix {
        Some(p) => {
            let pattern = format!("{}-%", crate::db::prefix::escape_like(p));
            (
                "WHERE archived_at IS NULL AND task_id LIKE ? ESCAPE '\\'".to_string(),
                Some(pattern),
            )
        }
        None => ("WHERE archived_at IS NULL".to_string(), None),
    };
    let sql = format!(
        "SELECT task_id, status, iteration \
         FROM run_tasks \
         {prefix_clause} \
         ORDER BY task_id, iteration ASC"
    );
    let mut stmt = conn.prepare(&sql)?;

    let rows: Result<Vec<(String, String, i64)>, rusqlite::Error> = match prefix_param {
        Some(ref p) => stmt
            .query_map(rusqlite::params![p], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect(),
        None => stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect(),
    };
    let rows = rows?;

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // Group by task_id: determine first-try success
    let mut task_first_try: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    for (task_id, status, _iteration) in &rows {
        // Only record first attempt per task
        task_first_try
            .entry(task_id.clone())
            .or_insert(status == "completed");
    }

    // Load file counts per task
    let file_counts = get_task_file_counts(conn, task_prefix)?;

    // Load synergy/conflict counts per task
    let synergy_counts = get_relationship_counts(conn, "synergyWith", task_prefix)?;
    let conflict_counts = get_relationship_counts(conn, "conflictsWith", task_prefix)?;

    // Build outcome records
    let outcomes: Vec<TaskOutcome> = task_first_try
        .into_iter()
        .map(|(task_id, first_try_success)| TaskOutcome {
            first_try_success,
            file_overlap_count: *file_counts.get(&task_id).unwrap_or(&0) as f64,
            synergy_count: *synergy_counts.get(&task_id).unwrap_or(&0) as f64,
            conflict_count: *conflict_counts.get(&task_id).unwrap_or(&0) as f64,
        })
        .collect();

    Ok(outcomes)
}

/// Get the number of files each task touches.
fn get_task_file_counts(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<std::collections::HashMap<String, i32>> {
    // task_files uses task_id column, not id — build clause manually
    let (prefix_clause, prefix_param) = match task_prefix {
        Some(p) => {
            let pattern = format!("{}-%", crate::db::prefix::escape_like(p));
            (
                "WHERE task_id LIKE ? ESCAPE '\\'".to_string(),
                Some(pattern),
            )
        }
        None => (String::new(), None),
    };
    let sql = format!("SELECT task_id, COUNT(*) FROM task_files {prefix_clause} GROUP BY task_id");
    let mut stmt = conn.prepare(&sql)?;
    let rows: Result<Vec<(String, i32)>, rusqlite::Error> = match prefix_param {
        Some(ref p) => stmt
            .query_map(rusqlite::params![p], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect(),
        None => stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect(),
    };
    Ok(rows?.into_iter().collect())
}

/// Get the count of relationships of a given type per task, optionally filtered by prefix.
fn get_relationship_counts(
    conn: &Connection,
    rel_type: &str,
    task_prefix: Option<&str>,
) -> TaskMgrResult<std::collections::HashMap<String, i32>> {
    let (prefix_clause, prefix_param) = match task_prefix {
        Some(p) => {
            let pattern = format!("{}-%", crate::db::prefix::escape_like(p));
            ("AND task_id LIKE ? ESCAPE '\\'".to_string(), Some(pattern))
        }
        None => (String::new(), None),
    };
    let sql = format!(
        "SELECT task_id, COUNT(*) FROM task_relationships WHERE rel_type = ? {prefix_clause} GROUP BY task_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Result<Vec<(String, i32)>, rusqlite::Error> = match prefix_param {
        Some(ref p) => stmt
            .query_map(rusqlite::params![rel_type, p], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect(),
        None => stmt
            .query_map([rel_type], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect(),
    };
    Ok(rows?.into_iter().collect())
}

/// Store weights as JSON in the global_state table.
fn store_weights(conn: &Connection, weights: &SelectionWeights) -> TaskMgrResult<()> {
    let json = serde_json::to_string(weights)
        .map_err(|e| crate::TaskMgrError::IoError(std::io::Error::other(e)))?;

    conn.execute(
        "UPDATE global_state SET last_task_id = ?1, updated_at = datetime('now') WHERE id = 1",
        [&json],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::test_utils::{insert_done_task, setup_test_db};
    use rusqlite::params;

    // --- AC: With no historical data, returns default weights unchanged ---

    #[test]
    fn test_no_historical_data_returns_defaults() {
        let (_temp_dir, conn) = setup_test_db();

        let weights = recalibrate_weights(&conn, None).unwrap();
        let defaults = SelectionWeights::default();
        assert_eq!(weights, defaults, "Should return defaults with no data");
    }

    #[test]
    fn test_default_weights_match_constants() {
        let defaults = SelectionWeights::default();
        assert_eq!(defaults.file_overlap, FILE_OVERLAP_SCORE);
        assert_eq!(defaults.synergy, SYNERGY_BONUS);
        assert_eq!(defaults.conflict, CONFLICT_PENALTY);
        assert_eq!(defaults.priority_base, PRIORITY_BASE);
    }

    // --- AC: Weights stay bounded within 0.5x-2.0x of defaults ---
    // (clamp_weight/clamp_negative_weight unit tests are in calibrate_math.rs)

    #[test]
    fn test_selection_weights_clamp_to_bounds() {
        let mut weights = SelectionWeights {
            file_overlap: 100,  // Way above 2.0x of 10
            synergy: 0,         // Below 0.5x of 3
            conflict: -100,     // Way below 2.0x of -5
            priority_base: 500, // Below 0.5x of 1000
        };
        weights.clamp_to_bounds();

        assert_eq!(weights.file_overlap, 20, "file_overlap clamped to upper");
        assert_eq!(
            weights.synergy, 1,
            "synergy clamped to lower (0.5 * 3 = 1.5 -> 1)"
        );
        assert_eq!(
            weights.conflict, -10,
            "conflict clamped to lower (more neg)"
        );
        assert_eq!(weights.priority_base, 500, "priority_base within bounds");
    }

    // --- AC: Minimum 10 completed tasks before calibration ---

    #[test]
    fn test_below_threshold_returns_defaults() {
        let (_temp_dir, conn) = setup_test_db();

        // Insert 9 tasks (below threshold)
        for i in 0..9 {
            insert_done_task(&conn, &format!("TASK-{:03}", i));
        }

        let weights = recalibrate_weights(&conn, None).unwrap();
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "Below threshold should return defaults"
        );
    }

    #[test]
    fn test_at_threshold_allows_calibration() {
        let (_temp_dir, conn) = setup_test_db();

        // Insert exactly 10 tasks (at threshold)
        for i in 0..10 {
            insert_done_task(&conn, &format!("TASK-{:03}", i));
        }

        let weights = recalibrate_weights(&conn, None).unwrap();
        // Currently returns defaults since compute_calibrated_weights is a placeholder,
        // but should NOT early-return with "below threshold" message
        assert_eq!(weights.file_overlap, FILE_OVERLAP_SCORE);
    }

    // --- AC: load_dynamic_weights fallback behavior ---

    #[test]
    fn test_load_dynamic_weights_no_stored_weights() {
        let (_temp_dir, conn) = setup_test_db();

        let weights = load_dynamic_weights(&conn);
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "Should return defaults when no weights stored"
        );
    }

    #[test]
    fn test_load_dynamic_weights_with_valid_json() {
        let (_temp_dir, conn) = setup_test_db();

        // Store valid weights JSON
        let weights = SelectionWeights {
            file_overlap: 15,
            synergy: 4,
            conflict: -7,
            priority_base: 1200,
        };
        store_weights(&conn, &weights).unwrap();

        let loaded = load_dynamic_weights(&conn);
        assert_eq!(loaded.file_overlap, 15);
        assert_eq!(loaded.synergy, 4);
        assert_eq!(loaded.conflict, -7);
        assert_eq!(loaded.priority_base, 1200);
    }

    #[test]
    fn test_load_dynamic_weights_corrupted_json_falls_back() {
        let (_temp_dir, conn) = setup_test_db();

        // Store corrupted JSON
        conn.execute(
            "UPDATE global_state SET last_task_id = '{invalid json!!!}' WHERE id = 1",
            [],
        )
        .unwrap();

        let weights = load_dynamic_weights(&conn);
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "Corrupted JSON should fall back to defaults"
        );
    }

    #[test]
    fn test_load_dynamic_weights_non_json_value_falls_back() {
        let (_temp_dir, conn) = setup_test_db();

        // Store a regular task ID (not JSON)
        conn.execute(
            "UPDATE global_state SET last_task_id = 'FEAT-001' WHERE id = 1",
            [],
        )
        .unwrap();

        let weights = load_dynamic_weights(&conn);
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "Non-JSON value should fall back to defaults"
        );
    }

    #[test]
    fn test_load_dynamic_weights_clamps_out_of_bounds() {
        let (_temp_dir, conn) = setup_test_db();

        // Store weights that exceed bounds
        let json = r#"{"file_overlap":100,"synergy":100,"conflict":-100,"priority_base":5000}"#;
        conn.execute(
            "UPDATE global_state SET last_task_id = ?1 WHERE id = 1",
            [json],
        )
        .unwrap();

        let weights = load_dynamic_weights(&conn);
        assert_eq!(
            weights.file_overlap, 20,
            "Should clamp file_overlap to 2.0x"
        );
        assert_eq!(weights.synergy, 6, "Should clamp synergy to 2.0x");
        assert_eq!(
            weights.conflict, -10,
            "Should clamp conflict to 2.0x (more negative)"
        );
        assert_eq!(
            weights.priority_base, 2000,
            "Should clamp priority_base to 2.0x"
        );
    }

    // --- Store and round-trip ---

    #[test]
    fn test_store_and_load_round_trip() {
        let (_temp_dir, conn) = setup_test_db();

        let original = SelectionWeights {
            file_overlap: 12,
            synergy: 5,
            conflict: -8,
            priority_base: 1100,
        };
        store_weights(&conn, &original).unwrap();

        let loaded = load_dynamic_weights(&conn);
        assert_eq!(loaded, original, "Round-trip should preserve weights");
    }

    // --- Serialization ---

    #[test]
    fn test_selection_weights_serialize_deserialize() {
        let weights = SelectionWeights::default();
        let json = serde_json::to_string(&weights).unwrap();
        let deserialized: SelectionWeights = serde_json::from_str(&json).unwrap();
        assert_eq!(weights, deserialized);
    }

    // (Correlation and adjust_weight unit tests are in calibrate_math.rs)

    // --- AC: Positive correlation increases file_overlap weight ---

    #[test]
    fn test_calibration_with_positive_file_correlation() {
        let (_temp_dir, conn) = setup_test_db();

        // Insert 10 done tasks (meet threshold)
        for i in 0..10 {
            let id = format!("TASK-{:03}", i);
            insert_done_task(&conn, &id);
        }

        // Create a run
        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        // Create run_tasks where tasks with more files succeed first try
        // Tasks 0-4: succeed (completed), have many files
        // Tasks 5-9: fail (failed), have few files
        for i in 0..5 {
            let id = format!("TASK-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'completed', ?)",
                params![id, i],
            )
            .unwrap();
            // Add multiple files for successful tasks
            for f in 0..3 {
                conn.execute(
                    "INSERT OR IGNORE INTO task_files (task_id, file_path) VALUES (?, ?)",
                    params![id, format!("src/file_{}.rs", f)],
                )
                .unwrap();
            }
        }
        for i in 5..10 {
            let id = format!("TASK-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'failed', ?)",
                params![id, i],
            )
            .unwrap();
            // Add only 1 file for failed tasks
            conn.execute(
                "INSERT OR IGNORE INTO task_files (task_id, file_path) VALUES (?, ?)",
                params![id, "src/single.rs"],
            )
            .unwrap();
        }

        let weights = recalibrate_weights(&conn, None).unwrap();

        // File overlap weight should increase because tasks with more files succeeded
        assert!(
            weights.file_overlap > FILE_OVERLAP_SCORE,
            "file_overlap should increase with positive correlation: got {}",
            weights.file_overlap
        );
    }

    // --- Load task outcomes tests ---

    #[test]
    fn test_load_task_outcomes_empty() {
        let (_temp_dir, conn) = setup_test_db();
        let outcomes = load_task_outcomes(&conn, None).unwrap();
        assert!(outcomes.is_empty());
    }

    #[test]
    fn test_load_task_outcomes_first_try_detection() {
        let (_temp_dir, conn) = setup_test_db();

        // Insert tasks
        insert_done_task(&conn, "T-001");
        insert_done_task(&conn, "T-002");

        // Create run
        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        // T-001: failed first, then completed → first_try_success = false
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', 'T-001', 'failed', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', 'T-001', 'completed', 2)",
            [],
        )
        .unwrap();

        // T-002: completed first try → first_try_success = true
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', 'T-002', 'completed', 1)",
            [],
        )
        .unwrap();

        let outcomes = load_task_outcomes(&conn, None).unwrap();
        assert_eq!(outcomes.len(), 2);

        // Find each outcome
        let t001 = outcomes.iter().find(|o| !o.first_try_success);
        let t002 = outcomes.iter().find(|o| o.first_try_success);

        assert!(t001.is_some(), "T-001 should be first_try_success=false");
        assert!(t002.is_some(), "T-002 should be first_try_success=true");
    }

    // --- Calibration with no run_tasks data ---

    #[test]
    fn test_calibration_no_run_tasks_returns_defaults() {
        let (_temp_dir, conn) = setup_test_db();

        // Insert 10 done tasks (meet threshold) but NO run_tasks
        for i in 0..10 {
            insert_done_task(&conn, &format!("TASK-{:03}", i));
        }

        let weights = recalibrate_weights(&conn, None).unwrap();
        // No run_tasks → compute_calibrated_weights returns defaults
        assert_eq!(weights, SelectionWeights::default());
    }

    // === Comprehensive tests (TEST-005) ===

    // --- AC: Calibration with conflicting signals ---

    #[test]
    fn test_calibration_conflicting_signals() {
        let (_temp_dir, conn) = setup_test_db();

        for i in 0..12 {
            insert_done_task(&conn, &format!("TASK-{:03}", i));
        }

        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        // Tasks 0-5: succeed, high file count, LOW synergy
        for i in 0..6 {
            let id = format!("TASK-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'completed', ?)",
                params![id, i],
            )
            .unwrap();
            // Many files
            for f in 0..4 {
                conn.execute(
                    "INSERT OR IGNORE INTO task_files (task_id, file_path) VALUES (?, ?)",
                    params![id, format!("src/file_{}.rs", f)],
                )
                .unwrap();
            }
            // No synergy relationships (low synergy)
        }

        // Tasks 6-11: fail, LOW file count, HIGH synergy
        for i in 6..12 {
            let id = format!("TASK-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'failed', ?)",
                params![id, i],
            )
            .unwrap();
            // 1 file only
            conn.execute(
                "INSERT OR IGNORE INTO task_files (task_id, file_path) VALUES (?, ?)",
                params![id, "src/single.rs"],
            )
            .unwrap();
            // Many synergy relationships
            for j in 6..12 {
                if j != i {
                    conn.execute(
                        "INSERT OR IGNORE INTO task_relationships (task_id, related_id, rel_type) VALUES (?, ?, 'synergyWith')",
                        params![id, format!("TASK-{:03}", j)],
                    )
                    .unwrap();
                }
            }
        }

        let weights = recalibrate_weights(&conn, None).unwrap();

        // file_overlap should INCREASE (successful tasks had more files)
        assert!(
            weights.file_overlap >= FILE_OVERLAP_SCORE,
            "file_overlap should increase or stay: got {}",
            weights.file_overlap
        );
        // synergy should DECREASE (failing tasks had more synergy)
        assert!(
            weights.synergy <= SYNERGY_BONUS,
            "synergy should decrease or stay: got {}",
            weights.synergy
        );
    }

    // --- AC: All tasks succeed (all same group → zero correlation → defaults) ---

    #[test]
    fn test_all_tasks_succeed_returns_near_defaults() {
        let (_temp_dir, conn) = setup_test_db();

        for i in 0..12 {
            insert_done_task(&conn, &format!("TASK-{:03}", i));
        }

        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        // All tasks succeed on first try → all in success group → no failure group → zero correlation
        for i in 0..12 {
            let id = format!("TASK-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'completed', ?)",
                params![id, i],
            )
            .unwrap();
        }

        let weights = recalibrate_weights(&conn, None).unwrap();
        // Zero correlation → adjustment factor is 0 → weights equal defaults
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "All same group should produce default weights"
        );
    }

    // --- AC: All tasks fail (all same group → zero correlation → defaults) ---

    #[test]
    fn test_all_tasks_fail_returns_near_defaults() {
        let (_temp_dir, conn) = setup_test_db();

        for i in 0..12 {
            insert_done_task(&conn, &format!("TASK-{:03}", i));
        }

        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        for i in 0..12 {
            let id = format!("TASK-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'failed', ?)",
                params![id, i],
            )
            .unwrap();
        }

        let weights = recalibrate_weights(&conn, None).unwrap();
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "All same group should produce default weights"
        );
    }

    // (clamp bound edge case tests are in calibrate_math.rs)

    // --- AC: Large number of tasks (50+) ---

    #[test]
    fn test_calibration_large_dataset() {
        let (_temp_dir, conn) = setup_test_db();

        for i in 0..50 {
            insert_done_task(&conn, &format!("TASK-{:03}", i));
        }

        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        // 25 succeed (high file count), 25 fail (low file count)
        for i in 0..25 {
            let id = format!("TASK-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'completed', ?)",
                params![id, i],
            )
            .unwrap();
            for f in 0..5 {
                conn.execute(
                    "INSERT OR IGNORE INTO task_files (task_id, file_path) VALUES (?, ?)",
                    params![id, format!("src/f_{}.rs", f)],
                )
                .unwrap();
            }
        }
        for i in 25..50 {
            let id = format!("TASK-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'failed', ?)",
                params![id, i],
            )
            .unwrap();
        }

        let weights = recalibrate_weights(&conn, None).unwrap();

        // With strong positive correlation for file_overlap:
        assert!(
            weights.file_overlap > FILE_OVERLAP_SCORE,
            "Large dataset with clear signal should increase file_overlap: {}",
            weights.file_overlap
        );
    }

    // --- AC: Multiple runs with same task appearing in different states ---

    #[test]
    fn test_multiple_runs_first_attempt_counts() {
        let (_temp_dir, conn) = setup_test_db();

        for i in 0..10 {
            insert_done_task(&conn, &format!("TASK-{:03}", i));
        }

        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-2', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        // Run 1: TASK-000 fails at iteration 1
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', 'TASK-000', 'failed', 1)",
            [],
        )
        .unwrap();
        // Run 2: TASK-000 succeeds at iteration 1
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-2', 'TASK-000', 'completed', 1)",
            [],
        )
        .unwrap();

        let outcomes = load_task_outcomes(&conn, None).unwrap();
        // TASK-000 should be first_try_success=false because its first appearance
        // (ordered by iteration ASC) was "failed"
        let task_outcome = outcomes.iter().find(|o| !o.first_try_success);
        assert!(
            task_outcome.is_some(),
            "First attempt was failed, so first_try_success should be false"
        );
    }

    // --- AC: Weight persistence across recalibrations ---

    #[test]
    fn test_recalibration_overwrites_previous_weights() {
        let (_temp_dir, conn) = setup_test_db();

        // Store initial weights
        let initial = SelectionWeights {
            file_overlap: 15,
            synergy: 5,
            conflict: -8,
            priority_base: 1100,
        };
        store_weights(&conn, &initial).unwrap();

        // Verify stored
        let loaded = load_dynamic_weights(&conn);
        assert_eq!(loaded, initial);

        // Store different weights
        let updated = SelectionWeights {
            file_overlap: 8,
            synergy: 2,
            conflict: -3,
            priority_base: 900,
        };
        store_weights(&conn, &updated).unwrap();

        // Verify overwritten
        let loaded = load_dynamic_weights(&conn);
        assert_eq!(loaded, updated, "Should have overwritten previous weights");
    }

    // --- AC: Partial JSON (missing fields) fallback ---

    #[test]
    fn test_load_partial_json_falls_back_to_defaults() {
        let (_temp_dir, conn) = setup_test_db();

        // Store JSON with missing fields
        let partial_json = r#"{"file_overlap":15}"#;
        conn.execute(
            "UPDATE global_state SET last_task_id = ?1 WHERE id = 1",
            [partial_json],
        )
        .unwrap();

        let weights = load_dynamic_weights(&conn);
        // Missing fields should cause deserialization to fail → fall back to defaults
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "Partial JSON should fall back to defaults"
        );
    }

    // --- AC: JSON with extra fields still parses (forward compatibility) ---

    #[test]
    fn test_load_json_with_extra_fields() {
        let (_temp_dir, conn) = setup_test_db();

        // serde(deny_unknown_fields) is not set, so extra fields should be ignored
        let json_with_extras = r#"{"file_overlap":12,"synergy":4,"conflict":-6,"priority_base":1050,"extra_field":"ignored"}"#;
        conn.execute(
            "UPDATE global_state SET last_task_id = ?1 WHERE id = 1",
            [json_with_extras],
        )
        .unwrap();

        let weights = load_dynamic_weights(&conn);
        assert_eq!(weights.file_overlap, 12);
        assert_eq!(weights.synergy, 4);
        assert_eq!(weights.conflict, -6);
        assert_eq!(weights.priority_base, 1050);
    }

    // (Correlation edge case and adjust_weight max tests are in calibrate_math.rs)

    // --- AC: Exactly 9 tasks (below threshold) ---

    #[test]
    fn test_exactly_nine_tasks_returns_defaults() {
        let (_temp_dir, conn) = setup_test_db();
        for i in 0..9 {
            insert_done_task(&conn, &format!("T-{:03}", i));
        }

        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();
        for i in 0..9 {
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'completed', ?)",
                params![format!("T-{:03}", i), i],
            )
            .unwrap();
        }

        let weights = recalibrate_weights(&conn, None).unwrap();
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "9 tasks (below 10 threshold) should return defaults"
        );
    }

    // --- AC: Exactly 10 tasks with run_tasks (at threshold, calibration runs) ---

    #[test]
    fn test_exactly_ten_tasks_with_data_calibrates() {
        let (_temp_dir, conn) = setup_test_db();
        for i in 0..10 {
            insert_done_task(&conn, &format!("T-{:03}", i));
        }

        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        // 5 succeed with files, 5 fail without
        for i in 0..5 {
            let id = format!("T-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'completed', ?)",
                params![id, i],
            )
            .unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO task_files (task_id, file_path) VALUES (?, 'src/a.rs')",
                params![id],
            )
            .unwrap();
        }
        for i in 5..10 {
            let id = format!("T-{:03}", i);
            conn.execute(
                "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', ?, 'failed', ?)",
                params![id, i],
            )
            .unwrap();
        }

        let weights = recalibrate_weights(&conn, None).unwrap();
        // Should have actually calibrated (not returned defaults due to threshold)
        // file_overlap should be >= default since successful tasks had files
        assert!(
            weights.file_overlap >= FILE_OVERLAP_SCORE,
            "At threshold with data should calibrate: {}",
            weights.file_overlap
        );
    }

    // --- AC: Empty string in global_state ---

    #[test]
    fn test_load_empty_string_falls_back() {
        let (_temp_dir, conn) = setup_test_db();
        conn.execute("UPDATE global_state SET last_task_id = '' WHERE id = 1", [])
            .unwrap();

        let weights = load_dynamic_weights(&conn);
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "Empty string should fall back to defaults"
        );
    }

    // --- AC: NULL in global_state ---

    #[test]
    fn test_load_null_falls_back() {
        let (_temp_dir, conn) = setup_test_db();
        conn.execute(
            "UPDATE global_state SET last_task_id = NULL WHERE id = 1",
            [],
        )
        .unwrap();

        let weights = load_dynamic_weights(&conn);
        assert_eq!(
            weights,
            SelectionWeights::default(),
            "NULL should fall back to defaults"
        );
    }
}
