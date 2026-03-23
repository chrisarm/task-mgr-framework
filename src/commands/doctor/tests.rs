//! Tests for the doctor command.

use super::*;
use crate::db::migrations::run_migrations;
use crate::db::{create_schema, open_connection};
use tempfile::TempDir;

fn setup_test_db() -> (TempDir, rusqlite::Connection) {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (temp_dir, conn)
}

fn insert_test_task(conn: &rusqlite::Connection, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?, 'Test Task', ?, 10)",
        rusqlite::params![id, status],
    )
    .unwrap();
}

// ============ find_stale_in_progress_tasks tests ============

#[test]
fn test_no_stale_tasks_when_healthy() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "todo");
    insert_test_task(&conn, "US-002", "done");

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();
    assert_eq!(result.summary.stale_tasks, 0);
}

#[test]
fn test_detects_stale_in_progress_task() {
    let (tmp_dir, conn) = setup_test_db();
    // Create an in_progress task with no active run
    insert_test_task(&conn, "US-001", "in_progress");

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.stale_tasks, 1);
    assert_eq!(result.issues[0].issue_type, IssueType::StaleInProgressTask);
    assert_eq!(result.issues[0].entity_id, "US-001");
}

#[test]
fn test_in_progress_task_with_active_run_is_not_stale() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "in_progress");

    // Create an active run tracking this task
    conn.execute(
        "INSERT INTO runs (run_id, status, started_at) VALUES ('run-1', 'active', datetime('now'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', 'US-001', 'started', 1)",
        [],
    )
    .unwrap();

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();
    assert_eq!(result.summary.stale_tasks, 0);
}

#[test]
fn test_in_progress_task_with_completed_run_is_stale() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "in_progress");

    // Create a completed run (not active)
    conn.execute(
        "INSERT INTO runs (run_id, status, started_at, ended_at) VALUES ('run-1', 'completed', datetime('now'), datetime('now'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-1', 'US-001', 'started', 1)",
        [],
    )
    .unwrap();

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();
    assert_eq!(result.summary.stale_tasks, 1);
}

// ============ find_active_runs_without_end tests ============

#[test]
fn test_detects_active_run_without_end() {
    let (tmp_dir, conn) = setup_test_db();

    // Create an active run without ended_at
    conn.execute(
        "INSERT INTO runs (run_id, status, started_at) VALUES ('run-orphan', 'active', datetime('now'))",
        [],
    )
    .unwrap();

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.active_runs, 1);
    assert!(result
        .issues
        .iter()
        .any(|i| i.issue_type == IssueType::ActiveRunWithoutEnd && i.entity_id == "run-orphan"));
}

#[test]
fn test_completed_runs_not_flagged() {
    let (tmp_dir, conn) = setup_test_db();

    // Create a completed run
    conn.execute(
        "INSERT INTO runs (run_id, status, started_at, ended_at) VALUES ('run-done', 'completed', datetime('now'), datetime('now'))",
        [],
    )
    .unwrap();

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();
    assert_eq!(result.summary.active_runs, 0);
}

// ============ find_orphaned_relationships tests ============

#[test]
fn test_detects_orphaned_relationship() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "todo");

    // Create a relationship to a non-existent task
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-001', 'US-NONEXISTENT', 'dependsOn')",
        [],
    )
    .unwrap();

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.orphaned_relationships, 1);
    assert!(result.issues.iter().any(|i| {
        i.issue_type == IssueType::OrphanedRelationship && i.entity_id.contains("US-NONEXISTENT")
    }));
}

#[test]
fn test_valid_relationships_not_flagged() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "todo");
    insert_test_task(&conn, "US-002", "todo");

    // Create a valid relationship
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-002', 'US-001', 'dependsOn')",
        [],
    )
    .unwrap();

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();
    assert_eq!(result.summary.orphaned_relationships, 0);
}

// ============ auto_fix tests ============

