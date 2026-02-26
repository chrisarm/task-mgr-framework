//! Task-context pattern matching backend.
//!
//! Scores learnings against task metadata (file patterns, type prefix, error
//! patterns). Only produces results when the query contains task context.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::models::Learning;
use crate::TaskMgrResult;

use super::{RetrievalBackend, RetrievalQuery, ScoredLearning};

/// Points awarded for file pattern match.
const FILE_MATCH_SCORE: i32 = 10;
/// Points awarded for task type prefix match.
const TYPE_MATCH_SCORE: i32 = 5;
/// Points awarded for error pattern match.
const ERROR_MATCH_SCORE: i32 = 2;
/// Points awarded for semantic tag-to-path context match.
const TAG_CONTEXT_MATCH_SCORE: i32 = 3;

/// Tags that are source/meta or generic category tags and must never trigger tag-path scoring.
const EXCLUDED_TAGS: &[&str] = &[
    "long-term",
    "raw",
    "rust-patterns",
    "python-patterns",
    "architecture-patterns",
    "database-sql",
    "testing-patterns",
    "general",
];

/// Pattern matching backend for task-based recall.
///
/// Scores learnings based on how well their applicability metadata matches
/// the current task's context (files, type prefix, error messages).
pub struct PatternsBackend;

impl RetrievalBackend for PatternsBackend {
    fn name(&self) -> &str {
        "patterns"
    }

    fn retrieve(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        // Patterns backend needs task context to work
        if query.task_files.is_empty() && query.task_prefix.is_none() && query.task_error.is_none()
        {
            return Ok(Vec::new());
        }

        let candidates = load_learnings_with_applicability(conn)?;

        // Batch-load tags when a tags filter is present OR task_files is non-empty
        // (single query vs. O(N); tag scoring requires task_files to be set)
        let needs_tags =
            query.tags.as_ref().is_some_and(|t| !t.is_empty()) || !query.task_files.is_empty();
        let tags_map = if needs_tags {
            let ids: Vec<i64> = candidates.iter().filter_map(|l| l.id).collect();
            batch_get_learning_tags(conn, &ids)?
        } else {
            HashMap::new()
        };

        let mut scored: Vec<ScoredLearning> = Vec::new();

        for learning in candidates {
            let mut score = 0i32;
            let mut reasons = Vec::new();

            // File pattern matching
            if let Some(ref patterns) = learning.applies_to_files {
                for file in &query.task_files {
                    if patterns.iter().any(|p| file_matches_pattern(file, p)) {
                        score += FILE_MATCH_SCORE;
                        reasons.push("file pattern match".to_string());
                        break;
                    }
                }
            }

            // Task type prefix matching
            if let Some(ref prefixes) = learning.applies_to_task_types {
                if let Some(ref task_prefix) = query.task_prefix {
                    if prefixes.iter().any(|p| task_prefix.starts_with(p)) {
                        score += TYPE_MATCH_SCORE;
                        reasons.push("task type match".to_string());
                    }
                }
            }

            // Error pattern matching
            if let (Some(ref task_error), Some(ref error_patterns)) =
                (&query.task_error, &learning.applies_to_errors)
            {
                if error_patterns
                    .iter()
                    .any(|p| task_error.to_lowercase().contains(&p.to_lowercase()))
                {
                    score += ERROR_MATCH_SCORE;
                    reasons.push("error pattern match".to_string());
                }
            }

            // Tag context matching (only fires when task_files is non-empty)
            if !query.task_files.is_empty() {
                let learning_id = learning.id.unwrap_or(0);
                let learning_tags = tags_map.get(&learning_id).map(Vec::as_slice).unwrap_or(&[]);
                if learning_tags
                    .iter()
                    .any(|tag| tag_matches_task_files(tag, &query.task_files))
                {
                    score += TAG_CONTEXT_MATCH_SCORE;
                    reasons.push("tag context match".to_string());
                }
            }

            // Apply outcome filter if provided
            if let Some(ref outcome_filter) = query.outcome {
                if learning.outcome != *outcome_filter {
                    continue;
                }
            }

            // Apply tags filter if provided (uses batch-loaded tags)
            if let Some(ref tags_filter) = query.tags {
                if !tags_filter.is_empty() {
                    let learning_tags = tags_map
                        .get(&learning.id.unwrap_or(0))
                        .cloned()
                        .unwrap_or_default();
                    if !tags_filter.iter().any(|t| learning_tags.contains(t)) {
                        continue;
                    }
                }
            }

            if score > 0 {
                scored.push(ScoredLearning {
                    learning,
                    relevance_score: f64::from(score),
                    match_reason: if reasons.is_empty() {
                        None
                    } else {
                        Some(reasons.join(", "))
                    },
                });
            }
        }

        // Sort by score DESC, then by last_applied_at DESC
        scored.sort_by(|a, b| {
            b.relevance_score
                .partial_cmp(&a.relevance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    compare_option_datetimes(
                        &b.learning.last_applied_at,
                        &a.learning.last_applied_at,
                    )
                })
        });

