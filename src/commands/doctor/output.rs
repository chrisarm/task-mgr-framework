//! Output types and formatting for the doctor command.
//!
//! Contains:
//! - IssueType enum for categorizing detected issues
//! - Issue struct for individual issues
//! - Fix struct for applied fixes
//! - DoctorResult and DoctorSummary for command results
//! - Text formatting functions

use serde::Serialize;

/// Types of issues the doctor can detect.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    /// Task stuck in in_progress with no active run
    StaleInProgressTask,
    /// Run left in active state without being ended
    ActiveRunWithoutEnd,
    /// Relationship references a non-existent task
    OrphanedRelationship,
    /// Task approaching automatic decay (warning, not an error)
    DecayWarning,
    /// Task completed in git history but not marked done in DB
    GitReconciliation,
}

impl std::fmt::Display for IssueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueType::StaleInProgressTask => write!(f, "stale_in_progress_task"),
            IssueType::ActiveRunWithoutEnd => write!(f, "active_run_without_end"),
            IssueType::OrphanedRelationship => write!(f, "orphaned_relationship"),
            IssueType::DecayWarning => write!(f, "decay_warning"),
            IssueType::GitReconciliation => write!(f, "git_reconciliation"),
        }
    }
}

/// A single issue found by the doctor.
#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    /// Type of issue
    pub issue_type: IssueType,
    /// ID of the affected entity (task_id, run_id, or relationship description)
    pub entity_id: String,
    /// Human-readable description of the issue
    pub description: String,
}

/// A fix that was applied by the doctor.
#[derive(Debug, Clone, Serialize)]
pub struct Fix {
    /// Type of issue that was fixed
    pub issue_type: IssueType,
    /// ID of the affected entity
    pub entity_id: String,
    /// Description of what was done
    pub action: String,
}

/// Result of running the doctor command.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorResult {
    /// Issues found during the check
    pub issues: Vec<Issue>,
    /// Fixes applied (only populated if auto_fix was true and dry_run was false)
    pub fixed: Vec<Fix>,
    /// Fixes that would be applied (only populated if dry_run is true)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub would_fix: Vec<Fix>,
    /// Whether auto_fix mode was enabled
    pub auto_fix: bool,
    /// Whether this was a dry run (no changes made)
    pub dry_run: bool,
    /// Summary counts
    pub summary: DoctorSummary,
}

/// Summary counts for the doctor result.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorSummary {
    /// Number of stale in_progress tasks found
    pub stale_tasks: usize,
    /// Number of active runs without end found
    pub active_runs: usize,
    /// Number of orphaned relationships found
    pub orphaned_relationships: usize,
    /// Number of tasks approaching decay (warnings only, not errors)
    pub decay_warnings: usize,
    /// Number of tasks reconciled from git history
    pub reconciled: usize,
    /// Total issues found
    pub total_issues: usize,
    /// Total issues fixed
    pub total_fixed: usize,
}

/// Format verbose output for the doctor command (to stderr).
///
/// Returns a string that should be written to stderr when --verbose is enabled.
#[must_use]
pub fn format_doctor_verbose(result: &DoctorResult) -> String {
    let mut output = String::new();

    output.push_str("[verbose] Doctor Command - Health Checks Performed:\n");
    output.push_str(&format!("{}\n", "-".repeat(60)));

    // Check 1: Stale in_progress tasks
    output.push_str("[verbose] Check 1: Stale in_progress tasks\n");
    output.push_str("  Query: Tasks in 'in_progress' with no active run tracking them\n");
    output.push_str(&format!(
        "  Found: {} issue(s)\n",
        result.summary.stale_tasks
    ));
    if result.summary.stale_tasks > 0 {
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::StaleInProgressTask)
        {
            output.push_str(&format!("    - {}\n", issue.entity_id));
        }
    }

    // Check 2: Active runs without end
    output.push_str("\n[verbose] Check 2: Active runs without end\n");
    output.push_str("  Query: Runs with status='active' and no ended_at timestamp\n");
    output.push_str(&format!(
        "  Found: {} issue(s)\n",
        result.summary.active_runs
    ));
    if result.summary.active_runs > 0 {
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::ActiveRunWithoutEnd)
        {
            output.push_str(&format!("    - {}\n", issue.entity_id));
        }
    }

    // Check 3: Orphaned relationships
    output.push_str("\n[verbose] Check 3: Orphaned relationships\n");
    output.push_str("  Query: Relationships where related_id references non-existent task\n");
    output.push_str(&format!(
        "  Found: {} issue(s)\n",
        result.summary.orphaned_relationships
    ));
    if result.summary.orphaned_relationships > 0 {
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::OrphanedRelationship)
        {
            output.push_str(&format!("    - {}\n", issue.entity_id));
        }
    }

    // Check 4: Git reconciliation (only shown if reconcile-git was enabled)
    if result.summary.reconciled > 0 {
        output.push_str("\n[verbose] Check 4: Git reconciliation\n");
        output.push_str("  Query: Tasks completed in git history but not marked done in DB\n");
        output.push_str(&format!(
            "  Found: {} task(s) to reconcile\n",
            result.summary.reconciled
        ));
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::GitReconciliation)
        {
            output.push_str(&format!("    - {}\n", issue.entity_id));
        }
    }

    output.push_str(&format!("\n{}\n", "-".repeat(60)));

    // Summary
    output.push_str(&format!(
        "[verbose] Summary: {} total issue(s) found\n",
        result.summary.total_issues
    ));

    // Mode info
    if result.dry_run {
        output.push_str("[verbose] Mode: DRY RUN (no changes will be made)\n");
    } else if result.auto_fix {
        output.push_str(&format!(
            "[verbose] Mode: AUTO FIX ({} issue(s) fixed)\n",
            result.summary.total_fixed
        ));
    } else {
        output.push_str("[verbose] Mode: CHECK ONLY (use --auto-fix to repair)\n");
    }

    output
}