#[test]
fn test_auto_fix_resets_stale_task() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "in_progress");

    let result = doctor(&conn, true, false, 0, false, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.total_fixed, 1);
    assert!(result.fixed.iter().any(|f| f.entity_id == "US-001"));

    // Verify task status was reset
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "todo");

    // Verify audit note was added
    let notes: String = conn
        .query_row("SELECT notes FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(notes.contains("[DOCTOR]"));
}

#[test]
fn test_auto_fix_aborts_active_run() {
    let (tmp_dir, conn) = setup_test_db();

    conn.execute(
        "INSERT INTO runs (run_id, status, started_at) VALUES ('run-stuck', 'active', datetime('now'))",
        [],
    )
    .unwrap();

    let result = doctor(&conn, true, false, 0, false, tmp_dir.path()).unwrap();

    assert!(result
        .fixed
        .iter()
        .any(|f| f.entity_id == "run-stuck" && f.issue_type == IssueType::ActiveRunWithoutEnd));

    // Verify run was aborted
    let status: String = conn
        .query_row(
            "SELECT status FROM runs WHERE run_id = 'run-stuck'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "aborted");

    // Verify ended_at was set
    let ended_at: Option<String> = conn
        .query_row(
            "SELECT ended_at FROM runs WHERE run_id = 'run-stuck'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ended_at.is_some());
}

#[test]
fn test_auto_fix_deletes_orphaned_relationship() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "todo");

    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-001', 'GHOST', 'dependsOn')",
        [],
    )
    .unwrap();

    let result = doctor(&conn, true, false, 0, false, tmp_dir.path()).unwrap();

    assert!(result
        .fixed
        .iter()
        .any(|f| f.issue_type == IssueType::OrphanedRelationship));

    // Verify relationship was deleted
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'US-001' AND related_id = 'GHOST'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_auto_fix_also_fixes_run_tasks() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "in_progress");

    // Create an active run with a started run_task
    conn.execute(
        "INSERT INTO runs (run_id, status, started_at) VALUES ('run-stuck', 'active', datetime('now'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, status, iteration, started_at) VALUES ('run-stuck', 'US-001', 'started', 1, datetime('now'))",
        [],
    )
    .unwrap();

    doctor(&conn, true, false, 0, false, tmp_dir.path()).unwrap();

    // Verify run_tasks was also marked as failed
    let run_task_status: String = conn
        .query_row(
            "SELECT status FROM run_tasks WHERE run_id = 'run-stuck' AND task_id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(run_task_status, "failed");
}

// ============ no auto_fix tests ============

#[test]
fn test_without_auto_fix_no_changes() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "in_progress");

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.stale_tasks, 1);
    assert!(result.fixed.is_empty());
    assert!(!result.auto_fix);

    // Verify task status was NOT changed
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "in_progress");
}

// ============ healthy database tests ============

#[test]
fn test_healthy_database() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "todo");
    insert_test_task(&conn, "US-002", "done");

    // Create a properly completed run
    conn.execute(
        "INSERT INTO runs (run_id, status, started_at, ended_at) VALUES ('run-good', 'completed', datetime('now'), datetime('now'))",
        [],
    )
    .unwrap();

    // Create valid relationships
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-002', 'US-001', 'dependsOn')",
        [],
    )
    .unwrap();

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.total_issues, 0);
    assert!(result.issues.is_empty());
}

// ============ format_text tests ============

#[test]
fn test_format_text_healthy() {
    let result = DoctorResult {
        issues: vec![],
        fixed: vec![],
        would_fix: vec![],
        auto_fix: false,
        dry_run: false,
        summary: DoctorSummary {
            stale_tasks: 0,
            active_runs: 0,
            orphaned_relationships: 0,
            decay_warnings: 0,
            reconciled: 0,
            total_issues: 0,
            total_fixed: 0,
        },
    };

    let text = format_text(&result);
    assert!(text.contains("No issues found"));
    assert!(text.contains("healthy"));
}

