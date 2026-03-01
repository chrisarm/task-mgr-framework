//! Types for the `curate` subcommands.

use serde::{Deserialize, Serialize};

/// A learning identified as a retirement candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetirementCandidate {
    /// Learning ID
    pub id: i64,
    /// Learning title
    pub title: String,
    /// Human-readable reason why this learning is a candidate
    pub reason: String,
}

/// Result of the `curate retire` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetireResult {
    /// Whether this was a dry run (no DB changes made)
    pub dry_run: bool,
    /// Number of candidates identified
    pub candidates_found: usize,
    /// Number of learnings actually retired (0 when dry_run=true)
    pub learnings_retired: usize,
    /// The candidate learnings
    pub candidates: Vec<RetirementCandidate>,
}

/// Result of the `curate unretire` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnretireResult {
    /// IDs successfully restored to active status
    pub restored: Vec<i64>,
    /// Per-ID error messages for IDs that could not be unretired
    pub errors: Vec<String>,
}

/// Validated field filter for `curate enrich --field`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichFieldFilter {
    AppliesToFiles,
    AppliesToTaskTypes,
    AppliesToErrors,
}

impl std::str::FromStr for EnrichFieldFilter {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "applies_to_files" => Ok(Self::AppliesToFiles),
            "applies_to_task_types" => Ok(Self::AppliesToTaskTypes),
            "applies_to_errors" => Ok(Self::AppliesToErrors),
            other => Err(format!(
                "unknown field '{}': expected one of applies_to_files, applies_to_task_types, applies_to_errors",
                other
            )),
        }
    }
}

/// Parameters for the `curate enrich` command.
#[derive(Debug, Clone)]
pub struct EnrichParams {
    /// If true, generate proposals but do not write to the database
    pub dry_run: bool,
    /// Number of learnings per LLM batch
    pub batch_size: usize,
    /// Restrict enrichment to a specific metadata field (None = all fields)
    pub field_filter: Option<EnrichFieldFilter>,
}

impl Default for EnrichParams {
    fn default() -> Self {
        Self {
            dry_run: false,
            batch_size: 20,
            field_filter: None,
        }
    }
}

/// A proposed metadata update for a single learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichProposal {
    /// ID of the learning being enriched
    pub learning_id: i64,
    /// Title of the learning (for human-readable output)
    pub learning_title: String,
    /// Proposed file glob patterns
    pub proposed_files: Vec<String>,
    /// Proposed task type prefixes
    pub proposed_task_types: Vec<String>,
    /// Proposed error patterns
    pub proposed_errors: Vec<String>,
    /// Proposed tags
    pub proposed_tags: Vec<String>,
}

/// Result of the `curate enrich` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichResult {
    /// Whether this was a dry run (no DB changes made)
    pub dry_run: bool,
    /// Field filter applied, if any
    pub field_filter: Option<String>,
    /// Total number of learnings considered for enrichment
    pub total_candidates: usize,
    /// Number of LLM batches processed
    pub batches_processed: usize,
    /// Number of learnings whose metadata was updated (0 when dry_run=true)
    pub learnings_enriched: usize,
    /// Number of LLM call failures encountered
    pub llm_errors: usize,
    /// Per-learning enrichment proposals
    pub proposals: Vec<EnrichProposal>,
}

/// A learning identified as a candidate for metadata enrichment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichCandidate {
    /// Learning ID
    pub id: i64,
    /// Learning title
    pub title: String,
    /// Whether `applies_to_files` is NULL
    pub missing_files: bool,
    /// Whether `applies_to_task_types` is NULL
    pub missing_task_types: bool,
    /// Whether `applies_to_errors` is NULL
    pub missing_errors: bool,
}

/// Parameters for `merge_cluster()`: the pre-validated input to the DB merge operation.
///
/// The caller is responsible for resolving which source IDs form a duplicate
/// cluster and for obtaining merged title/content from the LLM.  This struct
/// carries only what the DB layer needs to perform the merge.
#[derive(Debug, Clone)]
pub struct MergeClusterParams {
    /// IDs of source learnings to merge and retire.
    pub source_ids: Vec<i64>,
    /// Merged title produced by the LLM.
    pub merged_title: String,
    /// Merged content produced by the LLM.
    pub merged_content: String,
}

/// Result of a single `merge_cluster()` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeClusterResult {
    /// Database ID of the newly-created merged learning.
    pub merged_learning_id: i64,
    /// Source IDs that were retired as part of this merge.
    pub retired_source_ids: Vec<i64>,
    /// Source IDs skipped because they were already retired (e.g. merged by a
    /// prior cluster in the same batch).
    pub skipped_source_ids: Vec<i64>,
}

/// Parameters for the `curate retire` command.
#[derive(Debug, Clone)]
pub struct RetireParams {
    /// If true, identify candidates but do not set retired_at
    pub dry_run: bool,
    /// Minimum age in days for criterion 1 (default: 90)
    pub min_age_days: u32,
    /// Minimum times_shown for criteria 2 and 3 (default: 10)
    pub min_shows: u32,
    /// Maximum application rate for criterion 3 (default: 0.05)
    pub max_rate: f64,
}

impl Default for RetireParams {
    fn default() -> Self {
        Self {
            dry_run: false,
            min_age_days: 90,
            min_shows: 10,
            max_rate: 0.05,
        }
    }
}