        // Truncate to limit
        scored.truncate(query.limit);

        Ok(scored)
    }
}

/// Resolve task context from the database for a given task ID.
///
/// Returns (task_files, task_prefix, task_error) that can be set on a
/// [`RetrievalQuery`].
pub fn resolve_task_context(
    conn: &Connection,
    task_id: &str,
) -> TaskMgrResult<(Vec<String>, Option<String>, Option<String>)> {
    let task_files = get_task_files(conn, task_id)?;
    let task_prefix = Some(extract_task_prefix(task_id));
    let task_error = get_task_error(conn, task_id)?;
    Ok((task_files, task_prefix, task_error))
}

/// Gets files associated with a task.
fn get_task_files(conn: &Connection, task_id: &str) -> TaskMgrResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT file_path FROM task_files WHERE task_id = ?1")?;
    let files: Vec<String> = stmt
        .query_map([task_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(files)
}

/// Gets the last error for a task.
fn get_task_error(conn: &Connection, task_id: &str) -> TaskMgrResult<Option<String>> {
    let result = conn.query_row(
        "SELECT last_error FROM tasks WHERE id = ?1",
        [task_id],
        |row| row.get(0),
    );

    match result {
        Ok(error) => Ok(error),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Strips a leading UUID prefix (8 hex chars followed by `-`) from a task ID.
///
/// E.g., `"f424ade5-PA-FEAT-003"` → `"PA-FEAT-003"`, `"US-001"` → `"US-001"`.
fn strip_uuid_prefix(task_id: &str) -> &str {
    // UUID prefix is exactly 8 hex digits followed by a dash
    if task_id.len() >= 9 {
        let (prefix, rest) = task_id.split_at(9);
        if prefix.len() == 9
            && prefix.as_bytes()[8] == b'-'
            && prefix[..8].chars().all(|c| c.is_ascii_hexdigit())
        {
            return rest;
        }
    }
    task_id
}

/// Extracts the task type prefix from a task ID.
///
/// First strips any leading UUID prefix, then returns the full remaining ID.
/// The caller's `applies_to_task_types` entries use `starts_with()` matching,
/// so returning the full ID (e.g., `"US-001"`) lets `"US-"` match naturally.
///
/// E.g., `"f424ade5-PA-FEAT-003"` → `"PA-FEAT-003"`, `"US-001"` → `"US-001"`.
pub(crate) fn extract_task_prefix(task_id: &str) -> String {
    strip_uuid_prefix(task_id).to_string()
}

/// Extracts the task type prefix from a task prefix string.
///
/// Returns everything up to and including the first `-`. This allows learnings
/// to match all tasks of the same type via `starts_with()` scoring.
///
/// E.g., `"FEAT-003"` → `"FEAT-"`, `"US-001"` → `"US-"`.
pub(crate) fn type_prefix_from(task_prefix: &str) -> String {
    if let Some(pos) = task_prefix.find('-') {
        task_prefix[..=pos].to_string()
    } else {
        task_prefix.to_string()
    }
}

/// Loads learnings that have applicability metadata.
fn load_learnings_with_applicability(conn: &Connection) -> TaskMgrResult<Vec<Learning>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            l.id, l.created_at, l.task_id, l.run_id, l.outcome, l.title, l.content,
            l.root_cause, l.solution,
            l.applies_to_files, l.applies_to_task_types, l.applies_to_errors,
            l.confidence, l.times_shown, l.times_applied, l.last_shown_at, l.last_applied_at
        FROM learnings l
        WHERE l.applies_to_files IS NOT NULL
           OR l.applies_to_task_types IS NOT NULL
           OR l.applies_to_errors IS NOT NULL
           OR EXISTS (SELECT 1 FROM learning_tags lt WHERE lt.learning_id = l.id)
        "#,
    )?;

    let learnings: Vec<Learning> = stmt
        .query_map([], |row| {
            Learning::try_from(row)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(learnings)
}

/// Batch-loads tags for multiple learnings in a single query.
fn batch_get_learning_tags(
    conn: &Connection,
    learning_ids: &[i64],
) -> TaskMgrResult<HashMap<i64, Vec<String>>> {
    if learning_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders: Vec<String> = (1..=learning_ids.len())
        .map(|i| format!("?{}", i))
        .collect();
    let sql = format!(
        "SELECT learning_id, tag FROM learning_tags WHERE learning_id IN ({})",
        placeholders.join(", ")
    );
    let params: Vec<&dyn rusqlite::ToSql> = learning_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, tag) = row?;
        map.entry(id).or_default().push(tag);
    }
    Ok(map)
}

/// Maps a tag keyword (a single hyphen-separated token) to a path prefix, if any.
fn tag_keyword_to_path_prefix(keyword: &str) -> Option<&'static str> {
    match keyword {
        "workflow" => Some("workflow/"),
        "ses" | "email" => Some("ses/"),
        "pto" => Some("date/"),
        "embedding" => Some("kb/"),
        "consumer" => Some("consumer/"),
        _ => None,
    }
}

