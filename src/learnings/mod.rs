//! Learnings system for institutional memory.
//!
//! This module provides CRUD operations and recall functionality for learnings,
//! which capture knowledge from task execution to help future iterations.
//!
//! ## UCB Bandit Ranking (Phase 2)
//!
//! The bandit module implements a sliding-window UCB algorithm for ranking learnings,
//! balancing exploitation of proven learnings with exploration of new ones.
//! See [`bandit`] module for details.

pub mod bandit;
pub mod crud;
pub mod embeddings;
pub mod ingestion;
pub mod recall;
pub mod retrieval;
#[cfg(test)]
pub(crate) mod test_helpers;

pub use bandit::{
    WINDOW_SIZE, WindowStats, calculate_ucb_score, get_total_window_shows, get_window_stats,
    rank_learnings_by_ucb, record_learning_applied, record_learning_shown, refresh_sliding_window,
};
pub use crud::{
    DeleteLearningResult, EditLearningParams, EditLearningResult, LearningWriter,
    RecordLearningParams, RecordLearningResult, apply_supersession, delete_learning, edit_learning,
    ensure_learning_exists, format_delete_text, format_edit_text, get_learning, get_learning_tags,
    record_learning,
};
pub use recall::{
    RecallParams, RecallResult, ScoredLearningOutput, ScoredRecallResult,
    format_text as format_recall_text, recall_learnings, recall_learnings_scored,
    recall_learnings_with_backend,
};
pub use retrieval::{
    CompositeBackend, Fts5Backend, PatternsBackend, RetrievalBackend, RetrievalQuery,
    ScoredLearning, VectorBackend,
};