/// Format doctor result as human-readable text.
#[must_use]
pub fn format_text(result: &DoctorResult) -> String {
    let mut output = String::new();

    output.push_str("=== Database Health Check ===\n\n");

    if result.issues.is_empty() {
        output.push_str("✓ No issues found. Database is healthy.\n");
        return output;
    }

    // List issues by type
    output.push_str(&format!(
        "Found {} issue(s):\n\n",
        result.summary.total_issues
    ));

    if result.summary.stale_tasks > 0 {
        output.push_str(&format!(
            "Stale in_progress tasks ({})\n",
            result.summary.stale_tasks
        ));
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::StaleInProgressTask)
        {
            output.push_str(&format!("  - {}: {}\n", issue.entity_id, issue.description));
        }
        output.push('\n');
    }

    if result.summary.active_runs > 0 {
        output.push_str(&format!(
            "Active runs without end ({})\n",
            result.summary.active_runs
        ));
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::ActiveRunWithoutEnd)
        {
            output.push_str(&format!("  - {}: {}\n", issue.entity_id, issue.description));
        }
        output.push('\n');
    }

    if result.summary.orphaned_relationships > 0 {
        output.push_str(&format!(
            "Orphaned relationships ({})\n",
            result.summary.orphaned_relationships
        ));
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::OrphanedRelationship)
        {
            output.push_str(&format!("  - {}\n", issue.description));
        }
        output.push('\n');
    }

    if result.summary.decay_warnings > 0 {
        output.push_str(&format!(
            "⚠ Tasks approaching decay ({}) [warning only]\n",
            result.summary.decay_warnings
        ));
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::DecayWarning)
        {
            output.push_str(&format!("  - {}\n", issue.description));
        }
        output.push('\n');
    }

    if result.summary.reconciled > 0 {
        output.push_str(&format!(
            "Git reconciliation ({})\n",
            result.summary.reconciled
        ));
        for issue in result
            .issues
            .iter()
            .filter(|i| i.issue_type == IssueType::GitReconciliation)
        {
            output.push_str(&format!("  - {}: {}\n", issue.entity_id, issue.description));
        }
        output.push('\n');
    }

    // List would-be fixes in dry-run mode
    if result.dry_run && !result.would_fix.is_empty() {
        output.push_str(&format!(
            "[DRY RUN] Would fix {} issue(s):\n",
            result.would_fix.len()
        ));
        for fix in &result.would_fix {
            output.push_str(&format!("  → {}: {}\n", fix.entity_id, fix.action));
        }
        output.push('\n');
        output.push_str("No changes were made. Run without --dry-run to apply fixes.\n");
    } else if !result.fixed.is_empty() {
        // List actual fixes
        output.push_str(&format!("Fixed {} issue(s):\n", result.summary.total_fixed));
        for fix in &result.fixed {
            output.push_str(&format!("  ✓ {}: {}\n", fix.entity_id, fix.action));
        }
        output.push('\n');
    } else if result.auto_fix && !result.dry_run {
        output.push_str("No issues required fixing.\n");
    } else if !result.dry_run {
        output.push_str("Run with --auto-fix to automatically repair these issues.\n");
    }

    output
}
