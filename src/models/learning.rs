//! Learning model for the institutional memory system.
//!
//! This module defines the Learning struct for capturing and recalling
//! learnings from successes, failures, workarounds, and patterns discovered
//! during task execution.

use chrono::{DateTime, Utc};
use rusqlite::Row;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use super::{parse_datetime, parse_optional_datetime};
use crate::TaskMgrError;

/// Represents the outcome type of a learning.
///
/// Maps to the `outcome` column CHECK constraint in the learnings table:
/// `CHECK(outcome IN ('failure', 'success', 'workaround', 'pattern'))`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningOutcome {
    /// Learning from a failure - what went wrong and how to avoid it
    Failure,
    /// Learning from a success - what worked well and why
    Success,
    /// A workaround for a known issue
    Workaround,
    /// A general pattern discovered during development
    Pattern,
}

impl LearningOutcome {
    /// Returns the database string representation of this outcome.
    #[must_use]
    pub fn as_db_str(&self) -> &'static str {
        match self {
            LearningOutcome::Failure => "failure",
            LearningOutcome::Success => "success",
            LearningOutcome::Workaround => "workaround",
            LearningOutcome::Pattern => "pattern",
        }
    }

    /// Returns true if this outcome represents an error condition.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, LearningOutcome::Failure)
    }

    /// Returns true if this outcome represents a positive discovery.
    #[must_use]
    pub fn is_positive(&self) -> bool {
        matches!(self, LearningOutcome::Success | LearningOutcome::Pattern)
    }
}

impl fmt::Display for LearningOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_db_str())
    }
}

impl FromStr for LearningOutcome {
    type Err = TaskMgrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "failure" => Ok(LearningOutcome::Failure),
            "success" => Ok(LearningOutcome::Success),
            "workaround" => Ok(LearningOutcome::Workaround),
            "pattern" => Ok(LearningOutcome::Pattern),
            _ => Err(TaskMgrError::invalid_state(
                "LearningOutcome",
                s,
                "failure, success, workaround, or pattern",
                s,
            )),
        }
    }
}

/// Represents the confidence level of a learning.
///
/// Maps to the `confidence` column CHECK constraint in the learnings table:
/// `CHECK(confidence IN ('high', 'medium', 'low'))`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// High confidence - verified and reliable
    High,
    /// Medium confidence - likely correct but not fully verified
    #[default]
    Medium,
    /// Low confidence - tentative or uncertain
    Low,
}

impl Confidence {
    /// Returns the database string representation of this confidence level.
    #[must_use]
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_db_str())
    }
}

impl FromStr for Confidence {
    type Err = TaskMgrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "high" => Ok(Confidence::High),
            "medium" => Ok(Confidence::Medium),
            "low" => Ok(Confidence::Low),
            _ => Err(TaskMgrError::invalid_state(
                "Confidence",
                s,
                "high, medium, or low",
                s,
            )),
        }
    }
}

/// Represents a learning in the institutional memory system.
///
/// Learnings capture knowledge gained from task execution, including
/// failures, successes, workarounds, and patterns. They are used to
/// provide context to future iterations when working on similar tasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Learning {
    /// Database ID (auto-increment)
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
}

impl Learning {
    /// Creates a new learning with required fields.
    #[must_use]
    pub fn new(
        outcome: LearningOutcome,
        title: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Learning {
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
        }
    }

    /// Records that this learning was shown to an agent.
    pub fn mark_shown(&mut self) {
        self.times_shown += 1;
        self.last_shown_at = Some(Utc::now());
    }

