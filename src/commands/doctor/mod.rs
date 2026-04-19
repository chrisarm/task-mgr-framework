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
pub mod setup_checks;
pub mod setup_fixes;
pub mod setup_output;

#[cfg(test)]
mod tests;

pub use output::{
    DoctorResult, DoctorSummary, Fix, Issue, IssueType, format_doctor_verbose, format_text,
};
pub use setup_checks::EXPECTED_SKILLS;
pub use setup_fixes::{
    detect_additional_tools, fix_generate_claude_md, fix_generate_project_config,
    fix_install_skills, fix_patch_hook,
};
pub use setup_output::{CheckContext, SetupAuditResult, format_setup_text};

use std::path::Path;

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::commands::next::find_decay_warnings;
use crate::models::RunStatus;

use checks::{
    find_active_runs_without_end, find_git_reconciliation_tasks, find_orphaned_relationships,
    find_stale_in_progress_tasks, has_active_loop_lock,
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

    // Check for active runs without end.
    // Skip this category entirely if a loop lock is held — those runs belong
    // to a live loop and aborting them would crash the running session.
    let loop_is_running = has_active_loop_lock(dir);
    let active_runs = if loop_is_running {
        eprintln!("Note: skipping active-run checks — a loop is currently running");
        Vec::new()
    } else {
        find_active_runs_without_end(conn)?
    };
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
        find_decay_warnings(conn, decay_threshold, DECAY_WARNING_THRESHOLD, None)?
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

/// Run all setup checks against the Claude Code configuration for this project.
///
/// Derives paths from `project_dir` (the project root, not `.task-mgr`):
/// - `~/.claude/settings.json` — defaultMode and deny-conflict checks
/// - `~/.claude/hooks/guard-destructive.sh` — hook bypass check
/// - `~/.claude/commands/` — skill installation checks
/// - `<project_dir>/.task-mgr/config.json` — project config check
/// - `<project_dir>/CLAUDE.md` — documentation check
///
/// The home directory is read from the `HOME` environment variable.
///
/// # Auto-fix behaviour
/// When `auto_fix` is `true`, repairs are applied for every `auto_fixable`
/// check that is not already passing.  **`~/.claude/settings.json` is never
/// modified automatically** — the `fix_command` suggestion for those checks
/// is printed to the user via the normal check output.
///
/// # Returns
/// A `SetupAuditResult` with one `SetupCheck` per check run, plus applied
/// `SetupFix` entries when `auto_fix` is true.
pub fn audit_setup(project_dir: &Path, auto_fix: bool) -> SetupAuditResult {
    let home = std::env::var("HOME").unwrap_or_default();
    let claude_dir = std::path::PathBuf::from(&home).join(".claude");
    audit_setup_with_claude_dir(project_dir, &claude_dir, auto_fix)
}

/// Inner implementation of `audit_setup` with an explicit `claude_dir`.
///
/// Used directly by tests to avoid depending on the `HOME` environment variable.
fn audit_setup_with_claude_dir(
    project_dir: &Path,
    claude_dir: &Path,
    auto_fix: bool,
) -> SetupAuditResult {
    use setup_checks::{EXPECTED_SKILLS, default_registry};
    use setup_fixes::{
        detect_additional_tools, fix_generate_claude_md, fix_generate_project_config,
        fix_install_skills, fix_patch_hook,
    };
    use setup_output::SetupSeverity;

    let ctx = setup_output::CheckContext {
        settings_path: claude_dir.join("settings.json"),
        hook_path: claude_dir.join("hooks").join("guard-destructive.sh"),
        commands_dir: claude_dir.join("commands"),
        db_dir: project_dir.join(".task-mgr"),
        project_dir: project_dir.to_path_buf(),
    };
    let local_skills_dir = project_dir.join(".claude").join("commands");

    let registry = default_registry();
    let checks = registry.run_all(&ctx);

    if !auto_fix {
        return SetupAuditResult::new(checks);
    }

    // ── Apply auto-fixes for `auto_fixable` checks that are not passing ──

    let mut applied_fixes = Vec::new();

    // Fix missing skills (copy from local .claude/commands/ to ~/.claude/commands/).
    let missing_skills: Vec<&str> = EXPECTED_SKILLS
        .iter()
        .filter(|&&skill| !ctx.commands_dir.join(format!("{skill}.md")).exists())
        .copied()
        .collect();
    if !missing_skills.is_empty() {
        applied_fixes.extend(fix_install_skills(
            &local_skills_dir,
            &ctx.commands_dir,
            &missing_skills,
        ));
    }

    // Fix missing project config.
    let needs_config = checks
        .iter()
        .any(|c| c.name == "project_config" && c.severity != SetupSeverity::Pass);
    if needs_config {
        let detected = detect_additional_tools(&ctx.project_dir);
        applied_fixes.push(fix_generate_project_config(&ctx.db_dir, &detected));
    }

    // Fix hook missing bypass.
    let needs_hook_patch = checks
        .iter()
        .any(|c| c.name == "hook_bypass" && c.auto_fixable && c.severity != SetupSeverity::Pass);
    if needs_hook_patch {
        applied_fixes.push(fix_patch_hook(&ctx.hook_path));
    }

    // Fix missing CLAUDE.md.
    let needs_claude_md = checks
        .iter()
        .any(|c| c.name == "claude_md" && c.severity != SetupSeverity::Pass);
    if needs_claude_md {
        let db_path = ctx.db_dir.join("tasks.db");
        applied_fixes.push(fix_generate_claude_md(&ctx.project_dir, &db_path));
    }

    // Re-run checks so the result reflects the post-fix state.
    let updated_checks = registry.run_all(&ctx);

    SetupAuditResult::new_with_fixes(updated_checks, applied_fixes)
}

// ─── End-to-end audit_setup tests ─────────────────────────────────────────────

#[cfg(test)]
mod audit_setup_tests {
    use super::*;
    use setup_checks::EXPECTED_SKILLS;
    use setup_output::SetupSeverity;
    use tempfile::TempDir;

    /// Create a minimal Claude dir: settings.json and hooks dir (no hook file).
    fn make_claude_dir(base: &std::path::Path) -> std::path::PathBuf {
        let claude_dir = base.join(".claude");
        std::fs::create_dir_all(claude_dir.join("hooks")).unwrap();
        std::fs::create_dir_all(claude_dir.join("commands")).unwrap();
        claude_dir
    }

    /// Write a settings.json with a safe defaultMode and empty deny list.
    fn write_clean_settings(claude_dir: &std::path::Path) {
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"permissions":{"defaultMode":"auto","deny":[]}}"#,
        )
        .unwrap();
    }

    // ─── Test: full audit on a clean (unconfigured) project ──────────────────

    /// A clean project (no settings, no hook, no skills, no config.json, no
    /// CLAUDE.md) should have zero Blockers and produce the expected Warnings
    /// and Info findings.
    #[test]
    fn test_full_audit_clean_project_reports_expected_warnings() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        // Bare claude_dir: no settings.json, no hook, no skills installed.
        let claude_dir = make_claude_dir(home.path());

        // No .task-mgr/ dir and no CLAUDE.md in project.
        let db_dir = project.path().join(".task-mgr");
        std::fs::create_dir_all(&db_dir).unwrap();

        let result = audit_setup_with_claude_dir(project.path(), &claude_dir, false);

        // No settings.json → defaultMode check is Pass (safe default).
        // No hook file → hook_bypass is Pass (nothing to bypass).
        // All 6 expected skills are missing → 6 Warnings.
        // No config.json → 1 Warning.
        // No CLAUDE.md → 1 Info.

        assert!(
            !result.has_blockers(),
            "clean project must have no blockers: {:#?}",
            result.checks
        );

        let warn_count = result.warning_count();
        assert!(
            warn_count > EXPECTED_SKILLS.len(),
            "expected ≥ {} warnings (skills + config.json), got {warn_count}",
            EXPECTED_SKILLS.len() + 1
        );

        let info_count = result
            .checks
            .iter()
            .filter(|c| c.severity == SetupSeverity::Info)
            .count();
        assert_eq!(
            info_count, 1,
            "missing CLAUDE.md must produce exactly 1 Info"
        );
    }

    // ─── Test: full audit on a correctly configured project ──────────────────

    /// A fully configured project must report all Pass — no Warnings, no
    /// Blockers, no Info findings.
    #[test]
    fn test_full_audit_configured_project_all_pass() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        let claude_dir = make_claude_dir(home.path());
        write_clean_settings(&claude_dir);

        // Install all expected skills.
        let commands_dir = claude_dir.join("commands");
        for skill in EXPECTED_SKILLS {
            std::fs::write(commands_dir.join(format!("{skill}.md")), "# skill").unwrap();
        }

        // Install a hook with the bypass.
        std::fs::write(
            claude_dir.join("hooks").join("guard-destructive.sh"),
            "#!/bin/bash\n[ -n \"$LOOP_ALLOW_DESTRUCTIVE\" ] && exit 0\necho guard\n",
        )
        .unwrap();

        // Project config and CLAUDE.md.
        let db_dir = project.path().join(".task-mgr");
        std::fs::create_dir_all(&db_dir).unwrap();
        std::fs::write(db_dir.join("config.json"), "{}").unwrap();
        std::fs::write(project.path().join("CLAUDE.md"), "# project").unwrap();

        let result = audit_setup_with_claude_dir(project.path(), &claude_dir, false);

        assert!(
            !result.has_blockers(),
            "configured project must have no blockers"
        );
        assert_eq!(
            result.warning_count(),
            0,
            "configured project must have no warnings"
        );
        let non_pass = result
            .checks
            .iter()
            .filter(|c| c.severity != SetupSeverity::Pass)
            .count();
        assert_eq!(
            non_pass, 0,
            "every check must pass on a correctly configured project: {:#?}",
            result.checks
        );
    }

    // ─── Test: auto-fix then re-audit shows fixed issues now pass ─────────────

    /// After running auto-fix, a re-audit must report Pass for every issue that
    /// was auto-fixable.  The items that cannot be auto-fixed (settings.json
    /// problems) are not present here — this test uses a clean settings.json.
    #[test]
    fn test_auto_fix_then_reaudit_shows_fixed_issues_pass() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        let claude_dir = make_claude_dir(home.path());
        write_clean_settings(&claude_dir);

        // Provide source skills in local .claude/commands/ so auto-fix can copy.
        let local_commands = project.path().join(".claude").join("commands");
        std::fs::create_dir_all(&local_commands).unwrap();
        for skill in EXPECTED_SKILLS {
            std::fs::write(local_commands.join(format!("{skill}.md")), "# skill").unwrap();
        }

        // Provide a hook that needs patching.
        let hook_path = claude_dir.join("hooks").join("guard-destructive.sh");
        std::fs::write(&hook_path, "#!/bin/bash\necho guard\n").unwrap();

        // Create .task-mgr/ dir (so the fix can write config.json there).
        let db_dir = project.path().join(".task-mgr");
        std::fs::create_dir_all(&db_dir).unwrap();

        // Run auto-fix.
        let result = audit_setup_with_claude_dir(project.path(), &claude_dir, true);

        // Fixes must have been applied.
        assert!(
            !result.fixes.is_empty(),
            "auto-fix should apply at least one fix"
        );

        // The checks in the result are the post-fix re-run — auto-fixable
        // checks that were failing should now be Pass.
        let auto_fixable_non_pass: Vec<_> = result
            .checks
            .iter()
            .filter(|c| c.auto_fixable && c.severity != SetupSeverity::Pass)
            .collect();
        assert!(
            auto_fixable_non_pass.is_empty(),
            "all auto-fixable checks must pass after auto-fix: {auto_fixable_non_pass:#?}"
        );
    }

    // ─── Test: auto-fix is idempotent ─────────────────────────────────────────

    /// Running auto-fix twice must produce the same end state as running it once.
    /// The second run must not fail and must not produce more fixes.
    #[test]
    fn test_auto_fix_is_idempotent() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        let claude_dir = make_claude_dir(home.path());
        write_clean_settings(&claude_dir);

        // Provide source skills so auto-fix can copy them.
        let local_commands = project.path().join(".claude").join("commands");
        std::fs::create_dir_all(&local_commands).unwrap();
        for skill in EXPECTED_SKILLS {
            std::fs::write(local_commands.join(format!("{skill}.md")), "# skill").unwrap();
        }

        // Provide a hook that needs patching.
        let hook_path = claude_dir.join("hooks").join("guard-destructive.sh");
        std::fs::write(&hook_path, "#!/bin/bash\necho guard\n").unwrap();

        let db_dir = project.path().join(".task-mgr");
        std::fs::create_dir_all(&db_dir).unwrap();

        // First auto-fix run.
        let result1 = audit_setup_with_claude_dir(project.path(), &claude_dir, true);

        // Second auto-fix run — should be idempotent.
        let result2 = audit_setup_with_claude_dir(project.path(), &claude_dir, true);

        // After the second run, all fixes must succeed (idempotent ops return success).
        let failed_fixes: Vec<_> = result2.fixes.iter().filter(|f| !f.success).collect();
        assert!(
            failed_fixes.is_empty(),
            "second auto-fix run must not produce failures: {failed_fixes:#?}"
        );

        // The set of non-passing checks must be the same after both runs.
        let non_pass1: Vec<_> = result1
            .checks
            .iter()
            .filter(|c| c.severity != SetupSeverity::Pass)
            .map(|c| &c.name)
            .collect();
        let non_pass2: Vec<_> = result2
            .checks
            .iter()
            .filter(|c| c.severity != SetupSeverity::Pass)
            .map(|c| &c.name)
            .collect();
        assert_eq!(
            non_pass1, non_pass2,
            "idempotent: non-passing checks must be the same after both runs"
        );
    }
}
