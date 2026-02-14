//! Type definitions for learnings CRUD operations.
//!
//! This module contains parameter and result structs for creating, reading,
//! updating, and deleting learnings.

use serde::{Deserialize, Serialize};

use crate::models::{Confidence, LearningOutcome};

/// Parameters for creating a new learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordLearningParams {
    /// Type of outcome this learning represents
    pub outcome: LearningOutcome,
    /// Short title summarizing the learning
    pub title: String,
    /// Detailed content of the learning
    pub content: String,
    /// Task ID this learning is associated with (optional)
    pub task_id: Option<String>,
    /// Run ID this learning is associated with (optional)
    pub run_id: Option<String>,
    /// Root cause analysis (for failures)
    pub root_cause: Option<String>,
    /// Solution that was applied
    pub solution: Option<String>,
    /// File patterns this learning applies to
    pub applies_to_files: Option<Vec<String>>,
    /// Task type prefixes this learning applies to
    pub applies_to_task_types: Option<Vec<String>>,
    /// Error patterns this learning applies to
    pub applies_to_errors: Option<Vec<String>>,
    /// Tags for categorization
    pub tags: Option<Vec<String>>,
    /// Confidence level for this learning
    pub confidence: Confidence,
}

/// Result of recording a learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordLearningResult {
    /// Database ID of the created learning
    pub learning_id: i64,
    /// Title of the learning
    pub title: String,
    /// Outcome type
    pub outcome: LearningOutcome,
    /// Number of tags added
    pub tags_added: usize,
}

/// Result of deleting a learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteLearningResult {
    /// ID of the deleted learning
    pub learning_id: i64,
    /// Title of the deleted learning
    pub title: String,
    /// Number of tags that were deleted (via cascade)
    pub tags_deleted: usize,
}

/// Parameters for editing an existing learning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EditLearningParams {
    /// New title (if Some, updates the title)
    pub title: Option<String>,
    /// New content (if Some, updates the content)
    pub content: Option<String>,
    /// New solution (if Some, updates the solution)
    pub solution: Option<String>,
    /// New root cause (if Some, updates the root cause)
    pub root_cause: Option<String>,
    /// New confidence level (if Some, updates confidence)
    pub confidence: Option<Confidence>,
    /// Tags to add
    pub add_tags: Option<Vec<String>>,
    /// Tags to remove
    pub remove_tags: Option<Vec<String>>,
    /// File patterns to add
    pub add_files: Option<Vec<String>>,
    /// File patterns to remove
    pub remove_files: Option<Vec<String>>,
}

impl EditLearningParams {
    /// Returns true if any field is set for update.
    pub fn has_updates(&self) -> bool {
        self.title.is_some()
            || self.content.is_some()
            || self.solution.is_some()
            || self.root_cause.is_some()
            || self.confidence.is_some()
            || self.add_tags.is_some()
            || self.remove_tags.is_some()
            || self.add_files.is_some()
            || self.remove_files.is_some()
    }
}

/// Result of editing a learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditLearningResult {
    /// ID of the edited learning
    pub learning_id: i64,
    /// Title of the learning (after edit)
    pub title: String,
    /// Fields that were updated
    pub updated_fields: Vec<String>,
    /// Tags that were added
    pub tags_added: usize,
    /// Tags that were removed
    pub tags_removed: usize,
}