/// Returns true if a tag semantically maps to any of the given task file paths.
///
/// Tags in [`EXCLUDED_TAGS`] never trigger scoring.  For all other tags, the tag
/// is split on `-` and each token is checked against [`tag_keyword_to_path_prefix`].
/// If any task file path contains the mapped prefix the tag is considered a match.
fn tag_matches_task_files(tag: &str, task_files: &[String]) -> bool {
    if EXCLUDED_TAGS.contains(&tag) {
        return false;
    }
    tag.split('-').any(|keyword| {
        tag_keyword_to_path_prefix(keyword)
            .is_some_and(|prefix| task_files.iter().any(|f| f.contains(prefix)))
    })
}

/// Checks if a file path matches a pattern.
/// Supports simple glob patterns with * wildcard.
pub(crate) fn file_matches_pattern(file_path: &str, pattern: &str) -> bool {
    let pattern_lower = pattern.to_lowercase();
    let file_lower = file_path.to_lowercase();

    // If no wildcard, check exact match or containment
    if !pattern.contains('*') {
        return file_lower.contains(&pattern_lower);
    }

    // Simple glob matching: split on * and check each part
    let parts: Vec<&str> = pattern_lower.split('*').collect();
    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if let Some(found_pos) = file_lower[pos..].find(part) {
            // For first part, must match at start if pattern doesn't start with *
            if i == 0 && !pattern.starts_with('*') && found_pos != 0 {
                return false;
            }
            pos += found_pos + part.len();
        } else {
            return false;
        }
    }

    // For last part, must match at end if pattern doesn't end with *
    if !parts.is_empty() && !pattern.ends_with('*') {
        let last_part = parts.last().unwrap();
        if !last_part.is_empty() && !file_lower.ends_with(last_part) {
            return false;
        }
    }

    true
}

/// Compares two optional datetimes for sorting (descending).
fn compare_option_datetimes(
    a: &Option<chrono::DateTime<chrono::Utc>>,
    b: &Option<chrono::DateTime<chrono::Utc>>,
) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a_dt), Some(b_dt)) => a_dt.cmp(b_dt),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}
