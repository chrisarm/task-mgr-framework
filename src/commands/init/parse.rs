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
    #[serde(default)]
    pub required_tests: Vec<String>,
    #[serde(default)]
    pub max_retries: Option<i32>,
    /// Whether the loop must pause after this task for human review.
    /// Maps to `tasks.requires_human` (INTEGER DEFAULT 0).
    #[serde(default)]
    pub requires_human: Option<bool>,
    /// Seconds to wait for human input before timing out (NULL = no timeout).
    /// Maps to `tasks.human_review_timeout` (INTEGER DEFAULT NULL).
    #[serde(default)]
    pub human_review_timeout: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_story(json: &str) -> PrdUserStory {
        serde_json::from_str(json).expect("should deserialize")
    }

    fn minimal_story(extra: &str) -> String {
        format!(
            r#"{{
                "id": "US-001",
                "title": "Test",
                "priority": 1,
                "passes": false
                {extra}
            }}"#
        )
    }

    // ---- requiresHuman deserialization ----

    #[test]
    fn test_prd_story_deserializes_requires_human_true() {
        let json = minimal_story(r#","requiresHuman": true"#);
        let story = parse_story(&json);
        assert_eq!(story.requires_human, Some(true));
    }

    #[test]
    fn test_prd_story_deserializes_requires_human_false() {
        let json = minimal_story(r#","requiresHuman": false"#);
        let story = parse_story(&json);
        assert_eq!(story.requires_human, Some(false));
    }

    #[test]
    fn test_prd_story_deserializes_requires_human_absent() {
        let json = minimal_story("");
        let story = parse_story(&json);
        assert_eq!(story.requires_human, None);
    }

    // ---- humanReviewTimeout deserialization ----

    #[test]
    fn test_prd_story_deserializes_human_review_timeout_set() {
        let json = minimal_story(r#","humanReviewTimeout": 60"#);
        let story = parse_story(&json);
        assert_eq!(story.human_review_timeout, Some(60));
    }

    #[test]
    fn test_prd_story_deserializes_human_review_timeout_absent() {
        let json = minimal_story("");
        let story = parse_story(&json);
        assert_eq!(story.human_review_timeout, None);
    }

    #[test]
    fn test_prd_story_deserializes_human_review_timeout_null() {
        let json = minimal_story(r#","humanReviewTimeout": null"#);
        let story = parse_story(&json);
        assert_eq!(story.human_review_timeout, None);
    }

    // ---- combined: both fields ----

    #[test]
    fn test_prd_story_deserializes_requires_human_with_timeout() {
        let json = minimal_story(r#","requiresHuman": true,"humanReviewTimeout": 120"#);
        let story = parse_story(&json);
        assert_eq!(story.requires_human, Some(true));
        assert_eq!(story.human_review_timeout, Some(120));
    }
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
    /// If absent and `--no-prefix` is not set, a deterministic hash is generated from
    /// the branch name and filename, then written back to the JSON file for stability.
    #[serde(default)]
    pub task_prefix: Option<String>,
    /// Path to the PRD markdown file (e.g. `prd-model-selection.md`).
    /// Stored in `prd_files` for archive file discovery.
    #[serde(default)]
    pub prd_file: Option<String>,
    /// Default model for all tasks in this PRD. Maps to `prd_metadata.default_model`
    /// in the database after import.
    #[serde(default)]
    pub model: Option<String>,
    /// Default max retries for all tasks in this PRD. Per-task maxRetries overrides this.
    /// Maps to `prd_metadata.default_max_retries` in the database after import.
    #[serde(default)]
    pub default_max_retries: Option<i32>,
}
