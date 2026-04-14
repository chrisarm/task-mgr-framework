//! Learn command implementation.
//!
//! Records learnings from task outcomes into the institutional memory system.

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::cli::{Confidence as CliConfidence, LearningOutcome as CliOutcome};
use crate::learnings::retrieval::patterns::{resolve_task_context, type_prefix_from};
use crate::learnings::{LearningWriter, RecordLearningParams, RecordLearningResult};
use crate::models::{Confidence, LearningOutcome};
use crate::TaskMgrResult;

/// Parameters for the learn command.
#[derive(Debug, Clone)]
pub struct LearnParams {
    /// Type of learning outcome
    pub outcome: CliOutcome,
    /// Short title for the learning
    pub title: String,
    /// Detailed content of the learning
    pub content: String,
    /// Task ID this learning is associated with
    pub task_id: Option<String>,
    /// Run ID this learning is associated with
    pub run_id: Option<String>,
    /// Root cause of the issue
    pub root_cause: Option<String>,
    /// Solution that was applied
    pub solution: Option<String>,
    /// File patterns this learning applies to
    pub files: Option<Vec<String>>,
    /// Task type prefixes this learning applies to
    pub task_types: Option<Vec<String>>,
    /// Error patterns this learning applies to
    pub errors: Option<Vec<String>>,
    /// Tags for categorization
    pub tags: Option<Vec<String>>,
    /// Confidence level for this learning
    pub confidence: CliConfidence,
}

/// Result of the learn command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnResult {
    /// Database ID of the created learning
    pub learning_id: i64,
    /// Title of the learning
    pub title: String,
    /// Outcome type
    pub outcome: String,
    /// Number of tags added
    pub tags_added: usize,
}

impl From<RecordLearningResult> for LearnResult {
    fn from(result: RecordLearningResult) -> Self {
        LearnResult {
            learning_id: result.learning_id,
            title: result.title,
            outcome: result.outcome.to_string(),
            tags_added: result.tags_added,
        }
    }
}

/// Converts CLI outcome enum to model outcome enum.
fn cli_outcome_to_model(outcome: CliOutcome) -> LearningOutcome {
    match outcome {
        CliOutcome::Failure => LearningOutcome::Failure,
        CliOutcome::Success => LearningOutcome::Success,
        CliOutcome::Workaround => LearningOutcome::Workaround,
        CliOutcome::Pattern => LearningOutcome::Pattern,
    }
}

/// Converts CLI confidence enum to model confidence enum.
fn cli_confidence_to_model(confidence: CliConfidence) -> Confidence {
    match confidence {
        CliConfidence::High => Confidence::High,
        CliConfidence::Medium => Confidence::Medium,
        CliConfidence::Low => Confidence::Low,
    }
}

/// Records a learning from CLI parameters.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `db_dir` - Project directory for embedding scheduling; `None` skips embedding (tests)
/// * `params` - Learn command parameters from CLI
///
/// # Returns
///
/// Result containing the learning ID and metadata.
///
/// # Errors
///
/// Returns an error if:
/// - Database insert fails
/// - Task ID doesn't exist (foreign key violation)
/// - Run ID doesn't exist (foreign key violation)
pub fn learn(
    conn: &Connection,
    db_dir: Option<&Path>,
    params: LearnParams,
) -> TaskMgrResult<LearnResult> {
    // Auto-populate applies_to_files and applies_to_task_types from task context
    // when task_id is provided and --files/--task-types are not explicitly set.
    let (effective_files, effective_task_types) = if let Some(ref task_id) = params.task_id {
        let files_are_explicit = params.files.as_ref().is_some_and(|f| !f.is_empty());
        let types_are_explicit = params.task_types.as_ref().is_some_and(|t| !t.is_empty());

        match resolve_task_context(conn, task_id) {
            Ok((task_files, task_prefix, _task_error)) => {
                let files = if files_are_explicit {
                    params.files.clone()
                } else if task_files.is_empty() {
                    None
                } else {
                    Some(task_files)
                };

                let types = if types_are_explicit {
                    params.task_types.clone()
                } else {
                    task_prefix.map(|p| vec![type_prefix_from(&p)])
                };

                (files, types)
            }
            // Graceful degradation: context lookup failed, use explicit values only.
            Err(_) => (
                params.files.clone().filter(|f| !f.is_empty()),
                params.task_types.clone().filter(|t| !t.is_empty()),
            ),
        }
    } else {
        // No task_id — no auto-populate.
        (params.files.clone(), params.task_types.clone())
    };

    let record_params = RecordLearningParams {
        outcome: cli_outcome_to_model(params.outcome),
        title: params.title,
        content: params.content,
        task_id: params.task_id,
        run_id: params.run_id,
        root_cause: params.root_cause,
        solution: params.solution,
        applies_to_files: effective_files,
        applies_to_task_types: effective_task_types,
        applies_to_errors: params.errors,
        tags: params.tags,
        confidence: cli_confidence_to_model(params.confidence),
    };

    let mut writer = LearningWriter::new(db_dir);
    let result = writer.record(conn, record_params)?;
    writer.flush(conn);
    Ok(LearnResult::from(result))
}