#[test]
fn test_format_text_with_issues() {
    let result = DoctorResult {
        issues: vec![Issue {
            issue_type: IssueType::StaleInProgressTask,
            entity_id: "US-001".to_string(),
            description: "Task is stale".to_string(),
        }],
        fixed: vec![],
        would_fix: vec![],
        auto_fix: false,
        dry_run: false,
        summary: DoctorSummary {
            stale_tasks: 1,
            active_runs: 0,
            orphaned_relationships: 0,
            decay_warnings: 0,
            reconciled: 0,
            total_issues: 1,
            total_fixed: 0,
        },
    };

    let text = format_text(&result);
    assert!(text.contains("Found 1 issue"));
    assert!(text.contains("US-001"));
    assert!(text.contains("--auto-fix"));
}

#[test]
fn test_format_text_with_fixes() {
    let result = DoctorResult {
        issues: vec![Issue {
            issue_type: IssueType::StaleInProgressTask,
            entity_id: "US-001".to_string(),
            description: "Task is stale".to_string(),
        }],
        fixed: vec![Fix {
            issue_type: IssueType::StaleInProgressTask,
            entity_id: "US-001".to_string(),
            action: "Reset to todo".to_string(),
        }],
        would_fix: vec![],
        auto_fix: true,
        dry_run: false,
        summary: DoctorSummary {
            stale_tasks: 1,
            active_runs: 0,
            orphaned_relationships: 0,
            decay_warnings: 0,
            reconciled: 0,
            total_issues: 1,
            total_fixed: 1,
        },
    };

    let text = format_text(&result);
    assert!(text.contains("Fixed 1 issue"));
    assert!(text.contains("Reset to todo"));
}

// ============ serialization tests ============

#[test]
fn test_doctor_result_serialization() {
    let result = DoctorResult {
        issues: vec![Issue {
            issue_type: IssueType::StaleInProgressTask,
            entity_id: "US-001".to_string(),
            description: "Test issue".to_string(),
        }],
        fixed: vec![],
        would_fix: vec![],
        auto_fix: false,
        dry_run: false,
        summary: DoctorSummary {
            stale_tasks: 1,
            active_runs: 0,
            orphaned_relationships: 0,
            decay_warnings: 0,
            reconciled: 0,
            total_issues: 1,
            total_fixed: 0,
        },
    };

    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains("stale_in_progress_task"));
    assert!(json.contains("US-001"));
}

// ============ edge cases tests ============

#[test]
fn test_multiple_issues_all_types() {
    let (tmp_dir, conn) = setup_test_db();

    // Create a stale task
    insert_test_task(&conn, "US-001", "in_progress");

    // Create an active run
    conn.execute(
        "INSERT INTO runs (run_id, status, started_at) VALUES ('run-orphan', 'active', datetime('now'))",
        [],
    )
    .unwrap();

    // Create an orphaned relationship
    insert_test_task(&conn, "US-002", "todo");
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-002', 'GHOST', 'synergyWith')",
        [],
    )
    .unwrap();

    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.stale_tasks, 1);
    assert_eq!(result.summary.active_runs, 1);
    assert_eq!(result.summary.orphaned_relationships, 1);
    assert_eq!(result.summary.total_issues, 3);
}

#[test]
fn test_preserves_existing_notes_on_fix() {
    let (tmp_dir, conn) = setup_test_db();

    // Create task with existing notes
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, notes) VALUES ('US-001', 'Test', 'in_progress', 10, 'Existing notes here')",
        [],
    )
    .unwrap();

    doctor(&conn, true, false, 0, false, tmp_dir.path()).unwrap();

    let notes: String = conn
        .query_row("SELECT notes FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();

    assert!(notes.contains("Existing notes here"));
    assert!(notes.contains("[DOCTOR]"));
}

// ============ dry_run tests ============

