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

use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::TaskMgrResult;
use crate::models::{Learning, LearningOutcome};

use super::bandit;
use super::reranker::Reranker;
use super::retrieval::patterns::resolve_task_context;
use super::retrieval::{CompositeBackend, RetrievalBackend, RetrievalQuery, ScoredLearning};

/// Hard cap on the candidate slate size sent to the reranker.
///
/// Bounds rerank latency and request body size regardless of `limit *
/// reranker_over_fetch`. Cross-encoder rerank is the dominant cost of recall
/// when enabled; 30 docs keeps p99 latency reasonable on commodity GPUs.
const MAX_RERANK_SLATE: usize = 30;

/// Width of the rerank-score band inside which UCB breaks ties.
///
/// jina-v2 reranker emits sigmoid-bounded scores in `(0, 1)`, so 0.05 ≈ 5
/// score points — small enough that UCB exploration only fires between
/// near-identical candidates, never between clearly-better and clearly-worse
/// ones. If a future reranker model returns scores outside `(0, 1)` this
/// constant should be revisited (or replaced with a min-max normalisation).
const RERANK_TIEBREAK_BAND: f64 = 0.05;

/// Parameters for recalling learnings.
///
/// Not `Clone` because [`Self::reranker`] holds a `Box<dyn Reranker>` trait
/// object. Callers move the params into [`recall_learnings`] /
/// [`recall_learnings_scored`] / [`recall_learnings_with_backend`] by value.
#[derive(Default)]
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
    /// When `false` (default), superseded learnings are excluded from results.
    pub include_superseded: bool,
    /// When `true`, the caller has opted into degraded recall: an unreachable
    /// Ollama returns empty vector results instead of a hard error. Plumbed
    /// through to the `VectorBackend` strict-mode toggle by the command layer.
    /// Lives here (not on `RetrievalQuery`) so backends stay
    /// recall-policy-agnostic.
    pub allow_degraded: bool,
    /// Optional cross-encoder reranker. When `Some` AND `query` is non-empty,
    /// the recall pipeline switches to over-fetch + per-backend union +
    /// rerank + truncate. `Send + Sync` so `RecallParams` can cross threads.
    pub reranker: Option<Box<dyn Reranker + Send + Sync>>,
    /// Per-backend over-fetch factor when reranking. Effective slate size is
    /// `min(limit * reranker_over_fetch, MAX_RERANK_SLATE)`. Must be >= 1;
    /// values of 0 are clamped to 1.
    pub reranker_over_fetch: u32,
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
    let (scored, _ucb_cache) = retrieve_and_rank(conn, &params, backend)?;

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

/// Output of [`retrieve_and_rank`]: scored rows and an optional UCB-score cache.
///
/// The cache is `Some` iff UCB re-ranking ran ([`RecallParams::for_task`] was
/// set and `scored` was non-empty).
type RankedWithUcb = (Vec<ScoredLearning>, Option<HashMap<i64, f64>>);

