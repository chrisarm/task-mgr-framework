//! Output types and formatting for the next command.
//!
//! This module contains all the output structures and formatting functions
//! for the `next` command, including JSON-serializable types and text formatters.

use serde::Serialize;

use crate::models::Learning;

use super::selection::{ScoreBreakdown, ScoredTask};

/// Result of the next command, integrating task selection, claiming, and learnings.
#[derive(Debug, Clone, Serialize)]
pub struct NextResult {
    /// The selected task with full details and score
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<NextTaskOutput>,
    /// Eligible batch tasks (batchWith targets that are still todo)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub batch_tasks: Vec<String>,
    /// Relevant learnings for this task
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub learnings: Vec<LearningSummaryOutput>,
    /// Selection metadata
    pub selection: SelectionMetadata,
    /// Claim metadata (only present if --claim was used)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimMetadata>,
    /// Top candidate tasks for verbose output (up to 5)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_candidates: Vec<CandidateSummary>,
}

/// Summary of a candidate task for verbose output.
#[derive(Debug, Clone, Serialize)]
pub struct CandidateSummary {
    /// Task ID
    pub id: String,
    /// Task title
    pub title: String,
    /// Priority
    pub priority: i32,
    /// Total score
    pub total_score: i32,
    /// Score breakdown
    pub score: ScoreOutput,
}

/// Task output with score breakdown for transparency.
#[derive(Debug, Clone, Serialize)]
pub struct NextTaskOutput {
    /// Task ID
    pub id: String,
    /// Task title
    pub title: String,
    /// Task description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Task priority (1 = highest)
    pub priority: i32,
    /// Current task status
    pub status: String,
    /// Acceptance criteria for the task
    pub acceptance_criteria: Vec<String>,
    /// Additional notes about the task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Files this task touches
    pub files: Vec<String>,
    /// Task IDs in batchWith relationship
    pub batch_with: Vec<String>,
    /// Preferred model for this task (e.g., "claude-opus-4-6")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Difficulty level for this task (e.g., "low", "medium", "high")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,
    /// Note explaining why this task was escalated to a higher-tier model
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_note: Option<String>,
    /// Whether this task requires human review after completion.
    #[serde(default)]
    pub requires_human: bool,
    /// Score breakdown for transparency
    pub score: ScoreOutput,
}

/// Score breakdown for the next command output.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreOutput {
    /// Total calculated score
    pub total: i32,
    /// Score from priority (1000 - priority)
    pub priority: i32,
    /// Score from file overlap with --after-files
    pub file_overlap: i32,
    /// Score from synergy relationships
    pub synergy: i32,
    /// Score from conflict relationships (negative)
    pub conflict: i32,
    /// Number of files that overlapped
    pub file_overlap_count: i32,
    /// Tasks that provided synergy bonus
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub synergy_from: Vec<String>,
    /// Tasks that caused conflict penalty
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflict_from: Vec<String>,
}

/// Learning summary for the next command output.
#[derive(Debug, Clone, Serialize)]
pub struct LearningSummaryOutput {
    /// Learning ID
    pub id: i64,
    /// Learning title
    pub title: String,
    /// Learning outcome type
    pub outcome: String,
    /// Confidence level
    pub confidence: String,
    /// Learning content (may be truncated in text output)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// File patterns this learning applies to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applies_to_files: Option<Vec<String>>,
    /// Task type prefixes this learning applies to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applies_to_task_types: Option<Vec<String>>,
}

impl From<Learning> for LearningSummaryOutput {
    fn from(learning: Learning) -> Self {
        LearningSummaryOutput {
            id: learning.id.unwrap_or(0),
            title: learning.title.clone(),
            outcome: learning.outcome.to_string(),
            confidence: learning.confidence.to_string(),
            content: Some(learning.content.clone()),
            applies_to_files: learning.applies_to_files.clone(),
            applies_to_task_types: learning.applies_to_task_types.clone(),
        }
    }
}

/// Selection metadata for the next command output.
#[derive(Debug, Clone, Serialize)]
pub struct SelectionMetadata {
    /// Human-readable selection reason
    pub reason: String,
    /// Number of eligible tasks considered
    pub eligible_count: usize,
}

/// Claim metadata for the next command output.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimMetadata {
    /// Whether the task was claimed
    pub claimed: bool,
    /// Run ID if tracking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Global iteration counter after claiming
    pub iteration: i64,
}

/// Build a ScoreOutput from a ScoreBreakdown.
impl From<&ScoreBreakdown> for ScoreOutput {
    fn from(breakdown: &ScoreBreakdown) -> Self {
        ScoreOutput {
            total: breakdown.priority_score
                + breakdown.file_score
                + breakdown.synergy_score
                + breakdown.conflict_score,
            priority: breakdown.priority_score,
            file_overlap: breakdown.file_score,
            synergy: breakdown.synergy_score,
            conflict: breakdown.conflict_score,
            file_overlap_count: breakdown.file_overlap_count,
            synergy_from: breakdown.synergy_from.clone(),
            conflict_from: breakdown.conflict_from.clone(),
        }
    }
}

