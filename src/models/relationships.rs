//! Task relationship model for task-mgr.
//!
//! This module defines the TaskRelationship struct and RelationshipType enum
//! for tracking dependencies, synergies, batches, and conflicts between tasks.

use rusqlite::Row;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::TaskMgrError;

/// Represents the type of relationship between two tasks.
///
/// Maps to the `rel_type` column CHECK constraint in the task_relationships table:
/// `CHECK(rel_type IN ('dependsOn', 'synergyWith', 'batchWith', 'conflictsWith'))`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelationshipType {
    /// Hard dependency - task cannot start until related task is done/irrelevant
    DependsOn,
    /// Soft hint - prefer to do these tasks in adjacent iterations
    SynergyWith,
    /// Directive - do these tasks together in one iteration
    BatchWith,
    /// Avoidance hint - don't do immediately after related task
    ConflictsWith,
}

impl RelationshipType {
    /// Returns the database string representation of this relationship type.
    #[must_use]
    pub fn as_db_str(&self) -> &'static str {
        match self {
            RelationshipType::DependsOn => "dependsOn",
            RelationshipType::SynergyWith => "synergyWith",
            RelationshipType::BatchWith => "batchWith",
            RelationshipType::ConflictsWith => "conflictsWith",
        }
    }

    /// Returns true if this relationship type represents a hard constraint (blocking).
    ///
    /// Only `DependsOn` relationships are blocking - a task with dependencies
    /// cannot be started until all dependencies are satisfied.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        matches!(self, RelationshipType::DependsOn)
    }

    /// Returns true if this relationship type represents a soft hint (non-blocking).
    ///
    /// Soft hints influence task selection ordering but don't prevent task selection.
    /// Includes: SynergyWith, BatchWith, ConflictsWith
    #[must_use]
    pub fn is_soft_hint(&self) -> bool {
        !self.is_blocking()
    }

    /// Returns true if this relationship type has a positive influence on selection.
    ///
    /// SynergyWith and BatchWith increase the score of related tasks.
    #[must_use]
    pub fn is_positive(&self) -> bool {
        matches!(
            self,
            RelationshipType::SynergyWith | RelationshipType::BatchWith
        )
    }

    /// Returns true if this relationship type has a negative influence on selection.
    ///
    /// ConflictsWith decreases the score of related tasks.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        matches!(self, RelationshipType::ConflictsWith)
    }
}

impl fmt::Display for RelationshipType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_db_str())
    }
}

impl FromStr for RelationshipType {
    type Err = TaskMgrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dependsOn" => Ok(RelationshipType::DependsOn),
            "synergyWith" => Ok(RelationshipType::SynergyWith),
            "batchWith" => Ok(RelationshipType::BatchWith),
            "conflictsWith" => Ok(RelationshipType::ConflictsWith),
            _ => Err(TaskMgrError::invalid_state(
                "RelationshipType",
                s,
                "dependsOn, synergyWith, batchWith, or conflictsWith",
                s,
            )),
        }
    }
}

/// Represents a relationship between two tasks.
///
/// This struct maps to the `task_relationships` table in the database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRelationship {
    /// The task that has this relationship
    pub task_id: String,

    /// The task that is related to
    pub related_id: String,

    /// The type of relationship
    pub rel_type: RelationshipType,
}

impl TaskRelationship {
    /// Creates a new task relationship.
    #[must_use]
    pub fn new(
        task_id: impl Into<String>,
        related_id: impl Into<String>,
        rel_type: RelationshipType,
    ) -> Self {
        TaskRelationship {
            task_id: task_id.into(),
            related_id: related_id.into(),
            rel_type,
        }
    }

    /// Creates a new DependsOn relationship.
    #[must_use]
    pub fn depends_on(task_id: impl Into<String>, depends_on_id: impl Into<String>) -> Self {
        Self::new(task_id, depends_on_id, RelationshipType::DependsOn)
    }

    /// Creates a new SynergyWith relationship.
    #[must_use]
    pub fn synergy_with(task_id: impl Into<String>, synergy_id: impl Into<String>) -> Self {
        Self::new(task_id, synergy_id, RelationshipType::SynergyWith)
    }

    /// Creates a new BatchWith relationship.
    #[must_use]
    pub fn batch_with(task_id: impl Into<String>, batch_id: impl Into<String>) -> Self {
        Self::new(task_id, batch_id, RelationshipType::BatchWith)
    }

