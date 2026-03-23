//! Import learnings from a progress.json or standalone learnings JSON file.
//!
//! Parses `ProgressExport` format (from `task-mgr export --with-progress`)
//! or a standalone `Vec<LearningExport>` array. Deduplicates by title+content
//! hash. Supports `--reset-stats` flag.

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::Serialize;

use chrono::{DateTime, Utc};

use crate::db::open_and_migrate as open_connection;
use crate::learnings::{record_learning, RecordLearningParams};
use crate::models::{LearningExport, ProgressExport};
use crate::{TaskMgrError, TaskMgrResult};

/// SQLite datetime format (matches parse_datetime in models/datetime.rs).
const SQLITE_DATETIME_FMT: &str = "%Y-%m-%d %H:%M:%S";

/// Result of the import-learnings command.
#[derive(Debug, Clone, Serialize)]
pub struct ImportLearningsResult {
    /// Path to the source file
    pub source_file: String,
    /// Number of learnings imported
    pub learnings_imported: usize,
    /// Number of learnings skipped as duplicates
    pub learnings_skipped: usize,
    /// Number of tags imported across all learnings
    pub tags_imported: usize,
    /// Whether stats were reset on import
    pub stats_reset: bool,
}

/// Import learnings from a JSON file.
///
/// Accepts either:
/// - A `ProgressExport` JSON object (has `learnings` array)
/// - A standalone `Vec<LearningExport>` JSON array
///
/// # Arguments
///
/// * `dir` - Directory containing the database
/// * `from_file` - Path to the JSON file to import
/// * `reset_stats` - If true, zero out bandit statistics on imported learnings
pub fn import_learnings(
    dir: &Path,
    from_file: &Path,
    reset_stats: bool,
) -> TaskMgrResult<ImportLearningsResult> {
    // Read and parse the input file
    let content = fs::read_to_string(from_file).map_err(|e| {
        TaskMgrError::io_error(from_file.display().to_string(), "reading import file", e)
    })?;

    let learnings = parse_learnings(&content)?;

    let mut conn = open_connection(dir)?;

    // Load existing keys BEFORE conn.transaction() to avoid mutable borrow conflict
    let mut seen = load_existing_keys(&conn)?;

    // Wrap all inserts in a transaction for atomicity
    let tx = conn.transaction()?;

    let mut imported = 0;
    let mut skipped = 0;
    let mut tags_imported = 0;

    for learning in &learnings {
        let key = compute_dedup_key(&learning.title, &learning.content);

        // seen.insert() returns false if key already present (existing DB entry or within-batch dup)
        if !seen.insert(key) {
            skipped += 1;
            continue;
        }

        let params = learning_to_params(learning);
        let result = record_learning(&tx, params)?;

        // Preserve stats from export when not resetting
        if !reset_stats {
            update_stats(&tx, result.learning_id, learning)?;
        }

        imported += 1;
        tags_imported += result.tags_added;
    }

    tx.commit()?;

    Ok(ImportLearningsResult {
        source_file: from_file.display().to_string(),
        learnings_imported: imported,
        learnings_skipped: skipped,
        tags_imported,
        stats_reset: reset_stats,
    })
}

/// Parse learnings from JSON content.
///
/// Tries ProgressExport first (object with `learnings` field),
/// then falls back to a standalone `Vec<LearningExport>` array.
fn parse_learnings(content: &str) -> TaskMgrResult<Vec<LearningExport>> {
    // Try ProgressExport format first
    if let Ok(progress) = serde_json::from_str::<ProgressExport>(content) {
        return Ok(progress.learnings);
    }

    // Try standalone Vec<LearningExport>
    if let Ok(learnings) = serde_json::from_str::<Vec<LearningExport>>(content) {
        return Ok(learnings);
    }

    Err(TaskMgrError::invalid_state(
        "import-learnings",
        "JSON format",
        "ProgressExport object or LearningExport array",
        "unrecognized JSON format",
    ))
}

/// Compute a dedup key based on title + content.
fn compute_dedup_key(title: &str, content: &str) -> String {
    format!("{}:{}", title, content)
}

/// Load dedup keys of all existing learnings in the database.
fn load_existing_keys(conn: &rusqlite::Connection) -> TaskMgrResult<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT title, content FROM learnings WHERE retired_at IS NULL")?;
    let rows = stmt.query_map([], |row| {
        let title: String = row.get(0)?;
        let content: String = row.get(1)?;
        Ok((title, content))
    })?;

    let mut keys = HashSet::new();
    for row in rows {
        let (title, content) = row?;
        keys.insert(compute_dedup_key(&title, &content));
    }

    Ok(keys)
}