/// Build the task output from a scored task.
pub fn build_task_output(scored_task: &ScoredTask, claimed: bool) -> NextTaskOutput {
    let status = if claimed {
        "in_progress".to_string()
    } else {
        scored_task.task.status.to_string()
    };

    NextTaskOutput {
        id: scored_task.task.id.clone(),
        title: scored_task.task.title.clone(),
        description: scored_task.task.description.clone(),
        priority: scored_task.task.priority,
        status,
        acceptance_criteria: scored_task.task.acceptance_criteria.clone(),
        notes: scored_task.task.notes.clone(),
        files: scored_task.files.clone(),
        batch_with: scored_task.batch_with.clone(),
        model: scored_task.task.model.clone(),
        difficulty: scored_task.task.difficulty.clone(),
        escalation_note: scored_task.task.escalation_note.clone(),
        requires_human: scored_task.task.requires_human,
        score: ScoreOutput::from(&scored_task.score_breakdown),
    }
}

/// Format NextResult as human-readable text.
pub fn format_next_text(result: &NextResult) -> String {
    let mut output = String::new();

    match &result.task {
        Some(task) => {
            output.push_str(&format!("Next Task: {} - {}\n", task.id, task.title));
            output.push_str(&format!("{}\n\n", "=".repeat(60)));

            output.push_str(&format!("Priority: {}\n", task.priority));
            output.push_str(&format!("Status:   {}\n", task.status));
            output.push_str(&format!(
                "Score:    {} (priority: {}, file_overlap: {}, synergy: {}, conflict: {})\n",
                task.score.total,
                task.score.priority,
                task.score.file_overlap,
                task.score.synergy,
                task.score.conflict
            ));

            if let Some(ref desc) = task.description {
                output.push_str(&format!("\nDescription:\n  {}\n", desc));
            }

            if !task.acceptance_criteria.is_empty() {
                output.push_str("\nAcceptance Criteria:\n");
                for criterion in &task.acceptance_criteria {
                    output.push_str(&format!("  [ ] {}\n", criterion));
                }
            }

            if !task.files.is_empty() {
                output.push_str("\nFiles:\n");
                for file in &task.files {
                    output.push_str(&format!("  - {}\n", file));
                }
            }

            // Show claim info if claimed
            if let Some(ref claim) = result.claim {
                output.push_str(&format!(
                    "\nClaimed: {} (iteration: {}",
                    if claim.claimed { "Yes" } else { "No" },
                    claim.iteration
                ));
                if let Some(ref rid) = claim.run_id {
                    output.push_str(&format!(", run: {}", rid));
                }
                output.push_str(")\n");
            }

            // Show batch tasks if any
            if !result.batch_tasks.is_empty() {
                output.push_str("\nBatch Tasks (consider doing together):\n");
                for batch_id in &result.batch_tasks {
                    output.push_str(&format!("  - {}\n", batch_id));
                }
            }

            // Show learnings if any
            if !result.learnings.is_empty() {
                output.push_str(&format!(
                    "\nRelevant Learnings ({}):\n",
                    result.learnings.len()
                ));
                for (i, learning) in result.learnings.iter().enumerate() {
                    output.push_str(&format!(
                        "  {}. [{}] {} ({} confidence)\n",
                        i + 1,
                        learning.outcome,
                        learning.title,
                        learning.confidence
                    ));
                    if let Some(ref content) = learning.content {
                        let preview = crate::commands::truncate_str(content, 80);
                        output.push_str(&format!("     {}\n", preview));
                    }
                }
            }

            output.push_str(&format!(
                "\nEligible Tasks: {}",
                result.selection.eligible_count
            ));
        }
        None => {
            output.push_str("No tasks available for selection.\n\n");
            output.push_str(&result.selection.reason);
        }
    }

    output
}

/// Format verbose output for the next command (to stderr).
///
/// Returns a string that should be written to stderr when --verbose is enabled.
pub fn format_next_verbose(result: &NextResult) -> String {
    let mut output = String::new();

    if result.top_candidates.is_empty() {
        return output;
    }

    output.push_str("[verbose] Task Selection Scoring (top 5 candidates):\n");
    output.push_str(&format!("{}\n", "-".repeat(70)));

    for (i, candidate) in result.top_candidates.iter().enumerate() {
        let selected = if i == 0 { " <- SELECTED" } else { "" };
        output.push_str(&format!(
            "  {}. {} - {}{}\n",
            i + 1,
            candidate.id,
            candidate.title,
            selected
        ));
        output.push_str(&format!(
            "     Total Score: {} (priority: {:+}, file: {:+}, synergy: {:+}, conflict: {:+})\n",
            candidate.total_score,
            candidate.score.priority,
            candidate.score.file_overlap,
            candidate.score.synergy,
            candidate.score.conflict
        ));
        if candidate.score.file_overlap_count > 0 {
            output.push_str(&format!(
                "     File overlap: {} file(s)\n",
                candidate.score.file_overlap_count
            ));
        }
        if !candidate.score.synergy_from.is_empty() {
            output.push_str(&format!(
                "     Synergy from: {}\n",
                candidate.score.synergy_from.join(", ")
            ));
        }
        if !candidate.score.conflict_from.is_empty() {
            output.push_str(&format!(
                "     Conflicts with: {}\n",
                candidate.score.conflict_from.join(", ")
            ));
        }
    }

    output.push_str(&format!("{}\n", "-".repeat(70)));
    output.push_str(&format!(
        "[verbose] {} eligible tasks total\n",
        result.selection.eligible_count
    ));

    output
}
