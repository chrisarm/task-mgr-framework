//! Recall operations for learnings.
//!
//! This module orchestrates learning retrieval by delegating to pluggable
//! [`RetrievalBackend`]s. UCB bandit ranking can be layered on top by callers.
//!
//! ## Architecture
//!
//! 1. Build a [`RetrievalQuery`] from [`RecallParams`]
//! 2. Call `backend.retrieve()` — pluggable (FTS5, patterns, composite, etc.)
//! 3. Extract `Vec<Learning>` from scored results
//! 4. Update shown stats, return `RecallResult`
//!
//! ## Backward Compatibility
//!
//! [`recall_learnings()`] uses `CompositeBackend::default_backends()` and preserves
//! the same public signature as the original implementation.

#[cfg(test)]
mod tests;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::models::{Learning, LearningOutcome};
use crate::TaskMgrResult;

use super::bandit;
use super::retrieval::patterns::resolve_task_context;
use super::retrieval::{CompositeBackend, RetrievalBackend, RetrievalQuery, ScoredLearning};

/// Parameters for recalling learnings.
#[derive(Debug, Clone, Default)]
pub struct RecallParams {
    /// Free-text search query (LIKE matching on title and content)
    pub query: Option<String>,
    /// Task ID to find learnings matching the task's files and type
    pub for_task: Option<String>,
    /// Filter by tags (learning must have at least one of these tags)
    pub tags: Option<Vec<String>>,
    /// Filter by outcome type
    pub outcome: Option<LearningOutcome>,
    /// Maximum number of results to return
    pub limit: usize,
}

/// Result of recalling learnings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResult {
    /// The learnings that matched the query
    pub learnings: Vec<Learning>,
    /// Number of learnings returned
    pub count: usize,
    /// The query parameters used (for debugging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub for_task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags_filter: Option<Vec<String>>,
}

/// Recalls learnings using a specific retrieval backend.
///
/// Flow:
/// 1. Build `RetrievalQuery` from `RecallParams`
/// 2. Delegate to backend
/// 3. Extract learnings from scored results
/// 4. Update shown stats
/// 5. Return `RecallResult`
pub fn recall_learnings_with_backend(
    conn: &Connection,
    params: RecallParams,
    backend: &dyn RetrievalBackend,
) -> TaskMgrResult<RecallResult> {
    let limit = if params.limit == 0 { 5 } else { params.limit };

    // Build RetrievalQuery from RecallParams
    let mut query = RetrievalQuery {
        text: params.query.clone(),
        limit,
        tags: params.tags.clone(),
        outcome: params.outcome,
        ..Default::default()
    };

    // If for_task is set, resolve task context from DB
    if let Some(ref task_id) = params.for_task {
        let (task_files, task_prefix, task_error) = resolve_task_context(conn, task_id)?;
        query.task_id = Some(task_id.clone());
        query.task_files = task_files;
        query.task_prefix = task_prefix;
        query.task_error = task_error;
    }

    // Retrieve via backend
    let mut scored = backend.retrieve(conn, &query)?;

    // UCB fallback + re-ranking only for task-based recall (not CLI free-text queries)
    if params.for_task.is_some() {
        // Fill empty slots with exploration candidates
        if scored.len() < limit {
            let exclude_ids: Vec<i64> = scored.iter().filter_map(|s| s.learning.id).collect();
            let remaining = limit - scored.len();
            let fallback = load_ucb_fallback(conn, &exclude_ids, remaining)?;
            scored.extend(fallback);
        }

        // Re-rank: relevance tier dominates, UCB breaks ties within tiers
        rerank_with_ucb(conn, &mut scored)?;
    }

    // Extract learnings
    let learnings: Vec<Learning> = scored.into_iter().map(|s| s.learning).collect();

    // Note: times_shown is updated by bandit::record_learning_shown() in
    // loop_engine/prompt.rs — not here. The recall module is retrieval-only.

    Ok(RecallResult {
        count: learnings.len(),
        learnings,
        query: params.query.clone(),
        for_task: params.for_task.clone(),
        outcome_filter: params.outcome.map(|o| o.to_string()),
        tags_filter: params.tags.clone(),
    })
}

/// Recalls learnings using the default composite backend.
///
/// This is the backward-compatible entry point that preserves the original
/// `recall_learnings` signature.
pub fn recall_learnings(conn: &Connection, params: RecallParams) -> TaskMgrResult<RecallResult> {
    let backend = CompositeBackend::default_backends();
    recall_learnings_with_backend(conn, params, &backend)
}

/// Updates times_shown and last_shown_at for the given learnings.
pub fn update_shown_stats(conn: &Connection, learnings: &[Learning]) -> TaskMgrResult<()> {
    if learnings.is_empty() {
        return Ok(());
    }

    let ids: Vec<i64> = learnings.iter().filter_map(|l| l.id).collect();
    if ids.is_empty() {
        return Ok(());
    }

    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        r#"
        UPDATE learnings
        SET times_shown = times_shown + 1,
            last_shown_at = datetime('now')
        WHERE id IN ({})
        "#,
        placeholders.join(", ")
    );

    let params: Vec<&dyn rusqlite::ToSql> =
        ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    conn.execute(&sql, params.as_slice())?;

    Ok(())
}

