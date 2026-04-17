//! UCB (Upper Confidence Bound) bandit ranking for learnings.
//!
//! This module implements a sliding-window UCB algorithm for ranking learnings,
//! balancing exploitation of proven learnings with exploration of new ones.
//!
//! ## Algorithm Overview
//!
//! The UCB score combines two components:
//!
//! 1. **Exploitation**: How effective has this learning been when shown?
//!    - Formula: `(window_applied / window_shown) * confidence_weight`
//!    - Confidence weights: high=1.0, medium=0.7, low=0.4
//!
//! 2. **Exploration**: Bonus for less-shown learnings to encourage trying them.
//!    - Formula: `C * sqrt(ln(total_window_shows) / window_shown)` where C ≈ 1.41
//!    - New learnings (< 3 shows) get a fixed high bonus of 2.0
//!
//! ## Sliding Window
//!
//! To adapt to changing relevance over time, statistics are tracked within a
//! sliding window of 720 iterations. When a learning's window expires, its
//! window stats are reset, giving it fresh exploration potential.
//!
//! ## Usage
//!
//! ```text
//! // When showing learnings to an agent:
//! record_learning_shown(&conn, learning_id, current_iteration)?;
//!
//! // When agent confirms learning was useful:
//! record_learning_applied(&conn, learning_id)?;
//!
//! // To get UCB score for ranking:
//! let score = calculate_ucb_score(&learning, total_window_shows);
//! ```

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::models::{Confidence, Learning};

/// Size of the sliding window in iterations.
/// Stats are reset after 720 iterations to allow learnings to regain exploration potential.
pub const WINDOW_SIZE: i64 = 720;

/// Exploration constant (sqrt(2)) for the UCB formula.
const EXPLORATION_CONSTANT: f64 = std::f64::consts::SQRT_2;

/// Exploration bonus given to learnings with fewer than MIN_SHOWS_FOR_UCB shows.
/// This encourages trying new learnings before the UCB formula kicks in.
const NEW_LEARNING_BONUS: f64 = 2.0;

/// Minimum shows before normal UCB calculation applies.
/// Below this threshold, learnings get a fixed exploration bonus.
const MIN_SHOWS_FOR_UCB: i32 = 3;

/// Confidence weight multipliers for the exploitation term.
fn confidence_weight(confidence: Confidence) -> f64 {
    match confidence {
        Confidence::High => 1.0,
        Confidence::Medium => 0.7,
        Confidence::Low => 0.4,
    }
}

/// Statistics from the sliding window for a learning.
#[derive(Debug, Clone, Default)]
pub struct WindowStats {
    /// Times shown within the current window
    pub window_shown: i32,
    /// Times applied within the current window
    pub window_applied: i32,
    /// Iteration when this window started
    pub window_start_iteration: i64,
}

impl WindowStats {
    /// Check if the window has expired relative to the current iteration.
    pub fn is_expired(&self, current_iteration: i64) -> bool {
        current_iteration - self.window_start_iteration >= WINDOW_SIZE
    }
}

/// Calculates the UCB score for a learning.
///
/// # Arguments
///
/// * `window_stats` - The learning's sliding window statistics
/// * `confidence` - The learning's confidence level
/// * `total_window_shows` - Sum of window_shown across all learnings (for exploration term)
///
/// # Returns
///
/// The UCB score as a float. Higher scores should be ranked higher.
///
/// # Algorithm
///
/// For learnings with < 3 window shows:
///   score = NEW_LEARNING_BONUS (2.0) to encourage exploration
///
/// For learnings with >= 3 window shows:
///   exploitation = (window_applied / window_shown) * confidence_weight
///   exploration = C * sqrt(ln(total_window_shows) / window_shown)
///   score = exploitation + exploration
#[must_use]
pub fn calculate_ucb_score(
    window_stats: &WindowStats,
    confidence: Confidence,
    total_window_shows: i64,
) -> f64 {
    // Handle new learnings with high exploration bonus
    if window_stats.window_shown < MIN_SHOWS_FOR_UCB {
        return NEW_LEARNING_BONUS;
    }

    let window_shown = f64::from(window_stats.window_shown);
    let window_applied = f64::from(window_stats.window_applied);

    // Exploitation: effectiveness rate weighted by confidence
    let application_rate = window_applied / window_shown;
    let exploitation = application_rate * confidence_weight(confidence);

    // Exploration: UCB exploration bonus
    // Avoid log(0) by ensuring total_window_shows >= 1
    let total_shows = (total_window_shows.max(1)) as f64;
    let exploration = EXPLORATION_CONSTANT * (total_shows.ln() / window_shown).sqrt();

    exploitation + exploration
}

