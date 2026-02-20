//! JSON PRD parsing structures.
//!
//! This module contains the data structures used to deserialize PRD JSON files.

use serde::Deserialize;
use serde_json::Value;

/// JSON structure for a user story in the PRD.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrdUserStory {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: i32,
    pub passes: bool,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub review_scope: Option<Value>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub source_review: Option<String>,
    #[serde(default)]
    pub touches_files: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub synergy_with: Vec<String>,
    #[serde(default)]
    pub batch_with: Vec<String>,
    #[serde(default)]
    pub conflicts_with: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub difficulty: Option<String>,
    #[serde(default)]
    pub escalation_note: Option<String>,
}

/// JSON structure for the PRD file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrdFile {
    pub project: String,
    #[serde(default)]
    pub branch_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority_philosophy: Option<Value>,
    #[serde(default)]
    pub global_acceptance_criteria: Option<Value>,
    #[serde(default)]
    pub review_guidelines: Option<Value>,
    pub user_stories: Vec<PrdUserStory>,
    #[serde(default)]
    pub external_git_repo: Option<String>,
    /// Prefix applied to all task IDs during import to prevent cross-phase collisions.
    /// If absent and `--no-prefix` is not set, a short UUID is auto-generated and
    /// written back to the JSON file for stability across re-imports.
    #[serde(default)]
    pub task_prefix: Option<String>,
    /// Path to the PRD markdown file (e.g. `prd-model-selection.md`).
    /// Stored in `prd_files` for archive file discovery.
    #[serde(default)]
    pub prd_file: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}
