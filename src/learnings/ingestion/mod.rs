//! LLM-powered learning extraction from agent iteration output.
//!
//! Uses an auxiliary LLM pass to automatically extract structured learnings
//! from raw iteration output. Integrates with the existing CRUD system to
//! persist extracted learnings.
//!
//! ## Design
//!
//! - Best-effort: parse/spawn failures log warnings and return 0 learnings
//! - Primary-provider cost-efficient tier via `runner::dispatch`; on the
//!   built-in Claude config this intentionally uses Sonnet rather than Haiku
//! - Opt-out via `TASK_MGR_NO_EXTRACT_LEARNINGS=1`

pub mod extraction;

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::learnings::crud::{LearningWriter, RecordLearningParams};
use crate::learnings::embeddings::{NEAR_DUP_THRESHOLD, NearDupOutcome, NearDuplicateChecker};
use crate::learnings::retrieval::patterns::{resolve_task_context, type_prefix_from};
use crate::loop_engine::model::{ResolvedModelsConfig, cost_efficient_auxiliary_plan};
use crate::loop_engine::runner::dispatch_auxiliary;
use crate::loop_engine::signals::SignalFlag;
use crate::models::LearningOutcome;

/// Hard timeout for the extraction subprocess. Extraction is a one-shot
/// classification pass on ≤50KB of text; anything over ~5 min means the
/// spawned auxiliary LLM is stuck (rate limit, network stall, runaway internal
/// retry). Better to bail and move on than hang the whole loop.
const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

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

/// Extracts learnings from agent iteration output using LLM analysis.
///
/// Dispatches an auxiliary LLM with an extraction prompt, parses the JSON
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
/// * `db_dir` - Optional database directory for scheduling embeddings after recording.
///   Pass `Some(path)` in production paths to auto-embed learnings. Pass `None` in
///   tests or callers that don't need embeddings (writer no-ops, behavior matches pre-refactor).
/// * `resolved` - Resolved project model/routing config used to choose the
///   primary provider's cost-efficient auxiliary model.
pub fn extract_learnings_from_output(
    conn: &Connection,
    output: &str,
    task_id: Option<&str>,
    run_id: Option<&str>,
    db_dir: Option<&Path>,
    signal_flag: Option<&SignalFlag>,
    resolved: &ResolvedModelsConfig,
) -> TaskMgrResult<ExtractionResult> {
    if output.trim().is_empty() {
        return Ok(ExtractionResult::empty());
    }

    // Build extraction prompt
    let prompt = build_extraction_prompt(output, task_id);

    let plan = cost_efficient_auxiliary_plan(resolved);
    tracing::debug!(
        provider = ?plan.provider,
        model = plan.model.unwrap_or("<provider default>"),
        "spawning learning extraction auxiliary LLM"
    );

    // Bounded so a rate-limited or stalled extraction can't hang the whole
    // loop. The dispatcher keeps this text-only and omits effort flags.
    let auxiliary_result =
        match dispatch_auxiliary(plan, &prompt, EXTRACTION_TIMEOUT, db_dir, signal_flag) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("Warning: learning extraction spawn failed: {}", e);
                return Ok(ExtractionResult::empty());
            }
        };

    if auxiliary_result.timed_out {
        eprintln!(
            "Warning: learning extraction timed out after {}s — skipping",
            EXTRACTION_TIMEOUT.as_secs()
        );
        return Ok(ExtractionResult::empty());
    }
    if auxiliary_result.exit_code != 0 {
        eprintln!(
            "Warning: learning extraction auxiliary LLM exited with code {}",
            auxiliary_result.exit_code
        );
        return Ok(ExtractionResult::empty());
    }

    // Parse the extraction response
    let params_list = match parse_extraction_response(&auxiliary_result.output, task_id, run_id) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Warning: learning extraction parse failed: {}", e);
            return Ok(ExtractionResult::empty());
        }
    };

    if params_list.is_empty() {
        eprintln!(
            "Learning extraction: LLM returned 0 learnings (output len={})",
            output.len()
        );
        return Ok(ExtractionResult::empty());
    }

    // Enrich with task context (best-effort: fall back to unenriched params on error)
    let unenriched = params_list.clone();
    let params_list = enrich_extracted_params(conn, params_list, task_id).unwrap_or_else(|e| {
        eprintln!("Warning: learning enrichment failed: {}", e);
        unenriched
    });

    // Record each extracted learning, skipping duplicates. Tier-1 (exact
    // outcome+title match) always runs; Tier-2 (embedding near-duplicate) runs
    // only when the checker constructs — i.e. Ollama is up and `db_dir` is set.
    let mut writer = LearningWriter::new(db_dir);
    let mut checker = db_dir.and_then(|d| NearDuplicateChecker::new(conn, d, NEAR_DUP_THRESHOLD));
    let (learning_ids, deduped) = record_extracted_learnings(
        conn,
        &mut writer,
        params_list,
        checker.as_mut().map(|c| c as &mut dyn NearDupGuard),
    )?;

    if deduped > 0 {
        eprintln!("Learning extraction: {} duplicate(s) skipped", deduped);
    }

    // Flush AFTER the recording loop (and after any enclosing transaction would
    // have committed). This is where the Ollama embed HTTP calls happen; it must
    // never run inside a `rusqlite::Transaction` (learning #2174).
    writer.flush(conn);

    Ok(ExtractionResult {
        learnings_extracted: learning_ids.len(),
        learning_ids,
    })
}

