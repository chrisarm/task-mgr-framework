//! LLM-powered learning extraction from agent iteration output.
//!
//! Uses Claude to automatically extract structured learnings from raw
//! iteration output. Integrates with the existing CRUD system to persist
//! extracted learnings.
//!
//! ## Design
//!
//! - Best-effort: parse/spawn failures log warnings and return 0 learnings
//! - Same model as loop: uses `spawn_claude()` from loop_engine
//! - Opt-out via `TASK_MGR_NO_EXTRACT_LEARNINGS=1`

pub mod extraction;

use rusqlite::Connection;

use crate::learnings::crud::{record_learning, RecordLearningParams};
use crate::learnings::retrieval::patterns::{resolve_task_context, type_prefix_from};
use crate::loop_engine::claude;
use crate::models::LearningOutcome;
use crate::TaskMgrResult;

pub use extraction::{build_extraction_prompt, parse_extraction_response};

/// Result of learning extraction from output.
#[derive(Debug)]
pub struct ExtractionResult {
    /// Number of learnings successfully extracted and stored
    pub learnings_extracted: usize,
    /// Database IDs of the created learnings
    pub learning_ids: Vec<i64>,
}

/// Returns true if extraction is disabled via environment variable.
pub fn is_extraction_disabled() -> bool {
    std::env::var("TASK_MGR_NO_EXTRACT_LEARNINGS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Extracts learnings from Claude's iteration output using LLM analysis.
///
/// Spawns a Claude subprocess with an extraction prompt, parses the JSON
/// response, and records each extracted learning via the CRUD system.
///
/// Best-effort: never crashes the loop. Returns 0 learnings on any error.
///
/// # Arguments
///
/// * `conn` - Database connection for storing extracted learnings
/// * `output` - Raw output from a Claude iteration
/// * `task_id` - Optional task ID for context
/// * `run_id` - Optional run ID for association
pub fn extract_learnings_from_output(
    conn: &Connection,
    output: &str,
    task_id: Option<&str>,
    run_id: Option<&str>,
) -> TaskMgrResult<ExtractionResult> {
    if output.trim().is_empty() {
        return Ok(ExtractionResult {
            learnings_extracted: 0,
            learning_ids: Vec::new(),
        });
    }

    // Build extraction prompt
    let prompt = build_extraction_prompt(output, task_id);

    // Spawn Claude for extraction
    let claude_result = match claude::spawn_claude(&prompt, None, None, None) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Warning: learning extraction spawn failed: {}", e);
            return Ok(ExtractionResult {
                learnings_extracted: 0,
                learning_ids: Vec::new(),
            });
        }
    };

    if claude_result.exit_code != 0 {
        eprintln!(
            "Warning: learning extraction Claude exited with code {}",
            claude_result.exit_code
        );
        return Ok(ExtractionResult {
            learnings_extracted: 0,
            learning_ids: Vec::new(),
        });
    }

    // Parse the extraction response
    let params_list = match parse_extraction_response(&claude_result.output, task_id, run_id) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Warning: learning extraction parse failed: {}", e);
            return Ok(ExtractionResult {
                learnings_extracted: 0,
                learning_ids: Vec::new(),
            });
        }
    };

    // Enrich with task context (best-effort: unenriched params used on error)
    let params_list = enrich_extracted_params(conn, params_list, task_id).unwrap_or_else(|e| {
        eprintln!("Warning: learning enrichment failed: {}", e);
        Vec::new()
    });

    // Record each extracted learning, skipping duplicates
    let mut learning_ids = Vec::new();
    for params in params_list {
        // Dedup by (outcome, title) — same title with different wording is likely a duplicate
        if learning_exists(conn, &params.outcome, &params.title)? {
            continue;
        }
        match record_learning(conn, params) {
            Ok(result) => {
                learning_ids.push(result.learning_id);
            }
            Err(e) => {
                eprintln!("Warning: failed to record extracted learning: {}", e);
            }
        }
    }

    Ok(ExtractionResult {
        learnings_extracted: learning_ids.len(),
        learning_ids,
    })
}

