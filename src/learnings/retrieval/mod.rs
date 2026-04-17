//! Pluggable retrieval backends for the learnings system.
//!
//! The [`RetrievalBackend`] trait defines a standard interface for finding relevant
//! learnings. Backends handle retrieval only — UCB bandit ranking is layered on top
//! by the recall module.
//!
//! ## Built-in backends
//!
//! - [`Fts5Backend`] — FTS5 full-text search with BM25 scoring (LIKE fallback)
//! - [`PatternsBackend`] — Task-context pattern matching (file, type prefix, error)
//! - [`CompositeBackend`] — Merges results from multiple backends

pub mod composite;
pub mod fts5;
pub mod patterns;
pub mod vector;

#[cfg(test)]
mod tests;

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::models::{Learning, LearningOutcome};

/// Everything a retrieval backend needs to find relevant learnings.
#[derive(Debug, Clone, Default)]
pub struct RetrievalQuery {
    /// Free-text search query
    pub text: Option<String>,
    /// Task ID for task-context-aware retrieval
    pub task_id: Option<String>,
    /// File paths from the task's `touchesFiles`
    pub task_files: Vec<String>,
    /// Task type prefix (e.g., "US-")
    pub task_prefix: Option<String>,
    /// Error message from the task's last failure
    pub task_error: Option<String>,
    /// Filter by tags (learning must have at least one)
    pub tags: Option<Vec<String>>,
    /// Filter by outcome type
    pub outcome: Option<LearningOutcome>,
    /// Maximum results to return
    pub limit: usize,
}

/// A retrieval result with backend-specific relevance score.
#[derive(Debug, Clone)]
pub struct ScoredLearning {
    /// The retrieved learning
    pub learning: Learning,
    /// Relevance score (higher = more relevant)
    pub relevance_score: f64,
    /// Human-readable explanation of why this matched
    pub match_reason: Option<String>,
}

/// Pluggable learning retrieval backend.
///
/// Object-safe for `Box<dyn RetrievalBackend>` dispatch.
/// UCB bandit ranking is NOT part of this trait — it's layered on top.
pub trait RetrievalBackend: Send + Sync {
    /// Human-readable name of this backend (e.g., "fts5", "patterns").
    fn name(&self) -> &str;

    /// Retrieve relevant learnings matching the query.
    ///
    /// Returns scored results ordered by relevance (highest first).
    fn retrieve(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
    ) -> TaskMgrResult<Vec<ScoredLearning>>;

    /// Index a new learning. No-op for backends that use SQLite triggers (e.g., FTS5).
    fn index(&self, _conn: &Connection, _learning: &Learning) -> TaskMgrResult<()> {
        Ok(())
    }

    /// Remove a learning from the index. No-op for trigger-based backends.
    fn remove(&self, _conn: &Connection, _learning_id: i64) -> TaskMgrResult<()> {
        Ok(())
    }
}

pub use composite::CompositeBackend;
pub use fts5::Fts5Backend;
pub use patterns::PatternsBackend;
pub use vector::VectorBackend;
