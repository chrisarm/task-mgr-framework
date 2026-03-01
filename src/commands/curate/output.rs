//! Text output formatting for the `curate` subcommands.

use super::types::{RetireResult, UnretireResult};

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