/// Enriches extracted learning params with task context when applicability is missing.
///
/// After LLM extraction, learnings may have no applicability metadata. This function:
/// - Fills `applies_to_files` from the task's `task_files` in the DB (when empty)
/// - Fills `applies_to_task_types` with the type prefix derived from `task_id` (when empty)
///
/// LLM-provided values are preserved (not overwritten).
/// If `task_id` is `None`, params are returned unchanged without error.
pub(crate) fn enrich_extracted_params(
    conn: &Connection,
    params: Vec<RecordLearningParams>,
    task_id: Option<&str>,
) -> TaskMgrResult<Vec<RecordLearningParams>> {
    let task_id = match task_id {
        Some(id) => id,
        None => return Ok(params),
    };

    // Resolve task context once, shared across all extracted learnings.
    // Graceful degradation: if context lookup fails, return params unchanged.
    let (task_files, task_prefix, _task_error) = match resolve_task_context(conn, task_id) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!(
                "Warning: task context lookup failed during enrichment: {}",
                e
            );
            return Ok(params);
        }
    };

    let type_prefix = task_prefix.as_deref().map(type_prefix_from);

    let enriched = params
        .into_iter()
        .map(|mut p| {
            // Preserve LLM-provided applies_to_files; fill from task context only when absent.
            if p.applies_to_files.is_none() && !task_files.is_empty() {
                p.applies_to_files = Some(task_files.clone());
            }
            // Preserve LLM-provided applies_to_task_types; derive from task prefix only when absent.
            if p.applies_to_task_types.is_none() {
                if let Some(ref prefix) = type_prefix {
                    p.applies_to_task_types = Some(vec![prefix.clone()]);
                }
            }
            p
        })
        .collect();

    Ok(enriched)
}