/// Formats the learn result for text output.
pub fn format_text(result: &LearnResult) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Learning recorded: {}", result.title));
    lines.push(format!("  ID: {}", result.learning_id));
    lines.push(format!("  Outcome: {}", result.outcome));
    if result.tags_added > 0 {
        lines.push(format!("  Tags added: {}", result.tags_added));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learnings::test_helpers::{insert_task_with_files, setup_db};

    /// Reads applies_to_files (raw JSON text) for a learning from the DB.
    fn get_applies_to_files(conn: &Connection, learning_id: i64) -> Option<String> {
        conn.query_row(
            "SELECT applies_to_files FROM learnings WHERE id = ?1",
            [learning_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// Reads applies_to_task_types (raw JSON text) for a learning from the DB.
    fn get_applies_to_task_types(conn: &Connection, learning_id: i64) -> Option<String> {
        conn.query_row(
            "SELECT applies_to_task_types FROM learnings WHERE id = ?1",
            [learning_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_learn_minimal() {
        let (_temp_dir, conn) = setup_db();

        let params = LearnParams {
            outcome: CliOutcome::Failure,
            title: "Test failure".to_string(),
            content: "Something went wrong".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();

        assert!(result.learning_id > 0);
        assert_eq!(result.title, "Test failure");
        assert_eq!(result.outcome, "failure");
        assert_eq!(result.tags_added, 0);
    }

    #[test]
    fn test_learn_with_all_options() {
        let (_temp_dir, conn) = setup_db();

        // Create task and run for foreign key references
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
            .unwrap();

        let params = LearnParams {
            outcome: CliOutcome::Success,
            title: "Successful pattern".to_string(),
            content: "This worked well".to_string(),
            task_id: Some("US-001".to_string()),
            run_id: Some("run-001".to_string()),
            root_cause: Some("Root cause".to_string()),
            solution: Some("Applied fix".to_string()),
            files: Some(vec!["src/*.rs".to_string()]),
            task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
            errors: Some(vec!["E0001".to_string()]),
            tags: Some(vec!["rust".to_string(), "database".to_string()]),
            confidence: CliConfidence::High,
        };

        let result = learn(&conn, None, params).unwrap();

        assert!(result.learning_id > 0);
        assert_eq!(result.title, "Successful pattern");
        assert_eq!(result.outcome, "success");
        assert_eq!(result.tags_added, 2);
    }

    #[test]
    fn test_learn_all_outcomes() {
        let (_temp_dir, conn) = setup_db();

        let outcomes = [
            (CliOutcome::Failure, "failure"),
            (CliOutcome::Success, "success"),
            (CliOutcome::Workaround, "workaround"),
            (CliOutcome::Pattern, "pattern"),
        ];

        for (cli_outcome, expected_str) in outcomes {
            let params = LearnParams {
                outcome: cli_outcome,
                title: format!("{} test", expected_str),
                content: "Content".to_string(),
                task_id: None,
                run_id: None,
                root_cause: None,
                solution: None,
                files: None,
                task_types: None,
                errors: None,
                tags: None,
                confidence: CliConfidence::Medium,
            };

            let result = learn(&conn, None, params).unwrap();
            assert_eq!(result.outcome, expected_str);
        }
    }

    #[test]
    fn test_learn_invalid_task_id() {
        let (_temp_dir, conn) = setup_db();

        let params = LearnParams {
            outcome: CliOutcome::Failure,
            title: "Test".to_string(),
            content: "Content".to_string(),
            task_id: Some("NONEXISTENT".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params);
        assert!(result.is_err(), "Should fail with invalid task_id");
    }

    #[test]
    fn test_format_text_basic() {
        let result = LearnResult {
            learning_id: 42,
            title: "Test learning".to_string(),
            outcome: "failure".to_string(),
            tags_added: 0,
        };

        let text = format_text(&result);
        assert!(text.contains("Learning recorded: Test learning"));
        assert!(text.contains("ID: 42"));
        assert!(text.contains("Outcome: failure"));
        assert!(!text.contains("Tags added"));
    }

    #[test]
    fn test_format_text_with_tags() {
        let result = LearnResult {
            learning_id: 1,
            title: "Tagged learning".to_string(),
            outcome: "pattern".to_string(),
            tags_added: 3,
        };

        let text = format_text(&result);
        assert!(text.contains("Tags added: 3"));
    }

    #[test]
    fn test_cli_outcome_to_model() {
        assert_eq!(
            cli_outcome_to_model(CliOutcome::Failure),
            LearningOutcome::Failure
        );
        assert_eq!(
            cli_outcome_to_model(CliOutcome::Success),
            LearningOutcome::Success
        );
        assert_eq!(
            cli_outcome_to_model(CliOutcome::Workaround),
            LearningOutcome::Workaround
        );
        assert_eq!(
            cli_outcome_to_model(CliOutcome::Pattern),
            LearningOutcome::Pattern
        );
    }

    #[test]
    fn test_cli_confidence_to_model() {
        assert_eq!(
            cli_confidence_to_model(CliConfidence::High),
            Confidence::High
        );
        assert_eq!(
            cli_confidence_to_model(CliConfidence::Medium),
            Confidence::Medium
        );
        assert_eq!(cli_confidence_to_model(CliConfidence::Low), Confidence::Low);
    }

    // ─── Tests for auto-populate on learn command (FEAT-002) ─────────────────
    // FEAT-002 implemented: all tests are active.
    // When task_id is provided without explicit --files/--task-types,
    // learn() auto-fills from task context via resolve_task_context().

    /// Regression test: without task_id, no auto-populate occurs.
    #[test]
    fn test_learn_without_task_id_does_not_auto_populate() {
        let (_dir, conn) = setup_db();

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "No auto-populate".to_string(),
            content: "Content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();

        assert!(
            get_applies_to_files(&conn, result.learning_id).is_none(),
            "applies_to_files should be NULL when no task_id provided"
        );
        assert!(
            get_applies_to_task_types(&conn, result.learning_id).is_none(),
            "applies_to_task_types should be NULL when no task_id provided"
        );
    }

    /// Happy path: learn() with task_id auto-populates applies_to_files
    /// from the task's associated files in the task_files table.
    #[test]
    fn test_learn_auto_populates_files_from_task_files() {
        let (_dir, conn) = setup_db();
        insert_task_with_files(&conn, "FEAT-003", &["src/commands/learn.rs", "src/lib.rs"]);

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Auto-populate files".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None, // no explicit --files
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();
        let files_json = get_applies_to_files(&conn, result.learning_id);

        assert!(
            files_json.is_some(),
            "applies_to_files should be populated when task_id is provided"
        );
        let files_json = files_json.unwrap();
        assert!(
            files_json.contains("src/commands/learn.rs"),
            "Expected task file 'src/commands/learn.rs' in applies_to_files, got: {files_json}"
        );
        assert!(
            files_json.contains("src/lib.rs"),
            "Expected task file 'src/lib.rs' in applies_to_files, got: {files_json}"
        );
    }

    /// Happy path: learn() with task_id auto-populates applies_to_task_types
    /// with the type prefix extracted from the task ID (e.g., "FEAT-" from "FEAT-003").
    #[test]
    fn test_learn_auto_populates_task_types_from_prefix() {
        let (_dir, conn) = setup_db();
        insert_task_with_files(&conn, "FEAT-003", &[]);

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Auto-populate task types".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None, // no explicit --task-types
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();
        let types_json = get_applies_to_task_types(&conn, result.learning_id);

        assert!(
            types_json.is_some(),
            "applies_to_task_types should be populated when task_id is provided"
        );
        // The stored value should contain "FEAT-" as a type prefix, not the full task ID "FEAT-003".
        // This ensures future FEAT-xxx tasks will match via starts_with() scoring.
        let types_json = types_json.unwrap();
        assert!(
            types_json.contains("\"FEAT-\""),
            "Expected type prefix 'FEAT-' in applies_to_task_types, got: {types_json}"
        );
    }

    /// Edge case: explicit --files flag overrides auto-population from task context.
    #[test]
    fn test_learn_explicit_files_override_auto_populate() {
        let (_dir, conn) = setup_db();
        insert_task_with_files(&conn, "FEAT-003", &["src/other.rs"]);

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Explicit files override".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: Some(vec!["src/explicit.rs".to_string()]),
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();
        let files_json = get_applies_to_files(&conn, result.learning_id)
            .expect("applies_to_files should be set");

        assert!(
            files_json.contains("src/explicit.rs"),
            "Explicit file should be stored, got: {files_json}"
        );
        assert!(
            !files_json.contains("src/other.rs"),
            "Task file should NOT override explicit files, got: {files_json}"
        );
    }

    /// Edge case: explicit --task-types flag overrides auto-population from task prefix.
    #[test]
    fn test_learn_explicit_task_types_override_auto_populate() {
        let (_dir, conn) = setup_db();
        insert_task_with_files(&conn, "FEAT-003", &[]);

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Explicit task types override".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();
        let types_json = get_applies_to_task_types(&conn, result.learning_id)
            .expect("applies_to_task_types should be set");

        assert!(
            types_json.contains("\"US-\""),
            "Explicit type 'US-' should be stored, got: {types_json}"
        );
        assert!(
            types_json.contains("\"FIX-\""),
            "Explicit type 'FIX-' should be stored, got: {types_json}"
        );
        assert!(
            !types_json.contains("FEAT-"),
            "Auto-derived 'FEAT-' should NOT appear when types are explicit, got: {types_json}"
        );
    }

    /// Edge case: task with no task_files skips file auto-populate,
    /// but still auto-populates task type prefix.
    #[test]
    fn test_learn_no_task_files_still_populates_task_type() {
        let (_dir, conn) = setup_db();
        // Task exists but has no associated files
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES ('FEAT-003', 'Task with no files')",
            [],
        )
        .unwrap();

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "No task files".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();

        assert!(
            get_applies_to_files(&conn, result.learning_id).is_none(),
            "applies_to_files should be NULL when task has no associated files"
        );
        let types_json = get_applies_to_task_types(&conn, result.learning_id);
        assert!(
            types_json.is_some(),
            "applies_to_task_types should still be populated even with no task files"
        );
        assert!(
            types_json.unwrap().contains("\"FEAT-\""),
            "Type prefix 'FEAT-' should be derived from task ID even when no files exist"
        );
    }

    /// Edge case: empty files vec Some([]) is treated the same as None —
    /// auto-populate still runs, and NULL is stored rather than an empty JSON array.
    #[test]
    fn test_learn_empty_files_vec_treated_as_null_for_auto_populate() {
        let (_dir, conn) = setup_db();
        insert_task_with_files(&conn, "FEAT-003", &["src/feat.rs"]);

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Empty files vec".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: Some(vec![]), // empty — treated same as None
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();
        let files_json = get_applies_to_files(&conn, result.learning_id);

        // Auto-populate should have run (treating [] as not-explicitly-set),
        // so we get the task's actual files, not an empty array.
        assert!(
            files_json.is_some(),
            "Auto-populate should run when files=Some([]) (treated as None)"
        );
        let files_json = files_json.unwrap();
        assert!(
            files_json.contains("src/feat.rs"),
            "Task file 'src/feat.rs' should be auto-populated, got: {files_json}"
        );
        assert!(
            files_json != "[]",
            "Empty JSON array should not be stored; auto-populate should have filled it"
        );
    }

    /// Error path: when task_id references a non-existent task, learn() returns
    /// a FK constraint error. The auto-populate step itself (resolve_task_context)
    /// succeeds with empty files, but the record_learning insert fails due to the
    /// foreign key constraint between learnings.task_id and tasks.id.
    #[test]
    fn test_learn_task_not_in_db_returns_fk_error() {
        let (_dir, conn) = setup_db();
        // Intentionally do NOT insert the task — expect FK error from record_learning

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Non-existent task".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-999".to_string()), // not in DB
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        // FK constraint: learnings.task_id must reference tasks.id
        let result = learn(&conn, None, params);
        assert!(
            result.is_err(),
            "learn() should fail with FK violation when task_id not in DB"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("FOREIGN KEY"),
            "Error should mention FOREIGN KEY constraint, got: {err_msg}"
        );
    }

    /// Comprehensive: UUID-prefixed task IDs strip the prefix when deriving
    /// applies_to_task_types. "f424ade5-FEAT-003" → type prefix "FEAT-".
    #[test]
    fn test_learn_uuid_prefixed_task_id_derives_type_prefix() {
        let (_dir, conn) = setup_db();
        let uuid_task_id = "f424ade5-FEAT-003";
        insert_task_with_files(&conn, uuid_task_id, &["src/feat.rs"]);

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "UUID-prefixed task".to_string(),
            content: "Content".to_string(),
            task_id: Some(uuid_task_id.to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();
        let types_json = get_applies_to_task_types(&conn, result.learning_id)
            .expect("applies_to_task_types should be populated");

        // UUID prefix stripped → "FEAT-003" → type prefix "FEAT-"
        assert!(
            types_json.contains("\"FEAT-\""),
            "Expected type prefix 'FEAT-' after stripping UUID prefix, got: {types_json}"
        );
        assert!(
            !types_json.contains("f424ade5"),
            "UUID prefix should NOT appear in stored type, got: {types_json}"
        );
    }

    /// Boundary: empty string task_id is treated as Some(""), which auto-populate
    /// processes (resolve_task_context returns empty files). The record_learning call
    /// fails with FK error since "" is not a valid task id in the tasks table.
    /// This test verifies no panic occurs — only a clean error.
    #[test]
    fn test_learn_empty_string_task_id_returns_fk_error() {
        let (_dir, conn) = setup_db();

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Empty task_id".to_string(),
            content: "Content".to_string(),
            task_id: Some(String::new()), // empty string — not a valid task id
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        // No panic — clean error from FK constraint
        let result = learn(&conn, None, params);
        assert!(
            result.is_err(),
            "Empty task_id should return FK error, not panic"
        );
        // Error should be a FK violation, not an index-out-of-bounds or slice error
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("FOREIGN KEY"),
            "Error should be FK violation, got: {err_msg}"
        );
    }

    /// AC #1: resolve_task_context() failure (e.g., missing table) does NOT prevent
    /// learning creation. Graceful degradation: files/types fall back to explicit params.
    #[test]
    fn test_learn_resolve_task_context_failure_graceful_degradation() {
        let (_dir, conn) = setup_db();
        // Insert task so FK constraint on learnings.task_id is satisfied
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES ('FEAT-003', 'Test task')",
            [],
        )
        .unwrap();
        // Drop task_files table — causes resolve_task_context to return Err
        conn.execute("DROP TABLE IF EXISTS task_files", []).unwrap();

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Graceful degradation".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,      // no explicit files
            task_types: None, // no explicit task types
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        // Even though resolve_task_context errors (missing table),
        // learn() must still create the learning without panic.
        let result = learn(&conn, None, params);
        assert!(
            result.is_ok(),
            "Learning creation must succeed even when resolve_task_context fails, got: {:?}",
            result.err()
        );
        let res = result.unwrap();
        // With no explicit values and context failure, both fields should be NULL
        assert!(
            get_applies_to_files(&conn, res.learning_id).is_none(),
            "applies_to_files should be NULL when graceful degradation path taken"
        );
        assert!(
            get_applies_to_task_types(&conn, res.learning_id).is_none(),
            "applies_to_task_types should be NULL when graceful degradation path taken"
        );
    }

    /// AC #5: Task with files but non-standard task_id prefix still populates
    /// applies_to_files. File enrichment is independent of type-prefix extraction.
    #[test]
    fn test_learn_task_with_files_no_standard_prefix_still_populates_files() {
        let (_dir, conn) = setup_db();
        // Use a non-standard task ID that doesn't follow FEAT-/FIX-/etc. convention
        insert_task_with_files(&conn, "custom-task-id", &["src/custom_module.rs"]);

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Non-standard prefix task".to_string(),
            content: "Content".to_string(),
            task_id: Some("custom-task-id".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();
        let files_json = get_applies_to_files(&conn, result.learning_id);

        // Files should be populated regardless of whether the prefix is standard
        assert!(
            files_json.is_some(),
            "applies_to_files should be populated even when task_id has non-standard format"
        );
        assert!(
            files_json.unwrap().contains("src/custom_module.rs"),
            "Task files should be auto-populated independently of type-prefix extraction"
        );
    }

    /// Known-bad discriminator: learn() with task_id='FEAT-003' must use the
    /// actual task_files from the DB, not any hardcoded or default value.
    /// A stub that always returns ["src/main.rs"] will cause this test to fail.
    #[test]
    fn test_learn_discriminator_uses_actual_db_files_not_hardcoded() {
        let (_dir, conn) = setup_db();
        // Use a file that is distinctly NOT "src/main.rs"
        insert_task_with_files(&conn, "FEAT-003", &["src/commands/feat003_unique_file.rs"]);

        let params = LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Discriminator test".to_string(),
            content: "Content".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, None, params).unwrap();
        let files_json = get_applies_to_files(&conn, result.learning_id)
            .expect("applies_to_files should be populated for task with files");

        assert!(
            files_json.contains("src/commands/feat003_unique_file.rs"),
            "Should contain FEAT-003's actual file from DB, got: {files_json}"
        );
        assert!(
            !files_json.contains("src/main.rs"),
            "Should NOT contain hardcoded 'src/main.rs'; implementation must query the DB, got: {files_json}"
        );
    }
}
