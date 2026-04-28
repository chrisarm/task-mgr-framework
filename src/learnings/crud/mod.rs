//! CRUD operations for learnings.
//!
//! This module provides functions for creating, reading, updating, and deleting
//! learnings in the institutional memory system.
//!
//! ## Module Organization
//!
//! - [`types`]: Parameter and result structs for all CRUD operations
//! - [`create`]: Create operations (`record_learning`)
//! - [`read`]: Read operations (`get_learning`, `get_learning_tags`)
//! - [`update`]: Update operations (`edit_learning`)
//! - [`delete`]: Delete operations (`delete_learning`)
//! - [`output`]: Text formatting functions for operation results

mod create;
mod delete;
mod output;
mod read;
#[cfg(test)]
mod tests;
mod types;
mod update;
pub mod writer;

// Re-export all public items for convenience
pub use create::record_learning;
pub use delete::delete_learning;
pub use output::{format_delete_text, format_edit_text};
pub use read::{ensure_learning_exists, get_learning, get_learning_tags};
pub use types::{
    DeleteLearningResult, EditLearningParams, EditLearningResult, RecordLearningParams,
    RecordLearningResult,
};
pub use update::{apply_supersession, edit_learning};
pub use writer::LearningWriter;
