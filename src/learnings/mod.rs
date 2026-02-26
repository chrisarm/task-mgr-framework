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
pub mod ingestion;
pub mod recall;
pub mod retrieval;
#[cfg(test)]
pub(crate) mod test_helpers;

pub use bandit::{
    calculate_ucb_score, get_total_window_shows, get_window_stats, rank_learnings_by_ucb,
    record_learning_applied, record_learning_shown, refresh_sliding_window, WindowStats,
    WINDOW_SIZE,
};
pub use crud::{
    delete_learning, edit_learning, format_delete_text, format_edit_text, get_learning,
    get_learning_tags, record_learning, DeleteLearningResult, EditLearningParams,
    EditLearningResult, RecordLearningParams, RecordLearningResult,
};
pub use recall::{
    format_text as format_recall_text, recall_learnings, recall_learnings_with_backend,
    RecallParams, RecallResult,
};
pub use retrieval::{
    CompositeBackend, Fts5Backend, PatternsBackend, RetrievalBackend, RetrievalQuery,
    ScoredLearning,
};
