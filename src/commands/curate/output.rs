//! Text output formatting for the `curate` subcommands.

use super::types::{
    CountResult, DedupResult, EmbedResult, EnrichResult, RetireResult, UnretireResult,
};

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

/// Format `curate dedup` output as human-readable text.
pub fn format_dedup_text(result: &DedupResult) -> String {
    let dry_run_marker = if result.dry_run {
        " — DRY RUN — no changes made"
    } else {
        ""
    };

    let mut out = format!(
        "{} cluster(s), {} learning(s) merged, {} created{}\n",
        result.clusters_found, result.learnings_merged, result.learnings_created, dry_run_marker
    );

    if result.clusters_skipped > 0 {
        out.push_str(&format!(
            "{} cluster(s) skipped (all pairs previously dismissed)\n",
            result.clusters_skipped
        ));
    }

    if result.llm_errors > 0 {
        out.push_str(&format!("{} LLM error(s) encountered\n", result.llm_errors));
    }

    for cluster in &result.clusters {
        out.push_str(&format!("  Cluster: {}\n", cluster.merged_title));
        out.push_str(&format!("    Reason: {}\n", cluster.reason));
        for title in &cluster.source_titles {
            out.push_str(&format!("    - {title}\n"));
        }
        if let Some(id) = cluster.merged_learning_id {
            out.push_str(&format!("    -> merged learning id: {id}\n"));
        }
    }

    out
}

/// Format `curate embed` output as human-readable text.
pub fn format_embed_text(result: &EmbedResult) -> String {
    if result.status_only {
        return format!(
            "Embeddings: {}/{} active learnings embedded (model: {})\n",
            result.already_embedded, result.total_active, result.model
        );
    }

    let mut out = format!("Embedded {} learning(s)", result.embedded_this_run);
    if result.skipped_empty > 0 {
        out.push_str(&format!(
            ", {} skipped (empty content)",
            result.skipped_empty
        ));
    }
    if result.errors > 0 {
        out.push_str(&format!(", {} error(s)", result.errors));
    }
    out.push_str(".\n");
    out
}

/// Format `curate count` output as human-readable text.
pub fn format_count_text(result: &CountResult) -> String {
    format!(
        "Total: {}\nActive: {}\nRetired: {}\nEmbedded: {}\n",
        result.total, result.active, result.retired, result.embedded
    )
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