/// Shared retrieval pipeline: backend lookup + UCB fallback + re-ranking.
///
/// Returns `ScoredLearning` rows in final ranked order plus an optional cache
/// of per-learning UCB scores computed during re-ranking.
/// [`recall_learnings_scored`] uses the cache to avoid redundant
/// `bandit::get_window_stats` calls; [`recall_learnings_with_backend`] ignores
/// it.
///
/// ## Pipeline branches
///
/// * **Rerank path** — taken when `params.reranker.is_some()` AND
///   `params.query` is `Some(non-empty)`. Over-fetches a per-backend union
///   slate via [`RetrievalBackend::retrieve_for_rerank`], invokes the
///   cross-encoder reranker, optionally applies UCB tiebreaking inside ±0.05
///   rerank-score bands when `params.for_task` is also set, then truncates to
///   `limit`. A reranker `Err` is logged to stderr and the un-reranked slate is
///   returned in its original (per-backend union) order — the recall API
///   never propagates reranker failures.
/// * **Legacy path** — preserved verbatim for the no-reranker / no-query-text
///   case: `backend.retrieve` (max-merge) + UCB fallback + `rerank_with_ucb`.
fn retrieve_and_rank(
    conn: &Connection,
    params: &RecallParams,
    backend: &dyn RetrievalBackend,
) -> TaskMgrResult<RankedWithUcb> {
    let limit = if params.limit == 0 { 5 } else { params.limit };

    // Build RetrievalQuery from RecallParams
    let mut query = RetrievalQuery {
        text: params.query.clone(),
        limit,
        tags: params.tags.clone(),
        outcome: params.outcome,
        include_superseded: params.include_superseded,
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

    // Rerank path: only when a reranker is configured AND we have non-empty
    // query text. `--for-task` without `--query` deliberately skips rerank
    // (the cross-encoder needs a query string; FEAT-001's empty-string
    // short-circuit also bypasses Ollama).
    let rerank_query: Option<String> = query
        .text
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let (Some(reranker), Some(query_text)) = (params.reranker.as_ref(), rerank_query) {
        return rerank_pipeline(
            conn,
            backend,
            reranker.as_ref(),
            &mut query,
            &query_text,
            limit,
            params,
        );
    }

    // Legacy path: backend max-merge + UCB fallback + UCB-based re-rank.
    let mut scored = backend.retrieve(conn, &query)?;

    // UCB fallback + re-ranking only for task-based recall (not CLI free-text queries)
    let ucb_cache = if params.for_task.is_some() {
        // Fill empty slots with exploration candidates
        if scored.len() < limit {
            let exclude_ids: Vec<i64> = scored.iter().filter_map(|s| s.learning.id).collect();
            let remaining = limit - scored.len();
            let fallback =
                load_ucb_fallback(conn, &exclude_ids, remaining, params.include_superseded)?;
            scored.extend(fallback);
        }

        // Re-rank: relevance tier dominates, UCB breaks ties within tiers.
        // The returned cache is reused by recall_learnings_scored so we don't
        // re-query bandit stats per row.
        Some(rerank_with_ucb(conn, &mut scored)?)
    } else {
        None
    };

    Ok((scored, ucb_cache))
}

/// Cross-encoder rerank pipeline: over-fetch → rerank → optional UCB tiebreak → truncate.
///
/// Mutates `query.limit` to the inflated slate size before fetching candidates;
/// the original `limit` is applied as the final truncation. A reranker error
/// soft-fails: emits a stderr warning and returns the un-reranked slate in
/// per-backend union order.
fn rerank_pipeline(
    conn: &Connection,
    backend: &dyn RetrievalBackend,
    reranker: &(dyn Reranker + Send + Sync),
    query: &mut RetrievalQuery,
    query_text: &str,
    limit: usize,
    params: &RecallParams,
) -> TaskMgrResult<RankedWithUcb> {
    let over_fetch = params.reranker_over_fetch.max(1);
    let slate_limit = limit
        .saturating_mul(over_fetch as usize)
        .min(MAX_RERANK_SLATE);
    query.limit = slate_limit;

    let candidates = backend.retrieve_for_rerank(conn, query)?;
    if candidates.is_empty() {
        return Ok((Vec::new(), None));
    }

    // Clone before passing to rerank so we can fall back to the un-reranked
    // slate on Err. Cheap relative to the cross-encoder HTTP round-trip we're
    // about to make: ScoredLearning::clone is mostly a Vec<Learning> clone.
    let fallback = candidates.clone();
    let mut ranked = match reranker.rerank(query_text, candidates) {
        Ok(reranked) => reranked,
        Err(e) => {
            // Soft-fail: a reranker outage must never break recall. The Err
            // already carries the URL (LlamaBoxReranker formats the request
            // URL into its connection error); we don't duplicate it here.
            crate::output::warn(&format!("reranker: {e}; using un-reranked order"));
            fallback
        }
    };

    // UCB tiebreak only fires when both --query and --for-task are present.
    // Free-text recall (--query alone) intentionally skips UCB so CLI users
    // get a deterministic relevance ordering.
    let ucb_cache = if params.for_task.is_some() {
        apply_ucb_tiebreak(conn, &mut ranked)?;
        ranked.truncate(limit);
        // Populate the cache from the final truncated set so
        // recall_learnings_scored surfaces UCB values without a second
        // bandit::get_window_stats round-trip per row. Mirrors the legacy
        // path's invariant: ucb_score is Some iff for_task is set.
        let total_window_shows = bandit::get_total_window_shows(conn)?;
        let mut cache: HashMap<i64, f64> = HashMap::new();
        for s in ranked.iter() {
            if let Some(id) = s.learning.id {
                cache.insert(id, ucb_for_learning(conn, &s.learning, total_window_shows));
            }
        }
        Some(cache)
    } else {
        ranked.truncate(limit);
        None
    };

    Ok((ranked, ucb_cache))
}

/// In-band UCB tiebreak: stable-sort each ±0.05 rerank-score band by UCB desc.
///
/// Assumes `ranked` is already sorted descending by `relevance_score` (the
/// cross-encoder score, in `(0, 1)` for jina-v2). Walks contiguous runs whose
/// scores fall within [`RERANK_TIEBREAK_BAND`] of the band-anchor (the
/// highest-scored element in the band) and reorders inside the run by UCB
/// descending. The relative order of bands is preserved.
fn apply_ucb_tiebreak(conn: &Connection, ranked: &mut [ScoredLearning]) -> TaskMgrResult<()> {
    let total_window_shows = bandit::get_total_window_shows(conn)?;
    let stats_fn = |id: i64| bandit::get_window_stats(conn, id).ok();
    apply_ucb_tiebreak_inner(ranked, stats_fn, total_window_shows);
    Ok(())
}

/// Inner implementation that accepts an injectable stats fetcher for testability.
///
/// Each unique learning id triggers at most one `stats_fn` call; errors are
/// absorbed via `Option` (absent entry → sentinel `WindowStats::default()`).
pub(crate) fn apply_ucb_tiebreak_inner(
    ranked: &mut [ScoredLearning],
    stats_fn: impl Fn(i64) -> Option<bandit::WindowStats>,
    total_window_shows: i64,
) {
    if ranked.len() < 2 {
        return;
    }

    let mut start = 0;
    while start < ranked.len() {
        let band_top = ranked[start].relevance_score;
        let band_floor = band_top - RERANK_TIEBREAK_BAND;
        let mut end = start + 1;
        while end < ranked.len() && ranked[end].relevance_score >= band_floor {
            end += 1;
        }
        if end - start > 1 {
            // Pre-fetch UCB stats so the sort closure makes zero DB calls.
            let mut stats_cache: HashMap<i64, bandit::WindowStats> = HashMap::new();
            for c in &ranked[start..end] {
                if let Some(id) = c.learning.id
                    && let std::collections::hash_map::Entry::Vacant(e) = stats_cache.entry(id)
                    && let Some(s) = stats_fn(id)
                {
                    e.insert(s);
                }
            }

            // Stable sort by UCB descending — preserves rerank order within
            // ties at the UCB level.
            ranked[start..end].sort_by(|a, b| {
                let ucb_a = a
                    .learning
                    .id
                    .and_then(|id| stats_cache.get(&id))
                    .map(|s| {
                        bandit::calculate_ucb_score(s, a.learning.confidence, total_window_shows)
                    })
                    .unwrap_or(0.0);
                let ucb_b = b
                    .learning
                    .id
                    .and_then(|id| stats_cache.get(&id))
                    .map(|s| {
                        bandit::calculate_ucb_score(s, b.learning.confidence, total_window_shows)
                    })
                    .unwrap_or(0.0);
                ucb_b
                    .partial_cmp(&ucb_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        start = end;
    }
}

/// Recalls learnings using the default composite backend.
///
/// This is the backward-compatible entry point that preserves the original
/// `recall_learnings` signature.
pub fn recall_learnings(conn: &Connection, params: RecallParams) -> TaskMgrResult<RecallResult> {
    let backend = CompositeBackend::default_backends();
    recall_learnings_with_backend(conn, params, &backend)
}

/// A scored learning output, preserving numeric retrieval signals.
///
/// Output type for [`recall_learnings_scored`]. Unlike [`RecallResult`], this
/// retains the relevance, UCB, and combined scores alongside the match reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredLearningOutput {
    /// The retrieved learning
    pub learning: Learning,
    /// Backend relevance score (FTS5 BM25, pattern points, or vector cosine)
    pub relevance_score: f64,
    /// UCB bandit score (Some for `--for-task` recall, None for free-text recall)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ucb_score: Option<f64>,
    /// Final ranking score: `relevance_score * 100.0 + ucb_score` for task recall;
    /// equal to `relevance_score` when no UCB applies.
    pub combined_score: f64,
    /// Human-readable explanation of why this matched
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_reason: Option<String>,
}

/// Result of [`recall_learnings_scored`] — mirrors [`RecallResult`] but preserves scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredRecallResult {
    /// The scored learnings that matched the query, ordered by combined_score desc
    pub scored_learnings: Vec<ScoredLearningOutput>,
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

/// Recalls learnings preserving numeric retrieval scores.
///
/// Mirrors [`recall_learnings_with_backend`] but keeps the per-row relevance,
/// UCB, and combined scores produced by the ranking pipeline. For task-based
/// recall (`params.for_task.is_some()`) every row has `ucb_score = Some(..)`
/// and `combined_score = relevance_score * 100.0 + ucb_score`. For free-text
/// recall UCB is skipped entirely: `ucb_score = None` and `combined_score`
/// equals `relevance_score`.
pub fn recall_learnings_scored(
    conn: &Connection,
    params: RecallParams,
    backend: &dyn RetrievalBackend,
) -> TaskMgrResult<ScoredRecallResult> {
    let (scored, ucb_cache) = retrieve_and_rank(conn, &params, backend)?;

    let scored_learnings: Vec<ScoredLearningOutput> = scored
        .into_iter()
        .map(|s| {
            // UCB was computed during rerank_with_ucb and cached; looking it up
            // avoids a second round-trip to bandit::get_window_stats per row.
            let ucb_score = ucb_cache
                .as_ref()
                .and_then(|cache| s.learning.id.and_then(|id| cache.get(&id).copied()));
            let combined_score = match ucb_score {
                Some(ucb) => combine_scores(s.relevance_score, ucb),
                None => s.relevance_score,
            };
            ScoredLearningOutput {
                learning: s.learning,
                relevance_score: s.relevance_score,
                ucb_score,
                combined_score,
                match_reason: s.match_reason,
            }
        })
        .collect();

    // Invariant lock: ucb_score is Some iff --for-task recall produced results.
    // Catches future refactors that change when rerank_with_ucb runs.
    debug_assert!(
        scored_learnings.is_empty()
            || params.for_task.is_some() == scored_learnings.iter().any(|s| s.ucb_score.is_some()),
        "ucb_score presence must match for_task presence"
    );

    Ok(ScoredRecallResult {
        count: scored_learnings.len(),
        scored_learnings,
        query: params.query.clone(),
        for_task: params.for_task.clone(),
        outcome_filter: params.outcome.map(|o| o.to_string()),
        tags_filter: params.tags.clone(),
    })
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
    include_superseded: bool,
) -> TaskMgrResult<Vec<ScoredLearning>> {
    if remaining_slots == 0 {
        return Ok(Vec::new());
    }

    let mut conditions = vec!["retired_at IS NULL".to_string()];
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if !include_superseded {
        conditions.push(format!(
            "id {}",
            crate::learnings::retrieval::SUPERSESSION_SUBQUERY
        ));
    }

    if !exclude_ids.is_empty() {
        let placeholders: Vec<String> =
            (1..=exclude_ids.len()).map(|i| format!("?{}", i)).collect();
        conditions.push(format!("id NOT IN ({})", placeholders.join(", ")));
        for id in exclude_ids {
            params.push(Box::new(*id));
        }
    }

    let sql = format!(
        r#"
        SELECT id, created_at, task_id, run_id, outcome, title, content,
               root_cause, solution,
               applies_to_files, applies_to_task_types, applies_to_errors,
               confidence, times_shown, times_applied, last_shown_at, last_applied_at
        FROM learnings
        WHERE {}
        "#,
        conditions.join(" AND ")
    );

    let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let learnings: Vec<Learning> = stmt
        .query_map(params_ref.as_slice(), |row| {
            Learning::try_from(row)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?
        .collect::<Result<Vec<_>, _>>()?;

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

/// Computes the UCB score for a single learning, or 0.0 if stats are unavailable.
///
/// Factored out of [`rerank_with_ucb`]'s sort closure so scored output variants
/// can capture the same values used for ranking.
fn ucb_for_learning(conn: &Connection, learning: &Learning, total_window_shows: i64) -> f64 {
    learning
        .id
        .and_then(|id| bandit::get_window_stats(conn, id).ok())
        .map(|stats| bandit::calculate_ucb_score(&stats, learning.confidence, total_window_shows))
        .unwrap_or(0.0)
}

/// Combines relevance and UCB into the final ranking score.
///
/// Pattern-matched learnings (relevance 2/5/10) always outrank fallback (0.1)
/// because the `* 100.0` scale separates tiers cleanly.
fn combine_scores(relevance_score: f64, ucb_score: f64) -> f64 {
    relevance_score * 100.0 + ucb_score
}

/// Re-ranks scored learnings so relevance tier dominates and UCB breaks ties.
///
/// Sort key: `relevance_score * 100.0 + ucb_score`. Pattern-matched learnings
/// (relevance 2/5/10) always outrank fallback learnings (0.1). Within the same
/// relevance tier, UCB balances exploitation and exploration.
///
/// Returns a cache of per-learning UCB scores computed during this pass. The
/// cache lets [`recall_learnings_scored`] surface the same values that drove
/// ranking without re-querying `bandit::get_window_stats` per row — a sort of
/// O(N log N) stat lookups becomes O(N).
fn rerank_with_ucb(
    conn: &Connection,
    scored: &mut [ScoredLearning],
) -> TaskMgrResult<HashMap<i64, f64>> {
    let mut ucb_cache: HashMap<i64, f64> = HashMap::new();
    if scored.is_empty() {
        return Ok(ucb_cache);
    }

    let total_window_shows = bandit::get_total_window_shows(conn)?;

    // Compute UCB once per learning, then sort against cached values.
    for s in scored.iter() {
        if let Some(id) = s.learning.id {
            let ucb = ucb_for_learning(conn, &s.learning, total_window_shows);
            ucb_cache.insert(id, ucb);
        }
    }

    scored.sort_by(|a, b| {
        let ucb_a = a
            .learning
            .id
            .and_then(|id| ucb_cache.get(&id).copied())
            .unwrap_or(0.0);
        let ucb_b = b
            .learning
            .id
            .and_then(|id| ucb_cache.get(&id).copied())
            .unwrap_or(0.0);
        let score_a = combine_scores(a.relevance_score, ucb_a);
        let score_b = combine_scores(b.relevance_score, ucb_b);

        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(ucb_cache)
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