/// Convert a LearningExport to RecordLearningParams.
///
/// Stats (times_shown, times_applied) always start at 0 on import since
/// `record_learning()` doesn't accept those fields. This is the expected
/// behavior: imported learnings start fresh in the new database's bandit.
fn learning_to_params(learning: &LearningExport) -> RecordLearningParams {
    RecordLearningParams {
        outcome: learning.outcome,
        title: learning.title.clone(),
        content: learning.content.clone(),
        task_id: None, // Don't carry over task_id to avoid FK violations
        run_id: None,  // Don't carry over run_id to avoid FK violations
        root_cause: learning.root_cause.clone(),
        solution: learning.solution.clone(),
        applies_to_files: learning.applies_to_files.clone(),
        applies_to_task_types: learning.applies_to_task_types.clone(),
        applies_to_errors: learning.applies_to_errors.clone(),
        tags: if learning.tags.is_empty() {
            None
        } else {
            Some(learning.tags.clone())
        },
        confidence: learning.confidence,
    }
}

/// Format a DateTime<Utc> as a SQLite-compatible string.
fn format_sqlite_datetime(dt: &DateTime<Utc>) -> String {
    dt.format(SQLITE_DATETIME_FMT).to_string()
}

/// Update bandit statistics for an imported learning.
///
/// Preserves times_shown, times_applied, last_shown_at, and last_applied_at
/// from the source export. Called only when reset_stats=false.
fn update_stats(
    conn: &rusqlite::Connection,
    learning_id: i64,
    learning: &LearningExport,
) -> TaskMgrResult<()> {
    let last_shown = learning.last_shown_at.as_ref().map(format_sqlite_datetime);
    let last_applied = learning
        .last_applied_at
        .as_ref()
        .map(format_sqlite_datetime);

    conn.execute(
        "UPDATE learnings SET times_shown = ?1, times_applied = ?2, \
         last_shown_at = ?3, last_applied_at = ?4 WHERE id = ?5",
        rusqlite::params![
            learning.times_shown,
            learning.times_applied,
            last_shown,
            last_applied,
            learning_id,
        ],
    )?;

    Ok(())
}

/// Format import learnings result for text output.
pub fn format_text(result: &ImportLearningsResult) -> String {
    let mut output = String::new();

    output.push_str(&format!("Imported from: {}\n", result.source_file));
    output.push_str(&format!(
        "Learnings imported: {}\n",
        result.learnings_imported
    ));

    if result.learnings_skipped > 0 {
        output.push_str(&format!(
            "Learnings skipped (duplicates): {}\n",
            result.learnings_skipped
        ));
    }

    if result.tags_imported > 0 {
        output.push_str(&format!("Tags imported: {}\n", result.tags_imported));
    }

    if result.stats_reset {
        output.push_str("Bandit statistics: reset to zero\n");
    } else if result.learnings_imported > 0 {
        output.push_str("Bandit statistics: preserved from export\n");
    }

    output
}

// ========== TEST-INIT-001: retired_at Filtering Tests ==========
// Inline tests (not in tests.rs) to access private `load_existing_keys`.
// #[ignore] until FEAT-001 and FEAT-002 are implemented.
//
// Query location covered:
//  14. Import dedup check (load_existing_keys: SELECT title, content FROM learnings)
#[cfg(test)]
mod retired_tests {
    use super::load_existing_keys;
    use crate::db::{create_schema, open_connection, run_migrations};
    use crate::learnings::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, rusqlite::Connection) {
        let dir = TempDir::new().expect("create temp dir");
        let mut conn = open_connection(dir.path()).expect("open connection");
        create_schema(&conn).expect("create schema");
        run_migrations(&mut conn).expect("run migrations");
        (dir, conn)
    }

    use crate::learnings::test_helpers::retire_learning;

    #[test]
    fn test_retired_excluded_from_import_dedup_load_existing_keys() {
        // AC: import dedup (load_existing_keys) must exclude retired learnings.
        // After retirement, reimporting the same title+content must succeed
        // (not be blocked by the dedup check).
        let (_dir, conn) = setup_test_db();

        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Import dedup target".to_string(),
            content: "Dedup content".to_string(),
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
        retire_learning(&conn, result.learning_id);

        let keys = load_existing_keys(&conn).unwrap();
        let dedup_key = format!("{}:{}", "Import dedup target", "Dedup content");

        assert!(
            !keys.contains(&dedup_key),
            "load_existing_keys must exclude retired learnings from dedup set; \
             key '{}' should not be present",
            dedup_key
        );
    }
}