/// Test seam for the Tier-2 semantic-dedup arms of [`record_extracted_learnings`].
///
/// `extract_learnings_from_output` dispatches the primary-provider auxiliary LLM
/// before the recording loop, so the loop is unreachable from a unit test without
/// a subprocess + live Ollama. Injecting the guard behind this trait lets a scripted fake exercise every
/// arm offline. The fake is the second impl that earns the abstraction, so this is
/// DI-for-testability (CLAUDE.md §5), not premature generalization.
trait NearDupGuard {
    fn check(&self, title: &str, content: &str) -> NearDupOutcome;
    fn register(&mut self, id: i64, embedding: Vec<f32>);
}

impl NearDupGuard for NearDuplicateChecker {
    fn check(&self, title: &str, content: &str) -> NearDupOutcome {
        NearDuplicateChecker::check(self, title, content)
    }

    fn register(&mut self, id: i64, embedding: Vec<f32>) {
        NearDuplicateChecker::register(self, id, embedding);
    }
}

/// Records each candidate, dropping duplicates via two tiers.
///
/// - **Tier-1** (`learning_exists`, exact outcome+title) runs FIRST and
///   UNCONDITIONALLY — it is the sole guard when `guard` is `None` (Ollama down /
///   tests) and keeps offline behavior byte-identical to before this feature.
/// - **Tier-2** (`guard.check`, embedding cosine) runs only when `guard` is `Some`.
///   On `Duplicate` the candidate is skipped (counted); on `Unique(emb)` it is
///   recorded and its embedding `register`ed so later same-batch candidates compare
///   against it; on `Unavailable` it falls through to the plain record path.
///
/// Asymmetric-risk bias: any uncertainty (guard absent, `Unavailable`) RECORDS —
/// a wrongly-dropped distinct learning is unrecoverable at write time, whereas a
/// leaked dupe is cleaned up later by `curate dedup`.
///
/// Returns `(learning_ids, deduped_count)`. The caller owns `flush` (after this
/// returns) so no transaction is held across the Ollama calls.
fn record_extracted_learnings(
    conn: &Connection,
    writer: &mut LearningWriter<'_>,
    params_list: Vec<RecordLearningParams>,
    mut guard: Option<&mut dyn NearDupGuard>,
) -> TaskMgrResult<(Vec<i64>, usize)> {
    let mut learning_ids = Vec::new();
    let mut deduped = 0usize;

    for params in params_list {
        // Tier-1: exact (outcome, title) match — always first, never gated.
        if learning_exists(conn, &params.outcome, &params.title)? {
            deduped += 1;
            continue;
        }

        // Tier-2: embedding near-duplicate. Capture the candidate embedding to
        // register only on a successful record — check() borrows `guard` immutably
        // and returns an OWNED outcome, so that borrow ends before record/register.
        let embedding_to_register = match guard.as_deref_mut() {
            Some(g) => match g.check(&params.title, &params.content) {
                NearDupOutcome::Duplicate {
                    existing_id,
                    similarity,
                } => {
                    deduped += 1;
                    eprintln!(
                        "Learning extraction: skipped near-duplicate of #{} (cos={:.3})",
                        existing_id, similarity
                    );
                    continue;
                }
                NearDupOutcome::Unique(emb) => Some(emb),
                // Uncertainty records, never skips (asymmetric-risk bias).
                NearDupOutcome::Unavailable => None,
            },
            None => None,
        };

        match writer.record(conn, params) {
            Ok(result) => {
                learning_ids.push(result.learning_id);
                // Re-borrow `guard` mutably now that check()'s borrow has ended.
                if let Some(emb) = embedding_to_register
                    && let Some(g) = guard.as_deref_mut()
                {
                    g.register(result.learning_id, emb);
                }
            }
            Err(e) => {
                eprintln!("Warning: failed to record extracted learning: {}", e);
            }
        }
    }

    Ok((learning_ids, deduped))
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
            if p.applies_to_task_types.is_none()
                && let Some(ref prefix) = type_prefix
            {
                p.applies_to_task_types = Some(vec![prefix.clone()]);
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
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::{NearDupGuard, enrich_extracted_params, record_extracted_learnings};
    use crate::learnings::crud::{LearningWriter, RecordLearningParams};
    use crate::learnings::embeddings::NearDupOutcome;
    use crate::learnings::test_helpers::{insert_task_with_files, setup_db};
    use crate::models::{Confidence, LearningOutcome};

    /// Scripted fake guard: returns pre-loaded `check` outcomes in order and
    /// records every `register` call. Exercises the Tier-2 arms with no Ollama.
    struct ScriptedGuard {
        scripted: RefCell<VecDeque<NearDupOutcome>>,
        registered: Vec<(i64, Vec<f32>)>,
    }

    impl ScriptedGuard {
        fn new(outcomes: Vec<NearDupOutcome>) -> Self {
            Self {
                scripted: RefCell::new(outcomes.into()),
                registered: Vec::new(),
            }
        }
    }

    impl NearDupGuard for ScriptedGuard {
        fn check(&self, _title: &str, _content: &str) -> NearDupOutcome {
            self.scripted
                .borrow_mut()
                .pop_front()
                .expect("ScriptedGuard: check() called more times than scripted")
        }

        fn register(&mut self, id: i64, embedding: Vec<f32>) {
            self.registered.push((id, embedding));
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

    // ─── Tests for record_extracted_learnings (FEAT-002, no Ollama) ──────────

    /// guard=None, two distinct titles -> both record (no dedup).
    #[test]
    fn test_record_no_guard_distinct_titles_both_recorded() {
        let (_dir, conn) = setup_db();
        let mut writer = LearningWriter::new(None);

        let params = vec![
            make_minimal_params("First distinct learning"),
            make_minimal_params("Second distinct learning"),
        ];
        let (ids, deduped) = record_extracted_learnings(&conn, &mut writer, params, None).unwrap();

        assert_eq!(ids.len(), 2, "both distinct learnings should record");
        assert_eq!(deduped, 0, "nothing should be deduped");
    }

    /// guard=None, identical (outcome, title) -> Tier-1 catches the second.
    /// Regression guard: exact-match dedup must fire with NO guard present.
    #[test]
    fn test_record_no_guard_exact_match_tier1_skips_second() {
        let (_dir, conn) = setup_db();
        let mut writer = LearningWriter::new(None);

        let params = vec![
            make_minimal_params("Same outcome and title"),
            make_minimal_params("Same outcome and title"),
        ];
        let (ids, deduped) = record_extracted_learnings(&conn, &mut writer, params, None).unwrap();

        assert_eq!(
            ids.len(),
            1,
            "second identical learning must be Tier-1 deduped"
        );
        assert_eq!(
            deduped, 1,
            "exact-match dedup must count even with no guard"
        );
    }

    /// Scripted fake: Duplicate then Unique. First is deduped (not recorded),
    /// second is recorded AND register() is invoked with (new id, embedding).
    /// Exercises both Tier-2 arms offline.
    #[test]
    fn test_record_scripted_guard_duplicate_then_unique() {
        let (_dir, conn) = setup_db();
        let mut writer = LearningWriter::new(None);

        let emb = vec![0.1_f32, 0.2, 0.3];
        let mut guard = ScriptedGuard::new(vec![
            NearDupOutcome::Duplicate {
                existing_id: 4242,
                similarity: 0.97,
            },
            NearDupOutcome::Unique(emb.clone()),
        ]);

        // Distinct titles so Tier-1 never short-circuits before Tier-2 runs.
        let params = vec![
            make_minimal_params("Semantic duplicate candidate"),
            make_minimal_params("Genuinely unique candidate"),
        ];
        let (ids, deduped) = record_extracted_learnings(
            &conn,
            &mut writer,
            params,
            Some(&mut guard as &mut dyn NearDupGuard),
        )
        .unwrap();

        assert_eq!(ids.len(), 1, "only the Unique candidate should record");
        assert_eq!(deduped, 1, "the Duplicate candidate should be deduped");
        assert_eq!(
            guard.registered.len(),
            1,
            "register() must be called exactly once, on the Unique record"
        );
        assert_eq!(
            guard.registered[0],
            (ids[0], emb),
            "register() must receive the freshly-recorded id and its embedding"
        );
    }

    /// Unavailable outcome records the candidate (never dropped) and does NOT
    /// register (no embedding available). Asymmetric-risk: uncertainty records.
    #[test]
    fn test_record_scripted_guard_unavailable_records_without_register() {
        let (_dir, conn) = setup_db();
        let mut writer = LearningWriter::new(None);

        let mut guard = ScriptedGuard::new(vec![NearDupOutcome::Unavailable]);
        let params = vec![make_minimal_params("Ollama-down candidate")];
        let (ids, deduped) = record_extracted_learnings(
            &conn,
            &mut writer,
            params,
            Some(&mut guard as &mut dyn NearDupGuard),
        )
        .unwrap();

        assert_eq!(ids.len(), 1, "Unavailable must record, never drop");
        assert_eq!(deduped, 0, "Unavailable is not a dedup");
        assert!(
            guard.registered.is_empty(),
            "no embedding to register when checker is Unavailable"
        );
    }

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