#[test]
fn test_dry_run_shows_would_fix_without_modifying() {
    let (tmp_dir, conn) = setup_test_db();
    // Create a stale task
    insert_test_task(&conn, "US-001", "in_progress");

    let result = doctor(&conn, true, true, 0, false, tmp_dir.path()).unwrap();

    // Should have the issue
    assert_eq!(result.summary.stale_tasks, 1);
    assert!(result.dry_run);

    // Should show would_fix, not fixed
    assert!(result.fixed.is_empty());
    assert_eq!(result.would_fix.len(), 1);
    assert_eq!(result.would_fix[0].entity_id, "US-001");

    // Verify task was NOT modified
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "in_progress");
}

#[test]
fn test_dry_run_implies_auto_fix_for_output() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "in_progress");

    // Even without auto_fix=true, dry_run should show what would be fixed
    let result = doctor(&conn, false, true, 0, false, tmp_dir.path()).unwrap();

    assert!(result.dry_run);
    assert!(!result.auto_fix); // auto_fix flag itself is false
    assert_eq!(result.would_fix.len(), 1); // But we still see would_fix

    // Verify no modifications
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "in_progress");
}

#[test]
fn test_dry_run_all_issue_types() {
    let (tmp_dir, conn) = setup_test_db();

    // Create a stale task
    insert_test_task(&conn, "US-001", "in_progress");

    // Create an active run
    conn.execute(
        "INSERT INTO runs (run_id, status, started_at) VALUES ('run-orphan', 'active', datetime('now'))",
        [],
    )
    .unwrap();

    // Create an orphaned relationship
    insert_test_task(&conn, "US-002", "todo");
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-002', 'GHOST', 'synergyWith')",
        [],
    )
    .unwrap();

    let result = doctor(&conn, true, true, 0, false, tmp_dir.path()).unwrap();

    assert!(result.dry_run);
    assert_eq!(result.summary.total_issues, 3);
    assert_eq!(result.would_fix.len(), 3);
    assert!(result.fixed.is_empty());

    // Verify no modifications
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "in_progress");

    let run_status: String = conn
        .query_row(
            "SELECT status FROM runs WHERE run_id = 'run-orphan'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(run_status, "active");

    let rel_count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'US-002'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rel_count, 1);
}

#[test]
fn test_format_text_dry_run() {
    let result = DoctorResult {
        issues: vec![Issue {
            issue_type: IssueType::StaleInProgressTask,
            entity_id: "US-001".to_string(),
            description: "Task is stale".to_string(),
        }],
        fixed: vec![],
        would_fix: vec![Fix {
            issue_type: IssueType::StaleInProgressTask,
            entity_id: "US-001".to_string(),
            action: "Reset to todo".to_string(),
        }],
        auto_fix: true,
        dry_run: true,
        summary: DoctorSummary {
            stale_tasks: 1,
            active_runs: 0,
            orphaned_relationships: 0,
            decay_warnings: 0,
            reconciled: 0,
            total_issues: 1,
            total_fixed: 0,
        },
    };

    let text = format_text(&result);
    assert!(text.contains("[DRY RUN]"));
    assert!(text.contains("Would fix 1 issue"));
    assert!(text.contains("No changes were made"));
}

// ============ extract_bracketed_task_ids tests ============

#[test]
fn test_extract_task_id_basic() {
    let ids = checks::extract_bracketed_task_ids("feat: [FEAT-001] Foundation module");
    assert_eq!(ids, vec!["FEAT-001"]);
}

#[test]
fn test_extract_task_id_multiple() {
    let ids =
        checks::extract_bracketed_task_ids("feat: [FEAT-001] [FEAT-002] Combined implementation");
    assert_eq!(ids, vec!["FEAT-001", "FEAT-002"]);
}

#[test]
fn test_extract_task_id_complex_patterns() {
    let ids =
        checks::extract_bracketed_task_ids("feat: [TEST-INIT-005] Initial tests for soul features");
    assert_eq!(ids, vec!["TEST-INIT-005"]);
}

