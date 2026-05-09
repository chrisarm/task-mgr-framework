//! JSON PRD parsing structures.
//!
//! This module contains the data structures used to deserialize PRD JSON files.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON structure for a user story in the PRD.
///
/// `Serialize` is implemented so `task-mgr add` can round-trip a task back
/// into the PRD JSON on disk. `skip_serializing_if` attributes mirror the
/// `#[serde(default)]` attributes on the `Deserialize` side so a task that
/// comes in minimal goes out minimal.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrdUserStory {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub priority: i32,
    pub passes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_scope: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_review: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synergy_with: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batch_with: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts_with: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Task effort level ("low", "medium", "high").
    /// Canonical JSON key is `estimatedEffort`; `difficulty` is also accepted
    /// for older PRDs. The internal Rust name is kept as `difficulty` for
    /// historical reasons (DB column, Task struct field).
    #[serde(
        default,
        rename = "estimatedEffort",
        alias = "difficulty",
        skip_serializing_if = "Option::is_none"
    )]
    pub difficulty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_tests: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i32>,
    /// Whether the loop must pause after this task for human review.
    /// Maps to `tasks.requires_human` (INTEGER DEFAULT 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_human: Option<bool>,
    /// Seconds to wait for human input before timing out (NULL = no timeout).
    /// Maps to `tasks.human_review_timeout` (INTEGER DEFAULT NULL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_review_timeout: Option<u32>,
    /// Per-task override for parallel-slot shared-infra slot claiming (FEAT-003).
    /// `Some(true)` forces the claim regardless of file paths or task-id prefix;
    /// `Some(false)` opts the task OUT of the buildy-prefix heuristic AND any
    /// implicit path-based detection; `None` (default) falls through to the
    /// implicit detection logic in `select_parallel_group`.
    /// Maps to `tasks.claims_shared_infra` (INTEGER DEFAULT NULL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims_shared_infra: Option<bool>,
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

    // ---- claimsSharedInfra (FEAT-003) deserialization ----

    #[test]
    fn test_prd_story_deserializes_claims_shared_infra_true() {
        let json = minimal_story(r#","claimsSharedInfra": true"#);
        let story = parse_story(&json);
        assert_eq!(story.claims_shared_infra, Some(true));
    }

    #[test]
    fn test_prd_story_deserializes_claims_shared_infra_false() {
        let json = minimal_story(r#","claimsSharedInfra": false"#);
        let story = parse_story(&json);
        assert_eq!(story.claims_shared_infra, Some(false));
    }

    #[test]
    fn test_prd_story_deserializes_claims_shared_infra_absent() {
        let json = minimal_story("");
        let story = parse_story(&json);
        assert_eq!(story.claims_shared_infra, None);
    }

    #[test]
    fn test_prd_file_implicit_overlap_files_absent_is_none() {
        let json = r#"{
            "project": "Test",
            "userStories": []
        }"#;
        let prd: PrdFile = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(prd.implicit_overlap_files, None);
    }

    #[test]
    fn test_prd_file_implicit_overlap_files_round_trips() {
        let json = r#"{
            "project": "Test",
            "userStories": [],
            "implicitOverlapFiles": ["custom.lock", "tofu.lock.hcl"]
        }"#;
        let prd: PrdFile = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(
            prd.implicit_overlap_files.as_deref(),
            Some(&["custom.lock".to_string(), "tofu.lock.hcl".to_string()][..]),
        );
    }

    /// Defense-in-depth: malformed `implicit_overlap_files` (non-string entries)
    /// is rejected at parse time with a clear serde error rather than silently
    /// degrading. The baseline + project-config still apply via the call-site
    /// merge, but this PRD's contribution is explicitly invalid.
    #[test]
    fn test_prd_file_implicit_overlap_files_malformed_entries_fail_parse() {
        let json = r#"{
            "project": "Test",
            "userStories": [],
            "implicitOverlapFiles": [42, true]
        }"#;
        let result: Result<PrdFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "non-string entries in implicitOverlapFiles must be rejected"
        );
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
    /// PRD-level override for the implicit shared-infra basename list (FEAT-003).
    /// Extends (does not replace) the baseline `IMPLICIT_OVERLAP_FILES` plus
    /// `ProjectConfig::implicit_overlap_files`. Match is by basename across
    /// any path in a task's `touchesFiles`.
    #[serde(default)]
    pub implicit_overlap_files: Option<Vec<String>>,
}
