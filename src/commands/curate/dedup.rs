//! Dedup prompt construction and response parser for `curate dedup`.
//!
//! Builds a prompt that instructs Claude to identify semantic duplicate clusters
//! among a batch of learnings, and parses the JSON array response.
//!
//! NOTE: Full implementation deferred to FEAT-004 (curate_dedup orchestrator).

use crate::commands::curate::types::{DeduplicateLearningItem, RawDedupCluster};
use crate::TaskMgrResult;

/// Builds the dedup prompt for Claude.
///
/// - Wraps untrusted content (learning titles/content) with a random UUID delimiter
///   to prevent prompt injection.
/// - Includes UNTRUSTED warning.
/// - Includes ID, title, and content for each learning.
/// - Includes the similarity threshold as guidance.
/// - Requests a JSON array response where each element is a cluster of duplicate IDs.
///
/// # Stub
/// Not yet implemented — tracked as FEAT-004.
pub fn build_dedup_prompt(items: &[DeduplicateLearningItem], similarity_threshold: f64) -> String {
    let _ = (items, similarity_threshold);
    todo!("FEAT-004: implement build_dedup_prompt")
}

/// Parses Claude's dedup response into a vec of `RawDedupCluster`.
///
/// - Handles raw JSON arrays and markdown code-block-wrapped JSON.
/// - Returns empty vec on parse failure (best-effort / graceful degradation).
/// - Filters out clusters whose IDs do not all appear in `valid_ids`.
/// - Rejects clusters with fewer than 2 IDs (not a merge).
/// - When the same learning ID appears in multiple clusters, the first cluster
///   wins and later clusters containing that ID are skipped.
///
/// # Stub
/// Not yet implemented — tracked as FEAT-004.
pub fn parse_dedup_response(
    response: &str,
    valid_ids: &[i64],
) -> TaskMgrResult<Vec<RawDedupCluster>> {
    let _ = (response, valid_ids);
    todo!("FEAT-004: implement parse_dedup_response")
}
