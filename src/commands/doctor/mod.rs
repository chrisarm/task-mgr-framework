//! Doctor command implementation.
//!
//! The doctor command checks database health and fixes stale state:
//! - Stale in_progress tasks (no active run tracking them)
//! - Active runs without end (abandoned runs)
//! - Orphaned relationships (references to non-existent tasks)
//! - Tasks completed in git history but not marked done in DB (--reconcile-git)

mod checks;
mod fixes;
mod output;

#[cfg(test)]
mod tests;

pub use output::{
    format_doctor_verbose, format_text, DoctorResult, DoctorSummary, Fix, Issue, IssueType,
};

use std::path::Path;

use rusqlite::Connection;

use crate::commands::next::find_decay_warnings;
use crate::models::RunStatus;
use crate::TaskMgrResult;

use checks::{
    find_active_runs_without_end, find_git_reconciliation_tasks, find_orphaned_relationships,
    find_stale_in_progress_tasks,
};
use fixes::{fix_active_run, fix_git_reconciliation, fix_orphaned_relationship, fix_stale_task};

/// Check database health and optionally fix issues.
///
/// # Arguments
/// * `conn` - Database connection
/// * `auto_fix` - If true, automatically fix issues
/// * `dry_run` - If true, show what would be fixed without making changes (implies auto_fix for output)
/// * `decay_threshold` - Decay threshold in iterations (0 to disable decay checking)
/// * `reconcile_git` - If true, check git history for completed tasks not marked done
/// * `dir` - Project directory (used for git operations when reconcile_git is true)
///
/// # Returns
/// * `Ok(DoctorResult)` - Information about issues found and fixed
/// * `Err(TaskMgrError)` - If database error occurs
pub fn doctor(
    conn: &Connection,
    auto_fix: bool,
    dry_run: bool,
    decay_threshold: i64,
    reconcile_git: bool,
    dir: &Path,
) -> TaskMgrResult<DoctorResult> {
    let mut issues = Vec::new();
    let mut fixed = Vec::new();
    let mut would_fix = Vec::new();

    // In dry-run mode, we show what would be fixed even if auto_fix wasn't explicitly set
    let effective_auto_fix = auto_fix || dry_run;

    // Check for stale in_progress tasks
    let stale_tasks = find_stale_in_progress_tasks(conn)?;
    for (task_id, task_title) in &stale_tasks {
        issues.push(Issue {
            issue_type: IssueType::StaleInProgressTask,
            entity_id: task_id.clone(),
            description: format!(
                "Task '{}' ({}) is in_progress but has no active run tracking it",
                task_title, task_id
            ),
        });
    }

    // Check for active runs without end
    let active_runs = find_active_runs_without_end(conn)?;
    for (run_id, started_at) in &active_runs {
        issues.push(Issue {
            issue_type: IssueType::ActiveRunWithoutEnd,
            entity_id: run_id.clone(),
            description: format!(
                "Run '{}' started at {} is still active but appears abandoned",
                run_id, started_at
            ),
        });
    }

    // Check for orphaned relationships
    let orphaned = find_orphaned_relationships(conn)?;
    for (task_id, related_id, rel_type) in &orphaned {
        issues.push(Issue {
            issue_type: IssueType::OrphanedRelationship,
            entity_id: format!("{}->{}:{}", task_id, related_id, rel_type),
            description: format!(
                "Relationship from '{}' references non-existent task '{}'",
                task_id, related_id
            ),
        });
    }

    // Check for tasks approaching decay (warning only, 8 iterations before decay)
    const DECAY_WARNING_THRESHOLD: i64 = 8;
    let decay_warnings_list = if decay_threshold > 0 {
        find_decay_warnings(conn, decay_threshold, DECAY_WARNING_THRESHOLD)?
    } else {
        Vec::new()
    };
    for warning in &decay_warnings_list {
        issues.push(Issue {
            issue_type: IssueType::DecayWarning,
            entity_id: warning.task_id.clone(),
            description: format!(
                "Task '{}' ({}) is {} and will auto-reset to todo in {} iteration(s)",
                warning.task_title, warning.task_id, warning.status, warning.iterations_until_decay
            ),
        });
    }

    // Check for tasks completed in git but not marked done (only if --reconcile-git)
    let reconciliation_tasks = if reconcile_git {
        find_git_reconciliation_tasks(conn, dir)?
    } else {
        Vec::new()
    };
    for (task_id, task_title, commit_msg) in &reconciliation_tasks {
        let desc = if commit_msg.is_empty() {
            format!(
                "Task '{}' ({}) found in git commit history but not marked done",
                task_title, task_id
            )
        } else {
            format!(
                "Task '{}' ({}) found in git commit: {}",
                task_title, task_id, commit_msg
            )
        };
        issues.push(Issue {
            issue_type: IssueType::GitReconciliation,
            entity_id: task_id.clone(),
            description: desc,
        });
    }

    // Apply fixes (or preview what would be fixed in dry-run mode)
    if effective_auto_fix {
        // Fix stale in_progress tasks -> reset to todo
        for (task_id, _) in &stale_tasks {
            let fix = Fix {
                issue_type: IssueType::StaleInProgressTask,
                entity_id: task_id.clone(),
                action: "Reset task from 'in_progress' to 'todo' with audit note".to_string(),
            };
            if dry_run {
                would_fix.push(fix);
            } else {
                fix_stale_task(conn, task_id)?;
                fixed.push(fix);
            }
        }

        // Fix active runs -> mark as aborted
        for (run_id, _) in &active_runs {
            let fix = Fix {
                issue_type: IssueType::ActiveRunWithoutEnd,
                entity_id: run_id.clone(),
                action: format!(
                    "Marked run as '{}' with ended_at timestamp",
                    RunStatus::Aborted
                ),
            };
            if dry_run {
                would_fix.push(fix);
            } else {
                fix_active_run(conn, run_id)?;
                fixed.push(fix);
            }
        }

        // Fix orphaned relationships -> delete them
        for (task_id, related_id, rel_type) in &orphaned {
            let fix = Fix {
                issue_type: IssueType::OrphanedRelationship,
                entity_id: format!("{}->{}:{}", task_id, related_id, rel_type),
                action: "Deleted orphaned relationship".to_string(),
            };
            if dry_run {
                would_fix.push(fix);
            } else {
                fix_orphaned_relationship(conn, task_id, related_id, rel_type)?;
                fixed.push(fix);
            }
        }

        // Fix git reconciliation tasks -> mark as done
        for (task_id, _title, commit_msg) in &reconciliation_tasks {
            let fix = Fix {
                issue_type: IssueType::GitReconciliation,
                entity_id: task_id.clone(),
                action: "Marked task as 'done' from git history reconciliation".to_string(),
            };
            if dry_run {
                would_fix.push(fix);
            } else {
                fix_git_reconciliation(conn, task_id, commit_msg)?;
                fixed.push(fix);
            }
        }
    }

    let summary = DoctorSummary {
        stale_tasks: stale_tasks.len(),
        active_runs: active_runs.len(),
        orphaned_relationships: orphaned.len(),
        decay_warnings: decay_warnings_list.len(),
        reconciled: reconciliation_tasks.len(),
        total_issues: issues.len(),
        total_fixed: fixed.len(),
    };

    Ok(DoctorResult {
        issues,
        fixed,
        would_fix,
        auto_fix,
        dry_run,
        summary,
    })
}
