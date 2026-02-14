//! Learning export models for progress export.
//!
//! Contains export formats for learnings and learning summaries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::learning::{Confidence, LearningOutcome};

/// Export format for a learning.
///
/// Contains all learning fields for export/import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningExport {
    /// Database ID (may be None for imported learnings)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,

    /// When the learning was created
    pub created_at: DateTime<Utc>,

    /// ID of the task that generated this learning (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,

    /// ID of the run during which this learning was captured (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,

    /// Type of outcome this learning represents
    pub outcome: LearningOutcome,

    /// Short title summarizing the learning
    pub title: String,

    /// Detailed content of the learning
    pub content: String,

    /// Root cause analysis (for failures)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,

    /// Solution or fix applied (for failures/workarounds)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solution: Option<String>,

    /// File patterns this learning applies to (glob patterns)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_to_files: Option<Vec<String>>,

    /// Task type prefixes this learning applies to (e.g., "US-", "FIX-")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_to_task_types: Option<Vec<String>>,

    /// Error patterns this learning applies to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_to_errors: Option<Vec<String>>,

    /// Confidence level in this learning
    pub confidence: Confidence,

    /// Number of times this learning has been shown to an agent
    pub times_shown: i32,

    /// Number of times this learning has been marked as applied/useful
    pub times_applied: i32,

    /// When this learning was last shown to an agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_shown_at: Option<DateTime<Utc>>,

    /// When this learning was last marked as applied/useful
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_applied_at: Option<DateTime<Utc>>,

    /// Tags associated with this learning
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl LearningExport {
    /// Creates a new LearningExport with required fields.
    #[must_use]
    pub fn new(
        outcome: LearningOutcome,
        title: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        LearningExport {
            id: None,
            created_at: Utc::now(),
            task_id: None,
            run_id: None,
            outcome,
            title: title.into(),
            content: content.into(),
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            confidence: Confidence::default(),
            times_shown: 0,
            times_applied: 0,
            last_shown_at: None,
            last_applied_at: None,
            tags: Vec::new(),
        }
    }

    /// Returns the application rate (times_applied / times_shown).
    /// Returns None if times_shown is 0.
    #[must_use]
    pub fn application_rate(&self) -> Option<f64> {
        if self.times_shown == 0 {
            None
        } else {
            Some(f64::from(self.times_applied) / f64::from(self.times_shown))
        }
    }
}

/// Summary view of a learning for compact display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningSummary {
    /// Database ID
    pub id: i64,

    /// Short title
    pub title: String,

    /// Outcome type
    pub outcome: LearningOutcome,

    /// Confidence level
    pub confidence: Confidence,

    /// Application rate (times_applied / times_shown)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_rate: Option<f64>,

    /// When created
    pub created_at: DateTime<Utc>,
}

impl LearningSummary {
    /// Creates a new LearningSummary from a LearningExport.
    #[must_use]
    pub fn from_export(export: &LearningExport) -> Option<Self> {
        Some(LearningSummary {
            id: export.id?,
            title: export.title.clone(),
            outcome: export.outcome,
            confidence: export.confidence,
            application_rate: export.application_rate(),
            created_at: export.created_at,
        })
    }
}