#[test]
fn test_extract_task_id_ignores_lowercase() {
    let ids = checks::extract_bracketed_task_ids("feat: [feat-001] lowercase not matched");
    assert!(ids.is_empty());
}

#[test]
fn test_extract_task_id_ignores_no_hyphen() {
    let ids = checks::extract_bracketed_task_ids("feat: [FEAT001] no hyphen");
    assert!(ids.is_empty());
}

#[test]
fn test_extract_task_id_empty_brackets() {
    let ids = checks::extract_bracketed_task_ids("feat: [] empty brackets");
    assert!(ids.is_empty());
}

#[test]
fn test_extract_task_id_with_spaces() {
    let ids = checks::extract_bracketed_task_ids("feat: [FEAT 001] spaces not valid");
    assert!(ids.is_empty());
}

#[test]
fn test_extract_task_id_no_brackets() {
    let ids = checks::extract_bracketed_task_ids("feat: FEAT-001 no brackets");
    assert!(ids.is_empty());
}

#[test]
fn test_extract_task_id_unclosed_bracket() {
    let ids = checks::extract_bracketed_task_ids("feat: [FEAT-001 unclosed");
    assert!(ids.is_empty());
}

#[test]
fn test_extract_task_id_various_prefixes() {
    assert_eq!(
        checks::extract_bracketed_task_ids("fix: [FIX-001] bug fix"),
        vec!["FIX-001"]
    );
    assert_eq!(
        checks::extract_bracketed_task_ids("chore: [CODE-REVIEW-1] review"),
        vec!["CODE-REVIEW-1"]
    );
    assert_eq!(
        checks::extract_bracketed_task_ids("feat: [US-042] user story"),
        vec!["US-042"]
    );
}

// ============ is_valid_task_id tests ============

#[test]
fn test_valid_task_ids() {
    assert!(checks::is_valid_task_id("FEAT-001"));
    assert!(checks::is_valid_task_id("US-001"));
    assert!(checks::is_valid_task_id("TEST-INIT-005"));
    assert!(checks::is_valid_task_id("CODE-REVIEW-1"));
    assert!(checks::is_valid_task_id("FIX-001"));
    assert!(checks::is_valid_task_id("REFACTOR-1-001"));
    assert!(checks::is_valid_task_id("MILESTONE-FINAL"));
}

#[test]
fn test_invalid_task_ids() {
    assert!(!checks::is_valid_task_id("")); // empty
    assert!(!checks::is_valid_task_id("FEAT001")); // no hyphen
    assert!(!checks::is_valid_task_id("feat-001")); // lowercase
    assert!(!checks::is_valid_task_id("FEAT 001")); // space
    assert!(!checks::is_valid_task_id("FEAT-001!")); // special char
    assert!(!checks::is_valid_task_id(&"A-".repeat(20))); // too long (>30 chars)
}

// ============ git reconciliation tests (with real git repo) ============

/// Helper to set up a git repo in a temp dir with commits containing task IDs.
fn setup_git_repo_with_commits(dir: &std::path::Path, commits: &[&str]) {
    // Initialize git repo
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir)
        .output()
        .unwrap();

    // Configure git user for commits
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    // Create commits
    for (i, msg) in commits.iter().enumerate() {
        let filename = format!("file{}.txt", i);
        std::fs::write(dir.join(&filename), format!("content {}", i)).unwrap();
        std::process::Command::new("git")
            .args(["add", &filename])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(dir)
            .output()
            .unwrap();
    }
}

#[test]
fn test_reconcile_git_disabled_by_default() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "FEAT-001", "todo");
    setup_git_repo_with_commits(tmp_dir.path(), &["feat: [FEAT-001] Foundation module"]);

    // reconcile_git=false, so no reconciliation even though commit exists
    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();
    assert_eq!(result.summary.reconciled, 0);
}

