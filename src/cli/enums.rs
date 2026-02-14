//! CLI enum definitions for task-mgr.
//!
//! This module contains all enum types used in CLI argument parsing,
//! including output formats, status filters, and learning-related enums.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Output format for CLI responses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable text output
    Text,
    /// JSON output for machine parsing
    Json,
}

/// Task status filter for list command
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
pub enum TaskStatusFilter {
    /// Tasks not yet started
    Todo,
    /// Tasks currently being worked on
    InProgress,
    /// Completed tasks
    Done,
    /// Tasks blocked by issues
    Blocked,
    /// Tasks intentionally skipped
    Skipped,
    /// Tasks no longer relevant
    Irrelevant,
}

impl std::fmt::Display for TaskStatusFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatusFilter::Todo => write!(f, "todo"),
            TaskStatusFilter::InProgress => write!(f, "in_progress"),
            TaskStatusFilter::Done => write!(f, "done"),
            TaskStatusFilter::Blocked => write!(f, "blocked"),
            TaskStatusFilter::Skipped => write!(f, "skipped"),
            TaskStatusFilter::Irrelevant => write!(f, "irrelevant"),
        }
    }
}

/// Fail status options (blocked is default)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum, Default)]
pub enum FailStatus {
    /// Task is blocked by an issue
    #[default]
    Blocked,
    /// Task was intentionally skipped
    Skipped,
    /// Task is no longer relevant
    Irrelevant,
}

/// Run end status options
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum RunEndStatus {
    /// Run completed successfully
    Completed,
    /// Run was aborted
    Aborted,
}

/// Learning outcome types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
pub enum LearningOutcome {
    /// Learning from a failure
    Failure,
    /// Learning from a success
    Success,
    /// A workaround for an issue
    Workaround,
    /// A general pattern discovered
    Pattern,
}

impl std::fmt::Display for LearningOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LearningOutcome::Failure => write!(f, "failure"),
            LearningOutcome::Success => write!(f, "success"),
            LearningOutcome::Workaround => write!(f, "workaround"),
            LearningOutcome::Pattern => write!(f, "pattern"),
        }
    }
}

/// Confidence level for learnings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
pub enum Confidence {
    /// High confidence - verified and reliable
    High,
    /// Medium confidence - likely correct but not fully verified
    Medium,
    /// Low confidence - tentative or uncertain
    Low,
}

/// Shell type for completions generation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum Shell {
    /// Bash shell
    Bash,
    /// Zsh shell
    Zsh,
    /// Fish shell
    Fish,
    /// PowerShell
    Powershell,
}
