//! Output formatting for learnings CRUD operations.
//!
//! This module provides text formatting functions for CRUD operation results.

use super::types::{DeleteLearningResult, EditLearningResult};

/// Formats delete learning result as human-readable text.
pub fn format_delete_text(result: &DeleteLearningResult) -> String {
    format!(
        "Deleted learning #{}: {}\n",
        result.learning_id, result.title
    )
}

/// Formats edit learning result as human-readable text.
pub fn format_edit_text(result: &EditLearningResult) -> String {
    let mut output = format!(
        "Updated learning #{}: {}\n",
        result.learning_id, result.title
    );

    if result.updated_fields.is_empty() {
        output.push_str("  No fields were updated.\n");
    } else {
        output.push_str(&format!(
            "  Updated fields: {}\n",
            result.updated_fields.join(", ")
        ));
    }

    if result.tags_added > 0 {
        output.push_str(&format!("  Tags added: {}\n", result.tags_added));
    }
    if result.tags_removed > 0 {
        output.push_str(&format!("  Tags removed: {}\n", result.tags_removed));
    }

    output
}
