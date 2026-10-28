//! Show-learning command implementation.
//!
//! Prints the full record for one or more learnings by ID. `recall` and the
//! loop prompt both abbreviate content (100-char preview / 4 KB budget) and
//! omit `root_cause` / `solution`; this is the lookup that gives agents the
//! whole learning without reaching into SQLite directly.
//!
//! Read-only on purpose: it does NOT bump `times_shown`, so the UCB bandit
//! stays driven by what the loop actually injected and what agents applied.

use std::collections::HashSet;

use rusqlite::Connection;
use serde::Serialize;

use crate::learnings::{get_learning, get_learning_tags};
use crate::models::Learning;
use crate::{TaskMgrError, TaskMgrResult};

/// Full detail for a single learning.
#[derive(Debug, Clone, Serialize)]
pub struct LearningDetail {
    #[serde(flatten)]
    pub learning: Learning,
    pub tags: Vec<String>,
    /// Set when the learning was soft-archived by `curate retire`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<String>,
    /// Learnings that replace this one (recall hides superseded learnings).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub superseded_by: Vec<i64>,
    /// Learnings this one consolidates.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<i64>,
}

/// Result of the show-learning command.
#[derive(Debug, Clone, Serialize)]
pub struct ShowLearningResult {
    /// Found learnings, in the order requested (duplicates removed).
    pub learnings: Vec<LearningDetail>,
    /// Requested IDs with no matching row.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_found: Vec<i64>,
}

/// Loads full detail for each requested learning ID.
///
/// Retired and superseded learnings are returned (annotated) — an explicit
/// ID lookup is an intentional request, unlike recall's ranked search.
///
/// # Errors
///
/// Returns `TaskMgrError::NotFound` when IDs were requested and none exist.
/// A partial miss is not an error: the misses are listed in `not_found` so a
/// batch lookup with one stale ID still returns the rest.
pub fn show_learnings(conn: &Connection, ids: &[i64]) -> TaskMgrResult<ShowLearningResult> {
    let mut seen = HashSet::new();
    let mut learnings = Vec::new();
    let mut not_found = Vec::new();

    for &id in ids {
        if !seen.insert(id) {
            continue;
        }
        match get_learning(conn, id)? {
            Some(learning) => learnings.push(load_detail(conn, id, learning)?),
            None => not_found.push(id),
        }
    }

    if learnings.is_empty() && !not_found.is_empty() {
        let missing: Vec<String> = not_found.iter().map(i64::to_string).collect();
        return Err(TaskMgrError::learning_not_found(missing.join(", ")));
    }

    Ok(ShowLearningResult {
        learnings,
        not_found,
    })
}

fn load_detail(conn: &Connection, id: i64, learning: Learning) -> TaskMgrResult<LearningDetail> {
    let tags = get_learning_tags(conn, id)?;
    let retired_at: Option<String> = conn.query_row(
        "SELECT retired_at FROM learnings WHERE id = ?1",
        [id],
        |row| row.get(0),
    )?;
    let superseded_by = query_ids(
        conn,
        "SELECT new_learning_id FROM learning_supersessions \
         WHERE old_learning_id = ?1 ORDER BY new_learning_id",
        id,
    )?;
    let supersedes = query_ids(
        conn,
        "SELECT old_learning_id FROM learning_supersessions \
         WHERE new_learning_id = ?1 ORDER BY old_learning_id",
        id,
    )?;

    Ok(LearningDetail {
        learning,
        tags,
        retired_at,
        superseded_by,
        supersedes,
    })
}