    /// Records that this learning was applied/useful.
    pub fn mark_applied(&mut self) {
        self.times_applied += 1;
        self.last_applied_at = Some(Utc::now());
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

/// Parses an optional JSON array string into Option<Vec<String>>.
///
/// Note: This function intentionally returns None for invalid JSON rather than
/// propagating errors. This allows graceful degradation when database data
/// is malformed - the learning remains usable but without the optional field.
fn parse_optional_string_array(s: Option<String>) -> Option<Vec<String>> {
    match s {
        Some(json_str) if !json_str.is_empty() => serde_json::from_str(&json_str).ok(),
        _ => None,
    }
}

impl TryFrom<&Row<'_>> for Learning {
    type Error = TaskMgrError;

    fn try_from(row: &Row<'_>) -> Result<Self, Self::Error> {
        // Parse enums from strings
        let outcome_str: String = row.get("outcome")?;
        let outcome = LearningOutcome::from_str(&outcome_str)?;

        let confidence_str: String = row.get("confidence")?;
        let confidence = Confidence::from_str(&confidence_str)?;

        // Parse timestamps
        let created_at_str: String = row.get("created_at")?;
        let last_shown_at_str: Option<String> = row.get("last_shown_at")?;
        let last_applied_at_str: Option<String> = row.get("last_applied_at")?;

        // Parse JSON arrays
        let applies_to_files: Option<Vec<String>> =
            parse_optional_string_array(row.get("applies_to_files")?);
        let applies_to_task_types: Option<Vec<String>> =
            parse_optional_string_array(row.get("applies_to_task_types")?);
        let applies_to_errors: Option<Vec<String>> =
            parse_optional_string_array(row.get("applies_to_errors")?);

        Ok(Learning {
            id: row.get("id")?,
            created_at: parse_datetime(&created_at_str)?,
            task_id: row.get("task_id")?,
            run_id: row.get("run_id")?,
            outcome,
            title: row.get("title")?,
            content: row.get("content")?,
            root_cause: row.get("root_cause")?,
            solution: row.get("solution")?,
            applies_to_files,
            applies_to_task_types,
            applies_to_errors,
            confidence,
            times_shown: row.get("times_shown")?,
            times_applied: row.get("times_applied")?,
            last_shown_at: parse_optional_datetime(last_shown_at_str)?,
            last_applied_at: parse_optional_datetime(last_applied_at_str)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ LearningOutcome tests ============

    #[test]
    fn test_learning_outcome_display() {
        assert_eq!(LearningOutcome::Failure.to_string(), "failure");
        assert_eq!(LearningOutcome::Success.to_string(), "success");
        assert_eq!(LearningOutcome::Workaround.to_string(), "workaround");
        assert_eq!(LearningOutcome::Pattern.to_string(), "pattern");
    }

    #[test]
    fn test_learning_outcome_from_str() {
        assert_eq!(
            LearningOutcome::from_str("failure").unwrap(),
            LearningOutcome::Failure
        );
        assert_eq!(
            LearningOutcome::from_str("success").unwrap(),
            LearningOutcome::Success
        );
        assert_eq!(
            LearningOutcome::from_str("workaround").unwrap(),
            LearningOutcome::Workaround
        );
        assert_eq!(
            LearningOutcome::from_str("pattern").unwrap(),
            LearningOutcome::Pattern
        );
    }

    #[test]
    fn test_learning_outcome_from_str_invalid() {
        let result = LearningOutcome::from_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_learning_outcome_roundtrip() {
        let outcomes = [
            LearningOutcome::Failure,
            LearningOutcome::Success,
            LearningOutcome::Workaround,
            LearningOutcome::Pattern,
        ];

        for outcome in outcomes {
            let s = outcome.to_string();
            let parsed = LearningOutcome::from_str(&s).unwrap();
            assert_eq!(outcome, parsed);
        }
    }

    #[test]
    fn test_learning_outcome_is_error() {
        assert!(LearningOutcome::Failure.is_error());
        assert!(!LearningOutcome::Success.is_error());
        assert!(!LearningOutcome::Workaround.is_error());
        assert!(!LearningOutcome::Pattern.is_error());
    }

    #[test]
    fn test_learning_outcome_is_positive() {
        assert!(!LearningOutcome::Failure.is_positive());
        assert!(LearningOutcome::Success.is_positive());
        assert!(!LearningOutcome::Workaround.is_positive());
        assert!(LearningOutcome::Pattern.is_positive());
    }

    #[test]
    fn test_learning_outcome_serialization() {
        let outcome = LearningOutcome::Workaround;
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, r#""workaround""#);

        let deserialized: LearningOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, outcome);
    }

    // ============ Confidence tests ============

    #[test]
    fn test_confidence_display() {
        assert_eq!(Confidence::High.to_string(), "high");
        assert_eq!(Confidence::Medium.to_string(), "medium");
        assert_eq!(Confidence::Low.to_string(), "low");
    }

    #[test]
    fn test_confidence_from_str() {
        assert_eq!(Confidence::from_str("high").unwrap(), Confidence::High);
        assert_eq!(Confidence::from_str("medium").unwrap(), Confidence::Medium);
        assert_eq!(Confidence::from_str("low").unwrap(), Confidence::Low);
    }

    #[test]
    fn test_confidence_from_str_invalid() {
        let result = Confidence::from_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_confidence_roundtrip() {
        let levels = [Confidence::High, Confidence::Medium, Confidence::Low];

        for level in levels {
            let s = level.to_string();
            let parsed = Confidence::from_str(&s).unwrap();
            assert_eq!(level, parsed);
        }
    }

    #[test]
    fn test_confidence_default() {
        assert_eq!(Confidence::default(), Confidence::Medium);
    }

    #[test]
    fn test_confidence_serialization() {
        let confidence = Confidence::High;
        let json = serde_json::to_string(&confidence).unwrap();
        assert_eq!(json, r#""high""#);

        let deserialized: Confidence = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, confidence);
    }

    // ============ Learning tests ============

    #[test]
    fn test_learning_new() {
        let learning = Learning::new(
            LearningOutcome::Failure,
            "Test failure",
            "Detailed description of the failure",
        );
        assert!(learning.id.is_none());
        assert!(learning.task_id.is_none());
        assert!(learning.run_id.is_none());
        assert_eq!(learning.outcome, LearningOutcome::Failure);
        assert_eq!(learning.title, "Test failure");
        assert_eq!(learning.content, "Detailed description of the failure");
        assert_eq!(learning.confidence, Confidence::Medium);
        assert_eq!(learning.times_shown, 0);
        assert_eq!(learning.times_applied, 0);
    }

    #[test]
    fn test_learning_mark_shown() {
        let mut learning = Learning::new(LearningOutcome::Pattern, "Test", "Content");
        assert_eq!(learning.times_shown, 0);
        assert!(learning.last_shown_at.is_none());

        learning.mark_shown();
        assert_eq!(learning.times_shown, 1);
        assert!(learning.last_shown_at.is_some());

        learning.mark_shown();
        assert_eq!(learning.times_shown, 2);
    }

    #[test]
    fn test_learning_mark_applied() {
        let mut learning = Learning::new(LearningOutcome::Success, "Test", "Content");
        assert_eq!(learning.times_applied, 0);
        assert!(learning.last_applied_at.is_none());

        learning.mark_applied();
        assert_eq!(learning.times_applied, 1);
        assert!(learning.last_applied_at.is_some());

        learning.mark_applied();
        assert_eq!(learning.times_applied, 2);
    }

    #[test]
    fn test_learning_application_rate() {
        let mut learning = Learning::new(LearningOutcome::Pattern, "Test", "Content");

        // No shows yet
        assert!(learning.application_rate().is_none());

        // Show 4 times, apply 2 times
        learning.times_shown = 4;
        learning.times_applied = 2;
        assert_eq!(learning.application_rate(), Some(0.5));

        // All applied
        learning.times_applied = 4;
        assert_eq!(learning.application_rate(), Some(1.0));
    }

    #[test]
    fn test_learning_serialization() {
        let learning = Learning::new(LearningOutcome::Failure, "Test failure", "Content");
        let json = serde_json::to_string(&learning).unwrap();
        assert!(json.contains("\"outcome\":\"failure\""));
        assert!(json.contains("\"title\":\"Test failure\""));
        assert!(json.contains("\"confidence\":\"medium\""));
        // Optional None fields should be omitted
        assert!(!json.contains("\"id\""));
        assert!(!json.contains("\"task_id\""));
    }

    #[test]
    fn test_learning_deserialization() {
        let json = r#"{
            "id": 42,
            "created_at": "2026-01-18T12:00:00Z",
            "task_id": "US-001",
            "run_id": "run-123",
            "outcome": "workaround",
            "title": "SQLite WAL mode",
            "content": "Use WAL mode for crash recovery",
            "root_cause": "Corruption on crash",
            "solution": "PRAGMA journal_mode = WAL",
            "applies_to_files": ["src/db/*.rs"],
            "applies_to_task_types": ["US-", "TECH-"],
            "confidence": "high",
            "times_shown": 5,
            "times_applied": 3
        }"#;

        let learning: Learning = serde_json::from_str(json).unwrap();
        assert_eq!(learning.id, Some(42));
        assert_eq!(learning.task_id, Some("US-001".to_string()));
        assert_eq!(learning.run_id, Some("run-123".to_string()));
        assert_eq!(learning.outcome, LearningOutcome::Workaround);
        assert_eq!(learning.title, "SQLite WAL mode");
        assert_eq!(learning.root_cause, Some("Corruption on crash".to_string()));
        assert_eq!(
            learning.applies_to_files,
            Some(vec!["src/db/*.rs".to_string()])
        );
        assert_eq!(
            learning.applies_to_task_types,
            Some(vec!["US-".to_string(), "TECH-".to_string()])
        );
        assert_eq!(learning.confidence, Confidence::High);
        assert_eq!(learning.times_shown, 5);
        assert_eq!(learning.times_applied, 3);
    }

    #[test]
    fn test_learning_with_all_fields() {
        let mut learning = Learning::new(LearningOutcome::Pattern, "Pattern", "Content");
        learning.id = Some(1);
        learning.task_id = Some("US-001".to_string());
        learning.run_id = Some("run-123".to_string());
        learning.root_cause = Some("Root cause".to_string());
        learning.solution = Some("Solution".to_string());
        learning.applies_to_files = Some(vec!["src/*.rs".to_string()]);
        learning.applies_to_task_types = Some(vec!["US-".to_string()]);
        learning.applies_to_errors = Some(vec!["E0001".to_string()]);
        learning.confidence = Confidence::High;
        learning.times_shown = 10;
        learning.times_applied = 5;

        let json = serde_json::to_string(&learning).unwrap();
        let parsed: Learning = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, learning.id);
        assert_eq!(parsed.task_id, learning.task_id);
        assert_eq!(parsed.run_id, learning.run_id);
        assert_eq!(parsed.outcome, learning.outcome);
        assert_eq!(parsed.root_cause, learning.root_cause);
        assert_eq!(parsed.solution, learning.solution);
        assert_eq!(parsed.applies_to_files, learning.applies_to_files);
        assert_eq!(parsed.applies_to_task_types, learning.applies_to_task_types);
        assert_eq!(parsed.applies_to_errors, learning.applies_to_errors);
        assert_eq!(parsed.confidence, learning.confidence);
        assert_eq!(parsed.times_shown, learning.times_shown);
        assert_eq!(parsed.times_applied, learning.times_applied);
    }

    // Datetime parsing tests are in models/datetime.rs

    #[test]
    fn test_parse_optional_string_array() {
        // Valid JSON array
        let result = parse_optional_string_array(Some(r#"["a", "b", "c"]"#.to_string()));
        assert_eq!(
            result,
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );

        // Empty string
        let result = parse_optional_string_array(Some("".to_string()));
        assert!(result.is_none());

        // None
        let result = parse_optional_string_array(None);
        assert!(result.is_none());

        // Invalid JSON (returns None, doesn't panic)
        let result = parse_optional_string_array(Some("not json".to_string()));
        assert!(result.is_none());
    }
}
