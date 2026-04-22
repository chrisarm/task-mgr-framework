//! FTS5 full-text search backend with LIKE fallback.
//!
//! Wraps the existing BM25-based FTS5 query and LIKE fallback into the
//! [`RetrievalBackend`] trait. Indexing is handled by SQLite triggers, so
//! `index()` and `remove()` are no-ops.

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::models::Learning;

use super::{RetrievalBackend, RetrievalQuery, ScoredLearning};

/// FTS5 full-text search backend.
///
/// Uses SQLite FTS5 with BM25 scoring when available, falling back to
/// LIKE matching otherwise. Only produces results when the query contains
/// a text search term (`query.text`).
pub struct Fts5Backend;

impl RetrievalBackend for Fts5Backend {
    fn name(&self) -> &str {
        "fts5"
    }

    fn retrieve(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        match &query.text {
            Some(text) => {
                if is_fts5_available(conn) {
                    execute_fts5_query(conn, text, query)
                } else {
                    execute_like_query(conn, text, query)
                }
            }
            None => {
                // If task context is present, defer to PatternsBackend
                if !query.task_files.is_empty()
                    || query.task_prefix.is_some()
                    || query.task_error.is_some()
                {
                    return Ok(Vec::new());
                }
                // No text query and no task context — return recent learnings
                execute_unfiltered_query(conn, query)
            }
        }
    }

    // index() and remove() use default no-op — SQLite triggers handle FTS5 sync
}

/// Checks if FTS5 is available by checking for the learnings_fts table.
pub(crate) fn is_fts5_available(conn: &Connection) -> bool {
    let result: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='learnings_fts'",
        [],
        |row| row.get(0),
    );
    matches!(result, Ok(count) if count > 0)
}

/// Escapes LIKE metacharacters so `%` and `_` are treated as literals.
///
/// Uses backslash as the escape character (must pair with `ESCAPE '\'` in SQL).
fn escape_like_pattern(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\") // escape char itself first
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{}%", escaped)
}

/// Escapes a query string for safe use with FTS5.
pub(crate) fn escape_fts5_query(query: &str) -> String {
    let escaped = query.replace('"', "\"\"");
    format!("\"{}\"", escaped)
}

/// Appends outcome and tags filter conditions to the query builder.
///
/// `id_column` is `"l.id"` for FTS5 joins or `"id"` for direct queries.
/// `outcome_column` is `"l.outcome"` for FTS5 joins or `"outcome"` for direct queries.
fn append_common_filters(
    query: &RetrievalQuery,
    conditions: &mut Vec<String>,
    sql_params: &mut Vec<Box<dyn rusqlite::ToSql>>,
    id_column: &str,
    outcome_column: &str,
) {
    if let Some(ref outcome) = query.outcome {
        let param_num = sql_params.len() + 1;
        conditions.push(format!("{} = ?{}", outcome_column, param_num));
        sql_params.push(Box::new(outcome.as_db_str().to_string()));
    }
    if let Some(ref tags) = query.tags
        && !tags.is_empty()
    {
        let param_start = sql_params.len() + 1;
        let placeholders: Vec<String> = (0..tags.len())
            .map(|i| format!("?{}", param_start + i))
            .collect();
        conditions.push(format!(
            "{} IN (SELECT learning_id FROM learning_tags WHERE tag IN ({}))",
            id_column,
            placeholders.join(", ")
        ));
        for tag in tags {
            sql_params.push(Box::new(tag.clone()));
        }
    }
}

