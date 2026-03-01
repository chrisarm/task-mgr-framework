//! Text output formatting for the `curate` subcommands.

use super::types::{EnrichResult, RetireResult, UnretireResult};

/// Format `curate retire` output as human-readable text.
pub fn format_retire_text(result: &RetireResult) -> String {
    if result.dry_run {
        if result.candidates_found == 0 {
            return "No retirement candidates found.".to_string();
        }
        let mut out = format!(
            "Dry run: {} retirement candidate(s) identified (no changes made):\n",
            result.candidates_found
        );
        for c in &result.candidates {
            out.push_str(&format!("  [{}] {} — {}\n", c.id, c.title, c.reason));
        }
        out
    } else {
        if result.learnings_retired == 0 {
            return "No learnings retired.".to_string();
        }
        let mut out = format!("Retired {} learning(s):\n", result.learnings_retired);
        for c in &result.candidates {
            out.push_str(&format!("  [{}] {}\n", c.id, c.title));
        }
        out
    }
}

/// Format `curate enrich` output as human-readable text.
///
/// Dry-run: lists each proposal with learning ID, title, and proposed metadata.
/// Non-dry-run: summary of enriched learnings, batches, and errors.
pub fn format_enrich_text(result: &EnrichResult) -> String {
    if result.dry_run {
        if result.proposals.is_empty() {
            return "Dry run: no enrichment candidates found.".to_string();
        }
        let mut out = format!(
            "Dry run: {} enrichment proposal(s) (no changes made):\n",
            result.proposals.len()
        );
        for p in &result.proposals {
            out.push_str(&format!("  [{}] {}\n", p.learning_id, p.learning_title));
            if !p.proposed_files.is_empty() {
                out.push_str(&format!("    files: {}\n", p.proposed_files.join(", ")));
            }
            if !p.proposed_task_types.is_empty() {
                out.push_str(&format!(
                    "    task_types: {}\n",
                    p.proposed_task_types.join(", ")
                ));
            }
            if !p.proposed_errors.is_empty() {
                out.push_str(&format!("    errors: {}\n", p.proposed_errors.join(", ")));
            }
        }
        out
    } else {
        if result.total_candidates == 0 {
            return "No enrichment candidates found.".to_string();
        }
        let mut out = format!(
            "Enriched {} learning(s) across {} batch(es).",
            result.learnings_enriched, result.batches_processed
        );
        if result.llm_errors > 0 {
            out.push_str(&format!(" {} LLM error(s) encountered.", result.llm_errors));
        }
        out.push('\n');
        out
    }
}

/// Format `curate unretire` output as human-readable text.
pub fn format_unretire_text(result: &UnretireResult) -> String {
    let mut out = String::new();
    if !result.restored.is_empty() {
        out.push_str(&format!(
            "Restored {} learning(s): {:?}\n",
            result.restored.len(),
            result.restored
        ));
    }
    for err in &result.errors {
        out.push_str(&format!("Error: {err}\n"));
    }
    if out.is_empty() {
        out.push_str("Nothing to unretire.\n");
    }
    out
}