    /// Creates a new ConflictsWith relationship.
    #[must_use]
    pub fn conflicts_with(task_id: impl Into<String>, conflict_id: impl Into<String>) -> Self {
        Self::new(task_id, conflict_id, RelationshipType::ConflictsWith)
    }

    /// Returns true if this relationship is a hard constraint (blocking).
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.rel_type.is_blocking()
    }

    /// Returns true if this relationship is a soft hint (non-blocking).
    #[must_use]
    pub fn is_soft_hint(&self) -> bool {
        self.rel_type.is_soft_hint()
    }
}

impl TryFrom<&Row<'_>> for TaskRelationship {
    type Error = TaskMgrError;

    fn try_from(row: &Row<'_>) -> Result<Self, Self::Error> {
        let rel_type_str: String = row.get("rel_type")?;
        let rel_type = RelationshipType::from_str(&rel_type_str)?;

        Ok(TaskRelationship {
            task_id: row.get("task_id")?,
            related_id: row.get("related_id")?,
            rel_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ RelationshipType tests ============

    #[test]
    fn test_relationship_type_display() {
        assert_eq!(RelationshipType::DependsOn.to_string(), "dependsOn");
        assert_eq!(RelationshipType::SynergyWith.to_string(), "synergyWith");
        assert_eq!(RelationshipType::BatchWith.to_string(), "batchWith");
        assert_eq!(RelationshipType::ConflictsWith.to_string(), "conflictsWith");
    }

    #[test]
    fn test_relationship_type_from_str() {
        assert_eq!(
            RelationshipType::from_str("dependsOn").unwrap(),
            RelationshipType::DependsOn
        );
        assert_eq!(
            RelationshipType::from_str("synergyWith").unwrap(),
            RelationshipType::SynergyWith
        );
        assert_eq!(
            RelationshipType::from_str("batchWith").unwrap(),
            RelationshipType::BatchWith
        );
        assert_eq!(
            RelationshipType::from_str("conflictsWith").unwrap(),
            RelationshipType::ConflictsWith
        );
    }

    #[test]
    fn test_relationship_type_from_str_invalid() {
        let result = RelationshipType::from_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_relationship_type_roundtrip() {
        let types = [
            RelationshipType::DependsOn,
            RelationshipType::SynergyWith,
            RelationshipType::BatchWith,
            RelationshipType::ConflictsWith,
        ];

        for rel_type in types {
            let s = rel_type.to_string();
            let parsed = RelationshipType::from_str(&s).unwrap();
            assert_eq!(rel_type, parsed);
        }
    }

    #[test]
    fn test_relationship_type_is_blocking() {
        assert!(RelationshipType::DependsOn.is_blocking());
        assert!(!RelationshipType::SynergyWith.is_blocking());
        assert!(!RelationshipType::BatchWith.is_blocking());
        assert!(!RelationshipType::ConflictsWith.is_blocking());
    }

    #[test]
    fn test_relationship_type_is_soft_hint() {
        assert!(!RelationshipType::DependsOn.is_soft_hint());
        assert!(RelationshipType::SynergyWith.is_soft_hint());
        assert!(RelationshipType::BatchWith.is_soft_hint());
        assert!(RelationshipType::ConflictsWith.is_soft_hint());
    }

    #[test]
    fn test_relationship_type_is_positive() {
        assert!(!RelationshipType::DependsOn.is_positive());
        assert!(RelationshipType::SynergyWith.is_positive());
        assert!(RelationshipType::BatchWith.is_positive());
        assert!(!RelationshipType::ConflictsWith.is_positive());
    }

    #[test]
    fn test_relationship_type_is_negative() {
        assert!(!RelationshipType::DependsOn.is_negative());
        assert!(!RelationshipType::SynergyWith.is_negative());
        assert!(!RelationshipType::BatchWith.is_negative());
        assert!(RelationshipType::ConflictsWith.is_negative());
    }

    #[test]
    fn test_relationship_type_serialization() {
        let rel_type = RelationshipType::DependsOn;
        let json = serde_json::to_string(&rel_type).unwrap();
        assert_eq!(json, r#""dependsOn""#);

        let deserialized: RelationshipType = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, rel_type);
    }

    #[test]
    fn test_relationship_type_serialization_all() {
        let test_cases = [
            (RelationshipType::DependsOn, r#""dependsOn""#),
            (RelationshipType::SynergyWith, r#""synergyWith""#),
            (RelationshipType::BatchWith, r#""batchWith""#),
            (RelationshipType::ConflictsWith, r#""conflictsWith""#),
        ];

        for (rel_type, expected_json) in test_cases {
            let json = serde_json::to_string(&rel_type).unwrap();
            assert_eq!(json, expected_json);

            let deserialized: RelationshipType = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, rel_type);
        }
    }

    // ============ TaskRelationship tests ============

    #[test]
    fn test_task_relationship_new() {
        let rel = TaskRelationship::new("US-002", "US-001", RelationshipType::DependsOn);
        assert_eq!(rel.task_id, "US-002");
        assert_eq!(rel.related_id, "US-001");
        assert_eq!(rel.rel_type, RelationshipType::DependsOn);
    }

    #[test]
    fn test_task_relationship_depends_on() {
        let rel = TaskRelationship::depends_on("US-002", "US-001");
        assert_eq!(rel.task_id, "US-002");
        assert_eq!(rel.related_id, "US-001");
        assert_eq!(rel.rel_type, RelationshipType::DependsOn);
    }

    #[test]
    fn test_task_relationship_synergy_with() {
        let rel = TaskRelationship::synergy_with("US-002", "US-003");
        assert_eq!(rel.task_id, "US-002");
        assert_eq!(rel.related_id, "US-003");
        assert_eq!(rel.rel_type, RelationshipType::SynergyWith);
    }

    #[test]
    fn test_task_relationship_batch_with() {
        let rel = TaskRelationship::batch_with("US-002", "FIX-001");
        assert_eq!(rel.task_id, "US-002");
        assert_eq!(rel.related_id, "FIX-001");
        assert_eq!(rel.rel_type, RelationshipType::BatchWith);
    }

    #[test]
    fn test_task_relationship_conflicts_with() {
        let rel = TaskRelationship::conflicts_with("US-002", "TECH-001");
        assert_eq!(rel.task_id, "US-002");
        assert_eq!(rel.related_id, "TECH-001");
        assert_eq!(rel.rel_type, RelationshipType::ConflictsWith);
    }

    #[test]
    fn test_task_relationship_is_blocking() {
        let blocking = TaskRelationship::depends_on("US-002", "US-001");
        assert!(blocking.is_blocking());

        let non_blocking = TaskRelationship::synergy_with("US-002", "US-003");
        assert!(!non_blocking.is_blocking());
    }

    #[test]
    fn test_task_relationship_is_soft_hint() {
        let blocking = TaskRelationship::depends_on("US-002", "US-001");
        assert!(!blocking.is_soft_hint());

        let hint = TaskRelationship::synergy_with("US-002", "US-003");
        assert!(hint.is_soft_hint());

        let batch = TaskRelationship::batch_with("US-002", "FIX-001");
        assert!(batch.is_soft_hint());

        let conflict = TaskRelationship::conflicts_with("US-002", "TECH-001");
        assert!(conflict.is_soft_hint());
    }

    #[test]
    fn test_task_relationship_serialization() {
        let rel = TaskRelationship::depends_on("US-002", "US-001");
        let json = serde_json::to_string(&rel).unwrap();
        assert!(json.contains(r#""task_id":"US-002""#));
        assert!(json.contains(r#""related_id":"US-001""#));
        assert!(json.contains(r#""rel_type":"dependsOn""#));

        let deserialized: TaskRelationship = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, rel);
    }

    #[test]
    fn test_task_relationship_deserialization() {
        let json = r#"{
            "task_id": "US-003",
            "related_id": "US-001",
            "rel_type": "synergyWith"
        }"#;

        let rel: TaskRelationship = serde_json::from_str(json).unwrap();
        assert_eq!(rel.task_id, "US-003");
        assert_eq!(rel.related_id, "US-001");
        assert_eq!(rel.rel_type, RelationshipType::SynergyWith);
    }

    #[test]
    fn test_task_relationship_clone_and_eq() {
        let rel1 = TaskRelationship::depends_on("US-002", "US-001");
        let rel2 = rel1.clone();
        assert_eq!(rel1, rel2);
    }
}
