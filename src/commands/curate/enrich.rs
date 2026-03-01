//! Enrich prompt construction and response parser for `curate enrich`.
//!
//! Builds a prompt that instructs Claude to suggest missing metadata fields for
//! a batch of learnings, and parses the JSON array response.
//!
//! NOTE: `build_enrich_prompt` and `parse_enrich_response` are stubs.
//! Implementation is deferred to FEAT-004 and FEAT-005.

use crate::commands::curate::types::EnrichProposal;
use crate::TaskMgrResult;

/// A single learning passed to the enrich LLM prompt.
#[derive(Debug, Clone)]
pub struct EnrichBatchItem {
    /// Learning ID (used to correlate LLM response back to the learning)
    pub id: i64,
    /// Learning title
    pub title: String,
    /// Full learning content
    pub content: String,
    /// Existing tags (may be empty)
    pub existing_tags: Vec<String>,
}

/// Builds the enrich prompt for Claude.
///
/// - Wraps untrusted content (learning titles/content) with a random UUID delimiter
///   to prevent prompt injection.
/// - Includes UNTRUSTED warning.
/// - Includes ID, title, content, and existing tags for each batch item.
/// - Requests a JSON array response with specific field names.
///
/// **NOTE**: Stub — implementation deferred to FEAT-004.
pub fn build_enrich_prompt(_batch: &[EnrichBatchItem]) -> String {
    todo!("FEAT-004: implement build_enrich_prompt")
}

/// Parses Claude's enrich response into a vec of `EnrichProposal`.
///
/// - Handles raw JSON arrays and markdown code-block-wrapped JSON.
/// - Returns empty vec on parse failure (best-effort / graceful degradation).
/// - Validates learning IDs against the input batch; rejects any proposals that
///   reference an ID not in `batch_ids` (prevents hallucinated IDs).
///
/// **NOTE**: Stub — implementation deferred to FEAT-005.
pub fn parse_enrich_response(
    _response: &str,
    _batch_ids: &[i64],
) -> TaskMgrResult<Vec<EnrichProposal>> {
    todo!("FEAT-005: implement parse_enrich_response")
}
