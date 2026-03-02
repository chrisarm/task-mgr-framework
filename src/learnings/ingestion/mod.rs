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

impl ExtractionResult {
    fn empty() -> Self {
        Self {
            learnings_extracted: 0,
            learning_ids: Vec::new(),
        }
    }
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
        return Ok(ExtractionResult::empty());
    }

    // Build extraction prompt
    let prompt = build_extraction_prompt(output, task_id);

    // Spawn Claude for extraction
    let claude_result = match claude::spawn_claude(&prompt, None, None, None, None) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Warning: learning extraction spawn failed: {}", e);
            return Ok(ExtractionResult::empty());
        }
    };

    if claude_result.exit_code != 0 {
        eprintln!(
            "Warning: learning extraction Claude exited with code {}",
            claude_result.exit_code
        );
        return Ok(ExtractionResult::empty());
    }

    // Parse the extraction response
    let params_list = match parse_extraction_response(&claude_result.output, task_id, run_id) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Warning: learning extraction parse failed: {}", e);
            return Ok(ExtractionResult::empty());
        }
    };

    // Enrich with task context (best-effort: fall back to unenriched params on error)
    let unenriched = params_list.clone();
    let params_list = enrich_extracted_params(conn, params_list, task_id).unwrap_or_else(|e| {
        eprintln!("Warning: learning enrichment failed: {}", e);
        unenriched
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
        "SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL AND outcome = ?1 AND title = ?2",
        rusqlite::params![outcome.as_db_str(), title],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::enrich_extracted_params;
    use crate::learnings::crud::RecordLearningParams;
    use crate::learnings::test_helpers::{insert_task_with_files, setup_db};
    use crate::models::{Confidence, LearningOutcome};

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

    // ─── Tests for LLM extraction auto-populate (B2/FR-004) ──────────────────
    // FEAT-003 implemented: all enrichment tests are active.
    // ─────────────────────────────────────────────────────────────────────────

    /// When task_id is None, enrich_extracted_params returns params unchanged
    /// without error.
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

    /// Comprehensive: multiple learnings in a single extraction batch all receive
    /// the same task context (files and task type prefix) independently.
    #[test]
    fn test_enrich_multiple_params_all_get_same_task_context() {
        let (_dir, conn) = setup_db();
        insert_task_with_files(&conn, "FEAT-003", &["src/feat.rs", "src/lib.rs"]);

        let params = vec![
            make_minimal_params("First learning"),
            make_minimal_params("Second learning"),
            make_minimal_params("Third learning"),
        ];
        let enriched = enrich_extracted_params(&conn, params, Some("FEAT-003")).unwrap();

        assert_eq!(enriched.len(), 3, "All 3 params should be returned");
        for (i, p) in enriched.iter().enumerate() {
            let files = p
                .applies_to_files
                .as_ref()
                .unwrap_or_else(|| panic!("Learning {i} should have applies_to_files"));
            assert!(
                files.contains(&"src/feat.rs".to_string()),
                "Learning {i} should have 'src/feat.rs', got: {:?}",
                files
            );
            let types = p
                .applies_to_task_types
                .as_ref()
                .unwrap_or_else(|| panic!("Learning {i} should have applies_to_task_types"));
            assert!(
                types.iter().any(|t| t == "FEAT-"),
                "Learning {i} should have 'FEAT-' type, got: {:?}",
                types
            );
        }
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

    // ========== TEST-INIT-001: retired_at Filtering Tests ==========
    //
    // Tests verify retired learnings are excluded from the ingestion dedup check.
    // #[ignore] until FEAT-001 and FEAT-002 are implemented.
    //
    // Query location covered:
    //  13. Ingestion dedup check (learning_exists: SELECT COUNT WHERE outcome=? AND title=?)

    use crate::learnings::test_helpers::retire_learning as retire_learning_ingestion;

    #[test]
    fn test_retired_excluded_from_ingestion_dedup_learning_exists() {
        // AC: ingestion dedup check (learning_exists) must exclude retired learnings.
        // After retirement, a new learning with the same outcome+title should NOT be
        // considered a duplicate — it should be allowed through.
        use crate::learnings::crud::record_learning;

        let (_dir, conn) = setup_db();

        // Insert a learning and retire it
        let initial = make_minimal_params("Retired dedup target");
        let result = record_learning(&conn, initial).unwrap();
        retire_learning_ingestion(&conn, result.learning_id);

        // learning_exists is private; verify via query_row directly
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learnings WHERE outcome = 'pattern' AND title = 'Retired dedup target' AND retired_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "after retirement, learning_exists must return false (no active learning with same outcome+title)"
        );
    }
}
