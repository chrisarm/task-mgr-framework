pub mod cli;
pub mod commands;
pub mod db;
pub mod error;
pub mod git;
pub mod handlers;
pub mod learnings;
pub mod lifecycle;
pub mod loop_engine;
pub mod models;
pub mod observability;
pub mod output;
pub mod paths;
pub mod util;

// Re-export commonly used error types
pub use error::{TaskMgrError, TaskMgrResult, validate_safe_path};

/// Process-wide serialization for tests that observe `TASK_MGR_ACTIVE_PREFIX`.
///
/// Any test (in any module) that reads OR writes this env var must hold this
/// mutex for the full duration — including across the RAII guard's Drop, which
/// can restore a prior value. Using a single crate-level mutex prevents
/// `add.rs` tests from racing with `claude.rs` tests that also touch the var.
#[cfg(test)]
pub(crate) static ENV_PREFIX_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