/// Executes FTS5 query with BM25 scoring, returning ScoredLearning results.
fn execute_fts5_query(
    conn: &Connection,
    text: &str,
    query: &RetrievalQuery,
) -> TaskMgrResult<Vec<ScoredLearning>> {
    let fts_query = escape_fts5_query(text);

    let mut conditions = Vec::new();
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // FTS5 query is parameter 1
    sql_params.push(Box::new(fts_query));

    append_common_filters(query, &mut conditions, &mut sql_params, "l.id", "l.outcome");

    let additional_where = if conditions.is_empty() {
        String::new()
    } else {
        format!("AND {}", conditions.join(" AND "))
    };

    let supersession_clause = if query.include_superseded {
        String::new()
    } else {
        format!("AND l.id {}", super::SUPERSESSION_SUBQUERY)
    };

    // FTS5 query with BM25 scoring
    // bm25() returns a negative score (lower = better match), so we negate it
    let sql = format!(
        r#"
        SELECT
            l.id, l.created_at, l.task_id, l.run_id, l.outcome, l.title, l.content,
            l.root_cause, l.solution,
            l.applies_to_files, l.applies_to_task_types, l.applies_to_errors,
            l.confidence, l.times_shown, l.times_applied, l.last_shown_at, l.last_applied_at,
            -bm25(learnings_fts) AS relevance
        FROM learnings l
        INNER JOIN learnings_fts fts ON l.id = fts.rowid
        WHERE learnings_fts MATCH ?1
        AND l.retired_at IS NULL
        {}
        {}
        ORDER BY relevance DESC
        LIMIT ?
        "#,
        supersession_clause, additional_where
    );

    sql_params.push(Box::new(query.limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|p| p.as_ref()).collect();

    let results: Vec<ScoredLearning> = stmt
        .query_map(param_refs.as_slice(), |row| {
            let learning = Learning::try_from(row)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let relevance: f64 = row.get("relevance")?;
            Ok(ScoredLearning {
                learning,
                relevance_score: relevance,
                match_reason: Some("FTS5 text match".to_string()),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}

/// Executes LIKE-based fallback query, returning ScoredLearning results.
fn execute_like_query(
    conn: &Connection,
    text: &str,
    query: &RetrievalQuery,
) -> TaskMgrResult<Vec<ScoredLearning>> {
    let mut conditions = Vec::new();
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // Exclude retired learnings
    conditions.push("retired_at IS NULL".to_string());

    // Text search condition (escape LIKE metacharacters)
    conditions.push("(title LIKE ?1 ESCAPE '\\' OR content LIKE ?1 ESCAPE '\\')".to_string());
    let pattern = escape_like_pattern(text);
    sql_params.push(Box::new(pattern));

    if !query.include_superseded {
        conditions.push(format!("id {}", super::SUPERSESSION_SUBQUERY));
    }

    append_common_filters(query, &mut conditions, &mut sql_params, "id", "outcome");

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = format!(
        r#"
        SELECT
            id, created_at, task_id, run_id, outcome, title, content,
            root_cause, solution,
            applies_to_files, applies_to_task_types, applies_to_errors,
            confidence, times_shown, times_applied, last_shown_at, last_applied_at
        FROM learnings
        {}
        ORDER BY
            CASE WHEN last_applied_at IS NULL THEN 1 ELSE 0 END,
            last_applied_at DESC,
            created_at DESC
        LIMIT ?
        "#,
        where_clause
    );

    sql_params.push(Box::new(query.limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|p| p.as_ref()).collect();

    let results: Vec<ScoredLearning> = stmt
        .query_map(param_refs.as_slice(), |row| {
            let learning = Learning::try_from(row)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            Ok(ScoredLearning {
                learning,
                // LIKE matches get a fixed relevance; ordering comes from SQL ORDER BY
                relevance_score: 1.0,
                match_reason: Some("LIKE text match".to_string()),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}

/// Returns recent learnings with optional outcome/tags filters (no text search).
fn execute_unfiltered_query(
    conn: &Connection,
    query: &RetrievalQuery,
) -> TaskMgrResult<Vec<ScoredLearning>> {
    let mut conditions = vec!["retired_at IS NULL".to_string()];
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if !query.include_superseded {
        conditions.push(format!("id {}", super::SUPERSESSION_SUBQUERY));
    }

    append_common_filters(query, &mut conditions, &mut sql_params, "id", "outcome");

    let where_clause = format!("WHERE {}", conditions.join(" AND "));

    let sql = format!(
        r#"
        SELECT
            id, created_at, task_id, run_id, outcome, title, content,
            root_cause, solution,
            applies_to_files, applies_to_task_types, applies_to_errors,
            confidence, times_shown, times_applied, last_shown_at, last_applied_at
        FROM learnings
        {}
        ORDER BY
            CASE WHEN last_applied_at IS NULL THEN 1 ELSE 0 END,
            last_applied_at DESC,
            created_at DESC
        LIMIT ?
        "#,
        where_clause
    );

    sql_params.push(Box::new(query.limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|p| p.as_ref()).collect();

    let results: Vec<ScoredLearning> = stmt
        .query_map(param_refs.as_slice(), |row| {
            let learning = Learning::try_from(row)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            Ok(ScoredLearning {
                learning,
                // Fixed score — stable sort in CompositeBackend preserves SQL ordering
                relevance_score: 0.5,
                match_reason: Some("recency ordering".to_string()),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}