fn query_ids(conn: &Connection, sql: &str, id: i64) -> TaskMgrResult<Vec<i64>> {
    let mut stmt = conn.prepare(sql)?;
    let ids = stmt
        .query_map([id], |row| row.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(ids)
}

fn join_ids(ids: &[i64]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formats the show-learning result as text, with every field untruncated.
#[must_use]
pub fn format_text(result: &ShowLearningResult) -> String {
    let mut out = String::new();

    for (i, detail) in result.learnings.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let l = &detail.learning;
        let id = l.id.map(|id| id.to_string()).unwrap_or_default();
        out.push_str(&format!("=== Learning #{id}: {} ===\n", l.title));
        out.push_str(&format!(
            "Outcome: {} | Confidence: {} | Created: {}\n",
            l.outcome,
            l.confidence,
            l.created_at.format("%Y-%m-%d %H:%M:%S")
        ));
        if let Some(ref task_id) = l.task_id {
            out.push_str(&format!("Task: {task_id}\n"));
        }
        if let Some(ref run_id) = l.run_id {
            out.push_str(&format!("Run: {run_id}\n"));
        }
        if let Some(ref retired_at) = detail.retired_at {
            out.push_str(&format!("RETIRED: {retired_at}\n"));
        }
        if !detail.superseded_by.is_empty() {
            out.push_str(&format!(
                "SUPERSEDED BY: {}\n",
                join_ids(&detail.superseded_by)
            ));
        }
        if !detail.supersedes.is_empty() {
            out.push_str(&format!("Supersedes: {}\n", join_ids(&detail.supersedes)));
        }
        if !detail.tags.is_empty() {
            out.push_str(&format!("Tags: {}\n", detail.tags.join(", ")));
        }
        if let Some(ref files) = l.applies_to_files {
            out.push_str(&format!("Files: {}\n", files.join(", ")));
        }
        if let Some(ref types) = l.applies_to_task_types {
            out.push_str(&format!("Task types: {}\n", types.join(", ")));
        }
        if let Some(ref errors) = l.applies_to_errors {
            out.push_str(&format!("Errors: {}\n", errors.join(", ")));
        }
        out.push_str(&format!(
            "Stats: {} shown, {} applied\n",
            l.times_shown, l.times_applied
        ));

        out.push_str(&format!("\nContent:\n{}\n", l.content.trim_end()));
        if let Some(ref root_cause) = l.root_cause {
            out.push_str(&format!("\nRoot cause:\n{}\n", root_cause.trim_end()));
        }
        if let Some(ref solution) = l.solution {
            out.push_str(&format!("\nSolution:\n{}\n", solution.trim_end()));
        }
    }

    if !result.not_found.is_empty() {
        if !result.learnings.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("Not found: {}\n", join_ids(&result.not_found)));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, migrations::run_migrations, open_connection};
    use crate::learnings::crud::{RecordLearningParams, record_learning};
    use crate::models::{Confidence, LearningOutcome};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    fn insert(conn: &Connection, title: &str, content: &str) -> i64 {
        // learnings.task_id is an FK to tasks.
        conn.execute(
            "INSERT OR IGNORE INTO tasks (id, title, status, priority) \
             VALUES ('FEAT-001', 'Test Task', 'todo', 10)",
            [],
        )
        .unwrap();
        let params = RecordLearningParams {
            outcome: LearningOutcome::Failure,
            title: title.to_string(),
            content: content.to_string(),
            task_id: Some("FEAT-001".to_string()),
            run_id: None,
            root_cause: Some("the root cause".to_string()),
            solution: Some("the solution".to_string()),
            applies_to_files: Some(vec!["src/foo.rs".to_string()]),
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: Some(vec!["db".to_string()]),
            confidence: Confidence::High,
        };
        record_learning(conn, params).unwrap().learning_id
    }

    #[test]
    fn returns_full_untruncated_content_and_root_cause_solution() {
        let (_tmp, conn) = setup_db();
        let long = "x".repeat(5_000);
        let id = insert(&conn, "Long one", &long);

        let result = show_learnings(&conn, &[id]).unwrap();
        let text = format_text(&result);

        assert_eq!(result.learnings.len(), 1);
        assert!(text.contains(&long), "content must not be truncated");
        assert!(text.contains("Root cause:\nthe root cause"));
        assert!(text.contains("Solution:\nthe solution"));
        assert!(text.contains("Tags: db"));
        assert!(text.contains("Files: src/foo.rs"));
        assert!(text.contains("Task: FEAT-001"));
    }

    #[test]
    fn preserves_request_order_and_dedups() {
        let (_tmp, conn) = setup_db();
        let a = insert(&conn, "A", "a");
        let b = insert(&conn, "B", "b");

        let result = show_learnings(&conn, &[b, a, b]).unwrap();
        let ids: Vec<i64> = result
            .learnings
            .iter()
            .map(|d| d.learning.id.unwrap())
            .collect();
        assert_eq!(ids, vec![b, a]);
    }

    #[test]
    fn partial_miss_returns_found_and_lists_missing() {
        let (_tmp, conn) = setup_db();
        let a = insert(&conn, "A", "a");

        let result = show_learnings(&conn, &[a, 999_999]).unwrap();
        assert_eq!(result.learnings.len(), 1);
        assert_eq!(result.not_found, vec![999_999]);
        assert!(format_text(&result).contains("Not found: #999999"));
    }

    #[test]
    fn all_missing_is_not_found_error() {
        let (_tmp, conn) = setup_db();
        let err = show_learnings(&conn, &[999_998, 999_999]).unwrap_err();
        assert!(matches!(err, TaskMgrError::NotFound { .. }));
        assert!(err.to_string().contains("999998"));
    }

    #[test]
    fn annotates_retired_and_superseded() {
        let (_tmp, conn) = setup_db();
        let old = insert(&conn, "Old", "old");
        let new = insert(&conn, "New", "new");
        conn.execute(
            "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (?1, ?2)",
            [old, new],
        )
        .unwrap();
        conn.execute(
            "UPDATE learnings SET retired_at = '2026-10-01 00:00:00' WHERE id = ?1",
            [old],
        )
        .unwrap();

        let result = show_learnings(&conn, &[old, new]).unwrap();
        assert_eq!(result.learnings[0].superseded_by, vec![new]);
        assert_eq!(result.learnings[1].supersedes, vec![old]);

        let text = format_text(&result);
        assert!(text.contains("RETIRED: 2026-10-01 00:00:00"));
        assert!(text.contains(&format!("SUPERSEDED BY: #{new}")));
        assert!(text.contains(&format!("Supersedes: #{old}")));
    }

    #[test]
    fn does_not_bump_times_shown() {
        let (_tmp, conn) = setup_db();
        let a = insert(&conn, "A", "a");
        show_learnings(&conn, &[a]).unwrap();
        let shown: i32 = conn
            .query_row(
                "SELECT times_shown FROM learnings WHERE id = ?1",
                [a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(shown, 0);
    }

    #[test]
    fn json_includes_full_fields() {
        let (_tmp, conn) = setup_db();
        let a = insert(&conn, "A", "body");
        let json = serde_json::to_value(show_learnings(&conn, &[a]).unwrap()).unwrap();
        let l = &json["learnings"][0];
        assert_eq!(l["content"], "body");
        assert_eq!(l["root_cause"], "the root cause");
        assert_eq!(l["solution"], "the solution");
        assert_eq!(l["tags"][0], "db");
        assert!(json.get("not_found").is_none());
    }
}
