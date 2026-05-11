pub mod cli;
pub mod commands;
pub mod db;
pub mod error;
pub mod git;
pub mod handlers;
pub mod learnings;
pub mod loop_engine;
pub mod models;
pub mod output;
pub mod paths;
pub mod util;

// Re-export commonly used error types
pub use error::{TaskMgrError, TaskMgrResult, validate_safe_path};
