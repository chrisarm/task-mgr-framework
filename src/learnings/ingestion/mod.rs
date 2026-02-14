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

use crate::learnings::crud::record_learning;
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
    let claude_result = match claude::spawn_claude(&prompt, None) {
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
                eprintln!(
                    "Warning: failed to record extracted learning: {}",
                    e
                );
            }
        }
    }

    Ok(ExtractionResult {
        learnings_extracted: learning_ids.len(),
        learning_ids,
    })
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