/// Loads UCB-ranked fallback learnings to fill empty recall slots.
///
/// Loads all learnings not in `exclude_ids`, ranks them by UCB score, and
/// returns up to `remaining_slots` as exploration candidates.
fn load_ucb_fallback(
    conn: &Connection,
    exclude_ids: &[i64],
    remaining_slots: usize,
) -> TaskMgrResult<Vec<ScoredLearning>> {
    if remaining_slots == 0 {
        return Ok(Vec::new());
    }

    // Load all learnings, optionally excluding already-matched IDs
    let learnings: Vec<Learning> = if exclude_ids.is_empty() {
        let mut stmt = conn.prepare(
            r#"
            SELECT id, created_at, task_id, run_id, outcome, title, content,
                   root_cause, solution,
                   applies_to_files, applies_to_task_types, applies_to_errors,
                   confidence, times_shown, times_applied, last_shown_at, last_applied_at
            FROM learnings
            WHERE retired_at IS NULL
            "#,
        )?;
        let result = stmt
            .query_map([], |row| {
                Learning::try_from(row)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        result
    } else {
        let placeholders: Vec<String> =
            (1..=exclude_ids.len()).map(|i| format!("?{}", i)).collect();
        let sql = format!(
            r#"
            SELECT id, created_at, task_id, run_id, outcome, title, content,
                   root_cause, solution,
                   applies_to_files, applies_to_task_types, applies_to_errors,
                   confidence, times_shown, times_applied, last_shown_at, last_applied_at
            FROM learnings
            WHERE retired_at IS NULL
            AND id NOT IN ({})
            "#,
            placeholders.join(", ")
        );
        let params: Vec<&dyn rusqlite::ToSql> = exclude_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let result = stmt
            .query_map(params.as_slice(), |row| {
                Learning::try_from(row)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        result
    };

    // Rank by UCB
    let ranked = bandit::rank_learnings_by_ucb(conn, learnings)?;

    // Take remaining_slots from the top, tag as exploration candidates
    Ok(ranked
        .into_iter()
        .take(remaining_slots)
        .map(|learning| ScoredLearning {
            learning,
            relevance_score: 0.1,
            match_reason: Some("UCB exploration".to_string()),
        })
        .collect())
}

/// Re-ranks scored learnings so relevance tier dominates and UCB breaks ties.
///
/// Sort key: `relevance_score * 100.0 + ucb_score`. Pattern-matched learnings
/// (relevance 2/5/10) always outrank fallback learnings (0.1). Within the same
/// relevance tier, UCB balances exploitation and exploration.
fn rerank_with_ucb(conn: &Connection, scored: &mut [ScoredLearning]) -> TaskMgrResult<()> {
    if scored.is_empty() {
        return Ok(());
    }

    let total_window_shows = bandit::get_total_window_shows(conn)?;

    scored.sort_by(|a, b| {
        let ucb_a = a
            .learning
            .id
            .and_then(|id| bandit::get_window_stats(conn, id).ok())
            .map(|stats| {
                bandit::calculate_ucb_score(&stats, a.learning.confidence, total_window_shows)
            })
            .unwrap_or(0.0);

        let ucb_b = b
            .learning
            .id
            .and_then(|id| bandit::get_window_stats(conn, id).ok())
            .map(|stats| {
                bandit::calculate_ucb_score(&stats, b.learning.confidence, total_window_shows)
            })
            .unwrap_or(0.0);

        let score_a = a.relevance_score * 100.0 + ucb_a;
        let score_b = b.relevance_score * 100.0 + ucb_b;

        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(())
}

/// Formats the recall result as human-readable text.
#[must_use]
pub fn format_text(result: &RecallResult) -> String {
    let mut output = String::new();

    if result.learnings.is_empty() {
        output.push_str("No matching learnings found.\n");
        return output;
    }

    output.push_str(&format!("Found {} learning(s):\n\n", result.count));

    for (i, learning) in result.learnings.iter().enumerate() {
        output.push_str(&format!(
            "{}. [{}] {} ({})\n",
            i + 1,
            learning.id.map(|id| id.to_string()).unwrap_or_default(),
            learning.title,
            learning.outcome
        ));

        // Show confidence
        output.push_str(&format!("   Confidence: {}\n", learning.confidence));

        // Show content (truncated)
        let content_preview = if learning.content.chars().count() > 100 {
            let truncated: String = learning.content.chars().take(100).collect();
            format!("{}...", truncated)
        } else {
            learning.content.clone()
        };
        output.push_str(&format!("   {}\n", content_preview));

        // Show applicability
        if let Some(ref files) = learning.applies_to_files {
            output.push_str(&format!("   Files: {}\n", files.join(", ")));
        }
        if let Some(ref types) = learning.applies_to_task_types {
            output.push_str(&format!("   Task types: {}\n", types.join(", ")));
        }

        output.push('\n');
    }

    output
}
