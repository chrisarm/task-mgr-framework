//! Database schema definitions for task-mgr.
//!
//! Defines the SQL schema for tasks, task relationships, runs, learnings,
//! and supporting tables. Uses `CREATE TABLE IF NOT EXISTS` for idempotent
//! schema creation.
//!
//! # Module Structure
//!
//! - `tasks` - Tasks, task_files, and task_relationships tables
//! - `runs` - Runs and run_tasks tables
//! - `learnings` - Learnings and learning_tags tables
//! - `metadata` - PRD metadata and global state tables

pub mod key_decisions;
mod learnings;
mod metadata;
mod runs;
mod tasks;

#[cfg(test)]
mod tests;

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Creates the complete database schema for task-mgr.
///
/// Creates the following tables (if they don't exist):
/// - `tasks` - Main task table with all fields from PRD user stories
/// - `task_files` - Files associated with each task (touchesFiles)
/// - `task_relationships` - Relationships between tasks (dependsOn, synergyWith, etc.)
/// - `runs` - Execution session tracking
/// - `run_tasks` - Task execution within runs
/// - `learnings` - Institutional memory
/// - `learning_tags` - Flexible categorization for learnings
/// - `prd_metadata` - PRD structure preservation
/// - `global_state` - Iteration counter and state tracking
///
/// Also creates indexes for common query patterns.
///
/// # Arguments
///
/// * `conn` - A reference to an open SQLite connection
///
/// # Errors
///
/// Returns an error if any DDL statement fails.
pub fn create_schema(conn: &Connection) -> TaskMgrResult<()> {
    // Create tasks-related tables
    tasks::create_tasks_table(conn)?;
    tasks::create_task_files_table(conn)?;
    tasks::create_task_relationships_table(conn)?;

    // Create runs-related tables
    runs::create_runs_table(conn)?;
    runs::create_run_tasks_table(conn)?;

    // Create learnings-related tables
    learnings::create_learnings_table(conn)?;
    learnings::create_learning_tags_table(conn)?;

    // Create metadata tables
    metadata::create_prd_metadata_table(conn)?;
    metadata::create_prd_files_table(conn)?;
    metadata::create_global_state_table(conn)?;
    metadata::initialize_global_state(conn)?;

    // Create all indexes
    tasks::create_tasks_indexes(conn)?;
    runs::create_runs_indexes(conn)?;
    learnings::create_learnings_indexes(conn)?;

    Ok(())
}