#[test]
fn test_reconcile_git_detects_completed_task() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "FEAT-001", "todo");
    setup_git_repo_with_commits(tmp_dir.path(), &["feat: [FEAT-001] Foundation module"]);

    let result = doctor(&conn, false, false, 0, true, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.reconciled, 1);
    assert!(result
        .issues
        .iter()
        .any(|i| { i.issue_type == IssueType::GitReconciliation && i.entity_id == "FEAT-001" }));
}

#[test]
fn test_reconcile_git_skips_done_tasks() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "FEAT-001", "done"); // already done
    setup_git_repo_with_commits(tmp_dir.path(), &["feat: [FEAT-001] Foundation module"]);

    let result = doctor(&conn, false, false, 0, true, tmp_dir.path()).unwrap();

    // Task already done, so no reconciliation needed
    assert_eq!(result.summary.reconciled, 0);
}

#[test]
fn test_reconcile_git_skips_nonexistent_tasks() {
    let (tmp_dir, conn) = setup_test_db();
    // Don't insert any task — ID in commit doesn't exist in DB
    setup_git_repo_with_commits(
        tmp_dir.path(),
        &["feat: [GHOST-001] This task doesn't exist"],
    );

    let result = doctor(&conn, false, false, 0, true, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.reconciled, 0);
}

#[test]
fn test_reconcile_git_auto_fix_marks_done() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "FEAT-001", "todo");
    setup_git_repo_with_commits(tmp_dir.path(), &["feat: [FEAT-001] Foundation module"]);

    let result = doctor(&conn, true, false, 0, true, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.reconciled, 1);
    assert_eq!(result.summary.total_fixed, 1);
    assert!(result
        .fixed
        .iter()
        .any(|f| { f.entity_id == "FEAT-001" && f.issue_type == IssueType::GitReconciliation }));

    // Verify task was marked done
    let status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE id = 'FEAT-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "done");

    // Verify audit note includes commit message
    let notes: String = conn
        .query_row("SELECT notes FROM tasks WHERE id = 'FEAT-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(notes.contains("[DOCTOR]"));
    assert!(notes.contains("Reconciled from git history"));
}

#[test]
fn test_reconcile_git_dry_run_no_changes() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "FEAT-001", "todo");
    setup_git_repo_with_commits(tmp_dir.path(), &["feat: [FEAT-001] Foundation module"]);

    let result = doctor(&conn, true, true, 0, true, tmp_dir.path()).unwrap();

    assert_eq!(result.summary.reconciled, 1);
    assert!(result.fixed.is_empty());
    assert_eq!(result.would_fix.len(), 1);
    assert_eq!(result.would_fix[0].entity_id, "FEAT-001");

    // Verify task was NOT modified
    let status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE id = 'FEAT-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "todo");
}

#[test]
fn test_reconcile_git_multiple_tasks() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "FEAT-001", "todo");
    insert_test_task(&conn, "FEAT-002", "in_progress");
    insert_test_task(&conn, "FEAT-003", "done"); // already done, skip
    setup_git_repo_with_commits(
        tmp_dir.path(),
        &[
            "feat: [FEAT-001] First feature",
            "feat: [FEAT-002] Second feature",
            "feat: [FEAT-003] Third feature",
        ],
    );

    let result = doctor(&conn, false, false, 0, true, tmp_dir.path()).unwrap();

    // FEAT-001 and FEAT-002 should be reconciliation candidates, FEAT-003 is already done
    assert_eq!(result.summary.reconciled, 2);
}

#[test]
fn test_reconcile_git_no_git_repo() {
    let (tmp_dir, conn) = setup_test_db();
    insert_test_task(&conn, "FEAT-001", "todo");

    // No git repo initialized — should handle gracefully
    let result = doctor(&conn, false, false, 0, true, tmp_dir.path()).unwrap();
    assert_eq!(result.summary.reconciled, 0);
}

