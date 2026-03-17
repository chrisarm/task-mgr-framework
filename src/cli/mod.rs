//! CLI definitions for task-mgr.
//!
//! This module defines the command-line interface using clap derive macros,
//! including the main CLI struct, all subcommands, and associated enums.
//!
//! # Module Structure
//!
//! - `enums`: Output format, status filters, and learning-related enums
//! - `commands`: Commands enum and all subcommand definitions
//! - `tests`: Unit tests for CLI argument parsing

use std::path::PathBuf;

use clap::Parser;

pub mod commands;
pub mod enums;

#[cfg(test)]
mod tests;

// Re-export commonly used types at the module level for convenience
pub use commands::{
    Commands, CurateAction, DecisionAction, MigrateAction, RunAction, WorktreesAction,
};
pub use enums::{
    Confidence, FailStatus, LearningOutcome, OutputFormat, RunEndStatus, Shell, TaskStatusFilter,
};

/// Task Manager CLI - A standalone tool for managing AI agent loop tasks
/// with SQLite as working state.
#[derive(Parser, Debug)]
#[command(name = "task-mgr")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Directory for task-mgr database files
    #[arg(long, default_value = ".task-mgr", global = true)]
    pub dir: PathBuf,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, global = true)]
    pub format: OutputFormat,

    /// Enable verbose output (show detailed debug information)
    #[arg(short = 'v', long, global = true, default_value_t = false)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}