/// Checks whether a learning with the same outcome and title already exists.
fn learning_exists(
    conn: &Connection,
    outcome: &LearningOutcome,
    title: &str,
) -> TaskMgrResult<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM learnings WHERE outcome = ?1 AND title = ?2",
        rusqlite::params![outcome.as_db_str(), title],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::enrich_extracted_params;
    use crate::db::{create_schema, open_connection};
    use crate::learnings::crud::RecordLearningParams;
    use crate::models::{Confidence, LearningOutcome};

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    /// Inserts a task and associates file paths with it in task_files.
    fn insert_task_with_files(conn: &Connection, task_id: &str, files: &[&str]) {
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES (?1, 'Test Task')",
            [task_id],
        )
        .unwrap();
        for file in files {
            conn.execute(
                "INSERT INTO task_files (task_id, file_path) VALUES (?1, ?2)",
                rusqlite::params![task_id, file],
            )
            .unwrap();
        }
    }

    /// Creates a RecordLearningParams with all applicability fields empty.
    fn make_minimal_params(title: &str) -> RecordLearningParams {
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: title.to_string(),
            content: "Test content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        }
    }

    // ─── TDD: LLM extraction auto-populate tests (B2/FR-004) ─────────────────
    // All tests marked #[ignore] define expected enrichment behavior.
    // One active test verifies the no-task-id no-op (current stub behavior).
    // ─────────────────────────────────────────────────────────────────────────

    /// Active (not #[ignore]): when task_id is None, enrich_extracted_params
    /// returns params unchanged without error. This is both the stub behavior
    /// and the correct final behavior for the no-task-id case.
    #[test]
    fn test_enrich_no_task_id_returns_params_unchanged_no_error() {
        let (_dir, conn) = setup_db();

        let params = vec![make_minimal_params("No task id")];
        let result = enrich_extracted_params(&conn, params, None).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "No task id");
        assert!(
            result[0].applies_to_files.is_none(),
            "applies_to_files should remain None when no task_id"
        );
        assert!(
            result[0].applies_to_task_types.is_none(),
            "applies_to_task_types should remain None when no task_id"
        );
    }

    /// Happy path: extracted learning with no files gets task_files from context.
    #[test]
    fn test_enrich_populates_files_from_task_context() {
        let (_dir, conn) = setup_db();
        insert_task_with_files(
            &conn,
            "FEAT-003",
            &["src/learnings/ingestion/mod.rs", "src/lib.rs"],
        );

        let params = vec![make_minimal_params("No files learning")];
        let enriched = enrich_extracted_params(&conn, params, Some("FEAT-003")).unwrap();

        assert_eq!(enriched.len(), 1);
        let files = enriched[0]
            .applies_to_files
            .as_ref()
            .expect("applies_to_files should be populated from task context");
        assert!(
            files.contains(&"src/learnings/ingestion/mod.rs".to_string()),
            "Expected task file 'src/learnings/ingestion/mod.rs', got: {:?}",
            files
        );
        assert!(
            files.contains(&"src/lib.rs".to_string()),
            "Expected task file 'src/lib.rs', got: {:?}",
            files
        );
    }

    /// Happy path: extracted learning with no task_types gets type prefix from task_id.
    #[test]
    fn test_enrich_populates_task_types_from_task_prefix() {
        let (_dir, conn) = setup_db();
        insert_task_with_files(&conn, "FEAT-003", &[]);

        let params = vec![make_minimal_params("No task types learning")];
        let enriched = enrich_extracted_params(&conn, params, Some("FEAT-003")).unwrap();

        assert_eq!(enriched.len(), 1);
        let types = enriched[0]
            .applies_to_task_types
            .as_ref()
            .expect("applies_to_task_types should be populated from task_id prefix");
        assert!(
            types.iter().any(|t| t == "FEAT-"),
            "Expected type prefix 'FEAT-' derived from 'FEAT-003', got: {:?}",
            types
        );
        // Must be the prefix form, not the full task ID
        assert!(
            !types.iter().any(|t| t == "FEAT-003"),
            "Should store the type prefix 'FEAT-', not the full task id 'FEAT-003', got: {:?}",
            types
        );
    }

    /// Edge case: LLM-provided applies_to_files are preserved (not overwritten by task context).
    #[test]
    fn test_enrich_preserves_llm_provided_applies_to_files() {
        let (_dir, conn) = setup_db();
        // Task has different files from LLM-provided ones
        insert_task_with_files(&conn, "FEAT-003", &["src/task_file.rs"]);

        let mut params = make_minimal_params("LLM-provided files");
        params.applies_to_files = Some(vec!["src/llm_provided_file.rs".to_string()]);

        let enriched = enrich_extracted_params(&conn, vec![params], Some("FEAT-003")).unwrap();

        assert_eq!(enriched.len(), 1);
        let files = enriched[0]
            .applies_to_files
            .as_ref()
            .expect("applies_to_files should still be set");
        assert!(
            files.contains(&"src/llm_provided_file.rs".to_string()),
            "LLM-provided file should be preserved, got: {:?}",
            files
        );
        assert!(
            !files.contains(&"src/task_file.rs".to_string()),
            "Task file should NOT overwrite LLM-provided files, got: {:?}",
            files
        );
    }

    /// Known-bad discriminator: enrichment must use actual task_files from DB,
    /// not any hardcoded or default values. A stub returning ["src/main.rs"]
    /// will cause this test to fail.
    #[test]
    fn test_enrich_discriminator_uses_actual_db_files_not_hardcoded() {
        let (_dir, conn) = setup_db();
        // Use a file name that is distinctly NOT a common default like "src/main.rs"
        insert_task_with_files(&conn, "FEAT-003", &["src/ingestion_unique_9f3a2b.rs"]);

        let params = vec![make_minimal_params("Discriminator test")];
        let enriched = enrich_extracted_params(&conn, params, Some("FEAT-003")).unwrap();

        assert_eq!(enriched.len(), 1);
        let files = enriched[0]
            .applies_to_files
            .as_ref()
            .expect("applies_to_files should be populated for task with files");
        assert!(
            files.contains(&"src/ingestion_unique_9f3a2b.rs".to_string()),
            "Must contain FEAT-003's actual file from DB, got: {:?}",
            files
        );
        assert!(
            !files.contains(&"src/main.rs".to_string()),
            "Must NOT contain hardcoded 'src/main.rs'; query the DB, got: {:?}",
            files
        );
    }
}