/// Refreshes the sliding window for a learning if it has expired.
///
/// If the learning's window has expired (current_iteration - window_start >= WINDOW_SIZE),
/// resets window_shown, window_applied, and sets window_start_iteration to current_iteration.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning to refresh
/// * `current_iteration` - The current global iteration
///
/// # Returns
///
/// True if the window was refreshed, false if still within the window.
pub fn refresh_sliding_window(
    conn: &Connection,
    learning_id: i64,
    current_iteration: i64,
) -> TaskMgrResult<bool> {
    // Check current window state
    let (window_start,): (Option<i64>,) = conn.query_row(
        "SELECT window_start_iteration FROM learnings WHERE id = ?1",
        [learning_id],
        |row| Ok((row.get(0)?,)),
    )?;

    // If window_start is NULL, this is a new learning - initialize the window
    let window_start = window_start.unwrap_or(0);

    if current_iteration - window_start >= WINDOW_SIZE {
        // Window has expired - reset stats
        conn.execute(
            r#"
            UPDATE learnings
            SET window_shown = 0,
                window_applied = 0,
                window_start_iteration = ?1
            WHERE id = ?2
            "#,
            rusqlite::params![current_iteration, learning_id],
        )?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Records that a learning was shown to an agent.
///
/// This updates both the global times_shown/last_shown_at and the
/// window-specific window_shown counter.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning that was shown
/// * `current_iteration` - The current global iteration (for window refresh check)
pub fn record_learning_shown(
    conn: &Connection,
    learning_id: i64,
    current_iteration: i64,
) -> TaskMgrResult<()> {
    // First, refresh the window if expired
    refresh_sliding_window(conn, learning_id, current_iteration)?;

    // Update both global and window stats
    conn.execute(
        r#"
        UPDATE learnings
        SET times_shown = times_shown + 1,
            last_shown_at = datetime('now'),
            window_shown = window_shown + 1,
            window_start_iteration = COALESCE(window_start_iteration, ?1)
        WHERE id = ?2
        "#,
        rusqlite::params![current_iteration, learning_id],
    )?;

    Ok(())
}

/// Records that a learning was applied (marked as useful by the agent).
///
/// This updates both the global times_applied/last_applied_at and the
/// window-specific window_applied counter.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning that was applied
pub fn record_learning_applied(conn: &Connection, learning_id: i64) -> TaskMgrResult<()> {
    conn.execute(
        r#"
        UPDATE learnings
        SET times_applied = times_applied + 1,
            last_applied_at = datetime('now'),
            window_applied = window_applied + 1
        WHERE id = ?1
        "#,
        [learning_id],
    )?;

    Ok(())
}

/// Gets the total window shows across all learnings.
///
/// Used as the denominator in the UCB exploration term.
pub fn get_total_window_shows(conn: &Connection) -> TaskMgrResult<i64> {
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(window_shown), 0) FROM learnings WHERE retired_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(total)
}

/// Gets the window statistics for a learning.
pub fn get_window_stats(conn: &Connection, learning_id: i64) -> TaskMgrResult<WindowStats> {
    let stats = conn.query_row(
        r#"
        SELECT COALESCE(window_shown, 0), COALESCE(window_applied, 0), COALESCE(window_start_iteration, 0)
        FROM learnings
        WHERE id = ?1
        "#,
        [learning_id],
        |row| {
            Ok(WindowStats {
                window_shown: row.get(0)?,
                window_applied: row.get(1)?,
                window_start_iteration: row.get(2)?,
            })
        },
    )?;
    Ok(stats)
}

/// Ranks learnings by UCB score.
///
/// # Arguments
///
/// * `learnings` - The learnings to rank
/// * `conn` - Database connection (for fetching window stats)
///
/// # Returns
///
/// Learnings sorted by UCB score in descending order (highest score first).
pub fn rank_learnings_by_ucb(
    conn: &Connection,
    learnings: Vec<Learning>,
) -> TaskMgrResult<Vec<Learning>> {
    if learnings.is_empty() {
        return Ok(learnings);
    }

    let total_window_shows = get_total_window_shows(conn)?;

    // Compute scores and sort
    let mut scored: Vec<(Learning, f64)> = learnings
        .into_iter()
        .map(|learning| {
            let learning_id = learning.id.unwrap_or(0);
            let stats = get_window_stats(conn, learning_id).unwrap_or_default();
            let score = calculate_ucb_score(&stats, learning.confidence, total_window_shows);
            (learning, score)
        })
        .collect();

    // Sort by score descending
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    Ok(scored.into_iter().map(|(l, _)| l).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::{create_schema, open_connection};
    use crate::learnings::crud::{RecordLearningParams, record_learning};
    use crate::models::LearningOutcome;
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        // Run migrations to add UCB columns
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    #[test]
    fn test_confidence_weights() {
        assert!((confidence_weight(Confidence::High) - 1.0).abs() < f64::EPSILON);
        assert!((confidence_weight(Confidence::Medium) - 0.7).abs() < f64::EPSILON);
        assert!((confidence_weight(Confidence::Low) - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ucb_score_new_learning() {
        // New learnings with < 3 shows should get the bonus
        let stats = WindowStats {
            window_shown: 0,
            window_applied: 0,
            window_start_iteration: 0,
        };
        let score = calculate_ucb_score(&stats, Confidence::Medium, 100);
        assert!((score - NEW_LEARNING_BONUS).abs() < f64::EPSILON);

        let stats2 = WindowStats {
            window_shown: 2,
            window_applied: 1,
            window_start_iteration: 0,
        };
        let score2 = calculate_ucb_score(&stats2, Confidence::High, 100);
        assert!((score2 - NEW_LEARNING_BONUS).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ucb_score_established_learning() {
        // Learning with good track record
        let stats = WindowStats {
            window_shown: 10,
            window_applied: 8,
            window_start_iteration: 0,
        };
        let score_high = calculate_ucb_score(&stats, Confidence::High, 100);
        let score_low = calculate_ucb_score(&stats, Confidence::Low, 100);

        // High confidence should score higher than low
        assert!(score_high > score_low);

        // Both should have positive scores
        assert!(score_high > 0.0);
        assert!(score_low > 0.0);
    }

    #[test]
    fn test_ucb_score_exploration_vs_exploitation() {
        // Well-proven learning: high apply rate
        let proven = WindowStats {
            window_shown: 100,
            window_applied: 90,
            window_start_iteration: 0,
        };
        let proven_score = calculate_ucb_score(&proven, Confidence::High, 1000);

        // Underexplored learning: few shows
        let underexplored = WindowStats {
            window_shown: 5,
            window_applied: 3,
            window_start_iteration: 0,
        };
        let underexplored_score = calculate_ucb_score(&underexplored, Confidence::High, 1000);

        // Underexplored should get exploration bonus
        // Both should be positive
        assert!(proven_score > 0.0);
        assert!(underexplored_score > 0.0);
    }

    #[test]
    fn test_ucb_scores_change_with_feedback() {
        // Before any feedback
        let before = WindowStats {
            window_shown: 5,
            window_applied: 1,
            window_start_iteration: 0,
        };
        let score_before = calculate_ucb_score(&before, Confidence::Medium, 50);

        // After positive feedback
        let after = WindowStats {
            window_shown: 6,
            window_applied: 2,
            window_start_iteration: 0,
        };
        let score_after = calculate_ucb_score(&after, Confidence::Medium, 51);

        // Score should change with feedback (not necessarily increase due to exploration term)
        assert!((score_before - score_after).abs() > 0.0);
    }

    #[test]
    fn test_window_stats_expired() {
        let stats = WindowStats {
            window_shown: 10,
            window_applied: 5,
            window_start_iteration: 0,
        };

        assert!(!stats.is_expired(WINDOW_SIZE - 1));
        assert!(stats.is_expired(WINDOW_SIZE));
        assert!(stats.is_expired(WINDOW_SIZE + 100));
    }

    #[test]
    fn test_record_learning_shown_db() {
        let (_temp_dir, conn) = setup_db();

        // Create a learning
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Test".to_string(),
            content: "Content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        };
        let result = record_learning(&conn, params).unwrap();
        let learning_id = result.learning_id;

        // Record shown
        record_learning_shown(&conn, learning_id, 1).unwrap();

        // Verify counts
        let stats = get_window_stats(&conn, learning_id).unwrap();
        assert_eq!(stats.window_shown, 1);
        assert_eq!(stats.window_start_iteration, 1);

        // Record shown again
        record_learning_shown(&conn, learning_id, 2).unwrap();
        let stats = get_window_stats(&conn, learning_id).unwrap();
        assert_eq!(stats.window_shown, 2);
    }

    #[test]
    fn test_record_learning_applied_db() {
        let (_temp_dir, conn) = setup_db();

        // Create a learning
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Test".to_string(),
            content: "Content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        };
        let result = record_learning(&conn, params).unwrap();
        let learning_id = result.learning_id;

        // First show it
        record_learning_shown(&conn, learning_id, 1).unwrap();

        // Record applied
        record_learning_applied(&conn, learning_id).unwrap();

        // Verify counts
        let stats = get_window_stats(&conn, learning_id).unwrap();
        assert_eq!(stats.window_applied, 1);
    }

    #[test]
    fn test_window_refresh_on_expiry() {
        let (_temp_dir, conn) = setup_db();

        // Create a learning
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Test".to_string(),
            content: "Content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        };
        let result = record_learning(&conn, params).unwrap();
        let learning_id = result.learning_id;

        // Record some shows at iteration 1
        record_learning_shown(&conn, learning_id, 1).unwrap();
        record_learning_shown(&conn, learning_id, 2).unwrap();
        record_learning_applied(&conn, learning_id).unwrap();

        let stats = get_window_stats(&conn, learning_id).unwrap();
        assert_eq!(stats.window_shown, 2);
        assert_eq!(stats.window_applied, 1);

        // Record shown after window expires
        let new_iteration = WINDOW_SIZE + 10;
        record_learning_shown(&conn, learning_id, new_iteration).unwrap();

        // Stats should be reset
        let stats = get_window_stats(&conn, learning_id).unwrap();
        assert_eq!(stats.window_shown, 1); // Only the new show
        assert_eq!(stats.window_applied, 0); // Reset
        assert_eq!(stats.window_start_iteration, new_iteration);
    }

    #[test]
    fn test_get_total_window_shows() {
        let (_temp_dir, conn) = setup_db();

        // Initially zero
        let total = get_total_window_shows(&conn).unwrap();
        assert_eq!(total, 0);

        // Create and show some learnings
        for i in 1..=3 {
            let params = RecordLearningParams {
                outcome: LearningOutcome::Pattern,
                title: format!("Test {}", i),
                content: "Content".to_string(),
                task_id: None,
                run_id: None,
                root_cause: None,
                solution: None,
                applies_to_files: None,
                applies_to_task_types: None,
                applies_to_errors: None,
                tags: None,
                confidence: Confidence::Medium,
            };
            let result = record_learning(&conn, params).unwrap();
            record_learning_shown(&conn, result.learning_id, 1).unwrap();
            record_learning_shown(&conn, result.learning_id, 2).unwrap();
        }

        // Total should be 6 (3 learnings x 2 shows each)
        let total = get_total_window_shows(&conn).unwrap();
        assert_eq!(total, 6);
    }

    #[test]
    fn test_rank_learnings_by_ucb() {
        let (_temp_dir, conn) = setup_db();

        // Create learnings with different track records
        let params1 = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Proven".to_string(),
            content: "Content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        };
        let r1 = record_learning(&conn, params1).unwrap();

        let params2 = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "New".to_string(),
            content: "Content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        };
        let _r2 = record_learning(&conn, params2).unwrap();

        // Give first learning a track record
        for i in 1..=10 {
            record_learning_shown(&conn, r1.learning_id, i).unwrap();
            record_learning_applied(&conn, r1.learning_id).unwrap();
        }

        // Query learnings
        let mut stmt = conn.prepare("SELECT * FROM learnings ORDER BY id").unwrap();
        let learnings: Vec<Learning> = stmt
            .query_map([], |row| {
                Learning::try_from(row)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        // Rank them
        let ranked = rank_learnings_by_ucb(&conn, learnings).unwrap();

        // Should have both learnings
        assert_eq!(ranked.len(), 2);

        // New learning (with exploration bonus) should likely rank first
        assert_eq!(ranked[0].title, "New");
    }
}