#[test]
fn test_reconcile_git_preserves_existing_notes() {
    let (tmp_dir, conn) = setup_test_db();
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, notes) VALUES ('FEAT-001', 'Test', 'todo', 10, 'Pre-existing notes')",
        [],
    )
    .unwrap();
    setup_git_repo_with_commits(tmp_dir.path(), &["feat: [FEAT-001] Foundation module"]);

    doctor(&conn, true, false, 0, true, tmp_dir.path()).unwrap();

    let notes: String = conn
        .query_row("SELECT notes FROM tasks WHERE id = 'FEAT-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(notes.contains("Pre-existing notes"));
    assert!(notes.contains("[DOCTOR]"));
}

// ============ format_text git reconciliation tests ============

#[test]
fn test_format_text_git_reconciliation() {
    let result = DoctorResult {
        issues: vec![Issue {
            issue_type: IssueType::GitReconciliation,
            entity_id: "FEAT-001".to_string(),
            description: "Task found in git commit but not done".to_string(),
        }],
        fixed: vec![],
        would_fix: vec![],
        auto_fix: false,
        dry_run: false,
        summary: DoctorSummary {
            stale_tasks: 0,
            active_runs: 0,
            orphaned_relationships: 0,
            decay_warnings: 0,
            reconciled: 1,
            total_issues: 1,
            total_fixed: 0,
        },
    };

    let text = format_text(&result);
    assert!(text.contains("Git reconciliation (1)"));
    assert!(text.contains("FEAT-001"));
}

#[test]
fn test_format_text_git_reconciliation_verbose() {
    let result = DoctorResult {
        issues: vec![Issue {
            issue_type: IssueType::GitReconciliation,
            entity_id: "FEAT-001".to_string(),
            description: "Found in git commit".to_string(),
        }],
        fixed: vec![],
        would_fix: vec![],
        auto_fix: false,
        dry_run: false,
        summary: DoctorSummary {
            stale_tasks: 0,
            active_runs: 0,
            orphaned_relationships: 0,
            decay_warnings: 0,
            reconciled: 1,
            total_issues: 1,
            total_fixed: 0,
        },
    };

    let verbose = format_doctor_verbose(&result);
    assert!(verbose.contains("Git reconciliation"));
    assert!(verbose.contains("FEAT-001"));
}

#[test]
fn test_git_reconciliation_serialization() {
    let result = DoctorResult {
        issues: vec![Issue {
            issue_type: IssueType::GitReconciliation,
            entity_id: "FEAT-001".to_string(),
            description: "Found in git".to_string(),
        }],
        fixed: vec![Fix {
            issue_type: IssueType::GitReconciliation,
            entity_id: "FEAT-001".to_string(),
            action: "Marked as done".to_string(),
        }],
        would_fix: vec![],
        auto_fix: true,
        dry_run: false,
        summary: DoctorSummary {
            stale_tasks: 0,
            active_runs: 0,
            orphaned_relationships: 0,
            decay_warnings: 0,
            reconciled: 1,
            total_issues: 1,
            total_fixed: 1,
        },
    };

    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains("git_reconciliation"));
    assert!(json.contains("\"reconciled\":1"));
    assert!(json.contains("FEAT-001"));
}

// ============ soft-archive exclusion tests ============

/// Archived runs (archived_at IS NOT NULL) must not appear in
/// find_active_runs_without_end results (verified through doctor()).
#[test]
fn test_archived_run_excluded_from_active_runs_without_end() {
    let (tmp_dir, conn) = setup_test_db();

    conn.execute(
        "INSERT INTO runs (run_id, status, started_at) VALUES ('r-archived', 'active', datetime('now'))",
        [],
    )
    .unwrap();

    // Soft-archive the run
    conn.execute(
        "UPDATE runs SET archived_at = datetime('now') WHERE run_id = 'r-archived'",
        [],
    )
    .unwrap();

    // doctor() internally calls find_active_runs_without_end; archived runs must
    // not be reported as abandoned active runs.
    let result = doctor(&conn, false, false, 0, false, tmp_dir.path()).unwrap();
    assert_eq!(
        result.summary.active_runs, 0,
        "Archived runs must not be counted as active runs without end"
    );
}
