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
    Commands, CurateAction, DecisionAction, EnhanceCommand, MigrateAction, ModelsAction, RunAction,
    WorktreesAction,
};
pub use enums::{
    Confidence, FailStatus, LearningOutcome, OutputFormat, RunEndStatus, Shell, TaskStatusFilter,
};

/// Task Manager CLI - A standalone tool for managing AI agent loop tasks
/// with SQLite as working state.
#[derive(Parser, Debug)]
#[command(name = "task-mgr")]
#[command(
    author,
    version,
    about,
    long_about = "\
A standalone CLI tool for managing AI agent loop tasks with SQLite as working state.

task-mgr tracks tasks from PRD JSON files through their lifecycle (todo → in_progress → done),
manages autonomous agent loop sessions, records learnings from task outcomes, and provides
intelligent task prioritization using file-locality scoring and UCB bandit ranking."
)]
#[command(after_help = "\
COMMAND REFERENCE (by category):

  Task Management:
    init               Initialize database from a JSON PRD file
    list               List tasks with optional filtering
    show               Show detailed information about a single task
    next               Get the next recommended task to work on
    complete (done)    Mark one or more tasks as completed
    fail               Mark tasks as failed (blocked, skipped, or irrelevant)
    skip               Skip tasks intentionally (defer for later)
    irrelevant         Mark tasks as irrelevant (no longer needed)
    unblock            Return a blocked task to todo status
    unskip             Return a skipped task to todo status
    reset              Reset task(s) to todo status for re-running
    review             Review blocked and skipped tasks

  Run & Loop:
    run                Run lifecycle management (begin, update, end)
    loop               Run autonomous agent loop
    batch              Run multiple PRDs in sequence
    status             Show status dashboard for PRD projects
    stats              Show progress summary (counts, completion rate)
    history            Show run history

  Learnings:
    learn              Record a learning from a task outcome
    recall             Find relevant learnings for a task or query
    learnings          List all learnings
    apply-learning     Record that a learning was applied (confirmed useful)
    invalidate-learning  Invalidate a learning via two-step degradation
    delete-learning    Delete a learning from the database
    edit-learning      Edit an existing learning
    import-learnings   Import learnings from a progress.json or learnings file
    extract-learnings  Extract learnings from Claude output using LLM analysis

  Curation & Maintenance:
    curate             Curate learnings (retire, unretire, dedup, enrich)
    decisions          Manage key architectural decisions
    archive            Archive completed PRDs and extract learnings

  Utilities:
    doctor             Check database health and fix stale state
    export             Export database state to JSON
    migrate            Manage database schema migrations
    worktrees          Manage git worktrees (list, prune, remove)
    completions        Generate shell completions
    man-pages          Generate man pages

QUICK START:
    task-mgr init --from-json tasks/my-prd.json   # Import a PRD
    task-mgr status                                # Check progress
    task-mgr next                                  # Get next task
    task-mgr loop tasks/my-prd.json --yes          # Run agent loop

See 'task-mgr <command> --help' for detailed usage of each command.")]
pub struct Cli {
    /// Directory for task-mgr database files
    #[arg(long, env = "TASK_MGR_DIR", default_value = ".task-mgr", global = true)]
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
