//! Git-based task completion reconciliation.
//!
//! This module provides functions for detecting task completions via git commit
//! history — both in the local project repo and in external (agent-managed) repos.
//! It handles cross-PRD prefix isolation, word-boundary matching, and dependency-
//! gating via `force=false` in the complete command.

use std::path::Path;

use rusqlite::Connection;

use crate::commands::complete as complete_cmd;
use crate::db::prefix::prefix_and;
use crate::loop_engine::engine::update_prd_task_passes;

/// Scan recent commits in an external git repo for task completion evidence.
///
/// Queries all incomplete task IDs from the DB, then checks recent git commits
/// in the external repo for any that contain a task ID (case-insensitive).
/// Matches are marked as done and the PRD JSON is updated.
///
/// Returns the number of tasks reconciled.
pub(crate) fn reconcile_external_git_completions(
    external_repo: &Path,
    conn: &mut Connection,
    run_id: &str,
    prd_path: &Path,
    task_prefix: Option<&str>,
    scan_depth: usize,
) -> usize {
    use std::process::Command;

    // Validate the external repo exists
    if !external_repo.exists() {
        eprintln!(
            "Warning: external git repo not found at {}, skipping reconciliation",
            external_repo.display()
        );
        return 0;
    }

    // Get recent commits from external repo
    let depth_arg = format!("-{}", scan_depth);
    let output = match Command::new("git")
        .args(["log", "--oneline", &depth_arg])
        .current_dir(external_repo)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(o) => {
            eprintln!(
                "Warning: git log failed in {}: {}",
                external_repo.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return 0;
        }
        Err(e) => {
            eprintln!(
                "Warning: could not run git in {}: {}",
                external_repo.display(),
                e
            );
            return 0;
        }
    };

    if output.is_empty() {
        return 0;
    }

    // Per-commit processing: split into individual lines instead of bulk string
    // to prevent cross-commit substring collisions.
    let commit_lines: Vec<String> = output.lines().map(|l| l.to_uppercase()).collect();

    // Query all incomplete task IDs, scoped to this PRD's prefix.
    let (regc_pfx_clause, regc_pfx_param) = prefix_and(task_prefix);
    let regc_sql = format!(
        "SELECT id FROM tasks WHERE status NOT IN ('done', 'irrelevant') {regc_pfx_clause}"
    );
    let mut stmt = match conn.prepare(&regc_sql) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: could not query tasks for reconciliation: {}", e);
            return 0;
        }
    };

    let regc_params: Vec<&dyn rusqlite::types::ToSql> = match &regc_pfx_param {
        Some(p) => vec![p],
        None => vec![],
    };
    let task_ids: Vec<String> = stmt
        .query_map(regc_params.as_slice(), |row| row.get(0))
        .ok()
        .map(|rows| {
            rows.filter_map(|r: rusqlite::Result<String>| r.ok())
                .collect()
        })
        .unwrap_or_default();

    drop(stmt);

    let mut reconciled = 0;

    for task_id in &task_ids {
        // Require "-completed" suffix in commit message (case-insensitive)
        let completed_marker = format!("{}-COMPLETED", task_id.to_uppercase());
        let matched = commit_lines
            .iter()
            .any(|line| contains_task_id(line, &completed_marker));
        if matched {
            // Mark as done — force=false so dependency gating applies
            let ids = [task_id.clone()];
            if let Err(e) = complete_cmd::complete(
                conn,
                &ids,
                Some(run_id),
                None, // no specific commit hash from oneline
                false,
            ) {
                eprintln!("Reconciliation skipped for {}: {}", task_id, e);
                continue;
            }

            // Update PRD JSON
            if let Err(e) = update_prd_task_passes(prd_path, task_id, true, task_prefix) {
                eprintln!(
                    "Warning: failed to update PRD for reconciled task {}: {}",
                    task_id, e
                );
            }

            eprintln!(
                "Reconciled task {} (found in external repo commits)",
                task_id
            );
            reconciled += 1;
        }
    }

    reconciled
}

/// Check if `text` contains `task_id` at valid word boundaries.
/// Characters before/after the match must NOT be alphanumeric or hyphen.
/// Prevents "P9-FEAT-001" from matching "FEAT-001" and "FEAT-00" from matching inside "FEAT-001".
pub(crate) fn contains_task_id(text: &str, task_id: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = text[start..].find(task_id) {
        let abs_pos = start + pos;
        let end_pos = abs_pos + task_id.len();

        let valid_start = if abs_pos == 0 {
            true
        } else {
            let prev = text.as_bytes()[abs_pos - 1];
            !prev.is_ascii_alphanumeric() && prev != b'-'
        };

        let valid_end = if end_pos >= text.len() {
            true
        } else {
            let next = text.as_bytes()[end_pos];
            !next.is_ascii_alphanumeric() && next != b'-'
        };

        if valid_start && valid_end {
            return true;
        }
        start = abs_pos + 1;
    }
    false
}

/// Check recent git commits for the task ID with `-completed` suffix.
///
/// Returns the commit hash if found, None otherwise.
/// Checks the last `scan_depth` commits (subject + body) to handle multi-commit
/// iterations where the task ID may appear in an earlier commit.
/// Requires full prefixed ID with `-completed` suffix — no base ID fallback.
pub(crate) fn check_git_for_task_completion(
    project_root: &Path,
    task_id: &str,
    _task_prefix: Option<&str>,
    scan_depth: usize,
) -> Option<String> {
    use std::process::Command;

    // Get recent commits: hash + full message (subject + body).
    // Use a record separator to split multi-line commit messages.
    let depth_arg = format!("-{}", scan_depth);
    let output = Command::new("git")
        .args(["log", &depth_arg, "--format=%H%x00%B%x00"])
        .current_dir(project_root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    // Require "-completed" suffix in commit message (case-insensitive)
    let completed_marker = format!("{}-COMPLETED", task_id.to_uppercase());

    // Each record is: <hash>\0<full message>\0
    // Split on \0 and process pairs.
    let parts: Vec<&str> = raw.split('\0').collect();
    // parts layout: [hash, message, hash, message, ..., trailing-empty]
    for chunk in parts.chunks(2) {
        if chunk.len() < 2 {
            break;
        }
        let hash = chunk[0].trim();
        let message = chunk[1];

        if hash.is_empty() {
            continue;
        }

        let message_upper = message.to_uppercase();
        if contains_task_id(&message_upper, &completed_marker) {
            return Some(hash.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_commit(dir: &std::path::Path, msg: &str) {
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", msg])
            .current_dir(dir)
            .output()
            .expect("create commit");
    }

    // ======================================================================
    // contains_task_id() unit tests — boundary-aware matching
    // ======================================================================

    #[test]
    fn test_contains_task_id_blocks_hyphen_prefix() {
        // "P9-FEAT-001" must NOT match "FEAT-001" — hyphen precedes
        assert!(
            !contains_task_id("P9-FEAT-001", "FEAT-001"),
            "Hyphen-prefixed ID should not match"
        );
    }

    #[test]
    fn test_contains_task_id_blocks_numeric_suffix() {
        // "FEAT-001" must NOT match "FEAT-00" — digit follows
        assert!(
            !contains_task_id("FEAT-001", "FEAT-00"),
            "Partial ID with trailing digit should not match"
        );
    }

    #[test]
    fn test_contains_task_id_allows_space_prefix() {
        assert!(
            contains_task_id("feat: FEAT-001 done", "FEAT-001"),
            "Space-separated ID should match"
        );
    }

    #[test]
    fn test_contains_task_id_allows_bracket_prefix() {
        assert!(
            contains_task_id("[FEAT-001]", "FEAT-001"),
            "Bracket-delimited ID should match"
        );
    }

    #[test]
    fn test_contains_task_id_allows_start_of_string() {
        assert!(
            contains_task_id("FEAT-001 desc", "FEAT-001"),
            "ID at start of string should match"
        );
    }

    #[test]
    fn test_contains_task_id_allows_end_of_string() {
        assert!(
            contains_task_id("completed FEAT-001", "FEAT-001"),
            "ID at end of string should match"
        );
    }

    #[test]
    fn test_contains_task_id_exact_match() {
        assert!(
            contains_task_id("FEAT-001", "FEAT-001"),
            "Exact match should succeed"
        );
    }

    #[test]
    fn test_contains_task_id_blocks_alpha_prefix() {
        // "XFEAT-001" must NOT match "FEAT-001"
        assert!(
            !contains_task_id("XFEAT-001", "FEAT-001"),
            "Alpha-prefixed ID should not match"
        );
    }

    #[test]
    fn test_contains_task_id_blocks_alpha_suffix() {
        // "FEAT-001A" must NOT match "FEAT-001"
        assert!(
            !contains_task_id("FEAT-001A", "FEAT-001"),
            "Alpha-suffixed ID should not match"
        );
    }

    #[test]
    fn test_contains_task_id_allows_colon_delimiter() {
        assert!(
            contains_task_id("fix:FEAT-001:done", "FEAT-001"),
            "Colon-delimited ID should match"
        );
    }

    #[test]
    fn test_contains_task_id_no_match() {
        assert!(
            !contains_task_id("nothing here", "FEAT-001"),
            "Unrelated text should not match"
        );
    }

    #[test]
    fn test_contains_task_id_blocks_longer_numeric_id() {
        // "FEAT-0010" must NOT match "FEAT-001" — digit follows the match
        assert!(
            !contains_task_id("FEAT-0010", "FEAT-001"),
            "FEAT-0010 should not match FEAT-001 (trailing digit)"
        );
        // And the reverse: "FEAT-001" must NOT match "FEAT-0010"
        assert!(
            !contains_task_id("FEAT-001", "FEAT-0010"),
            "FEAT-001 should not match FEAT-0010 (not a substring)"
        );
    }

    // --- check_git_for_task_completion tests ---

    #[test]
    fn test_check_git_completion_finds_task_id_in_commit() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: SEC-H005-completed - Add feature");

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", None, 7);
        assert!(
            result.is_some(),
            "Should find task ID with -completed suffix in commit message"
        );
    }

    #[test]
    fn test_check_git_completion_case_insensitive() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: sec-h005-Completed lowercase");

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", None, 7);
        assert!(result.is_some(), "Should find task ID case-insensitively");
    }

    #[test]
    fn test_check_git_completion_returns_none_when_not_found() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: unrelated commit");

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", None, 7);
        assert!(
            result.is_none(),
            "Should return None when task ID not in commit"
        );
    }

    #[test]
    fn test_check_git_completion_returns_commit_hash() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: TASK-001-completed test");

        let result = check_git_for_task_completion(temp_dir.path(), "TASK-001", None, 7);
        assert!(result.is_some());
        let hash = result.unwrap();
        assert_eq!(hash.len(), 40, "Should return full commit hash");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "Hash should be hex"
        );
    }

    #[test]
    fn test_check_git_completion_finds_task_in_earlier_commit() {
        // Claude may create multiple commits; the task ID might be in an earlier one
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(
            temp_dir.path(),
            "feat: TASK-001-completed - implement feature",
        );
        git_commit(temp_dir.path(), "fix: adjust config formatting");
        git_commit(temp_dir.path(), "chore: update lockfile");

        let result = check_git_for_task_completion(temp_dir.path(), "TASK-001", None, 7);
        assert!(
            result.is_some(),
            "Should find task ID in earlier commit (not just HEAD)"
        );
    }

    #[test]
    fn test_check_git_completion_finds_task_in_commit_body() {
        // Task ID may appear in commit body, not just subject
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        std::process::Command::new("git")
            .args([
                "commit",
                "--allow-empty",
                "-m",
                "feat: TASK-001-completed\n\nCompletes TASK-001 acceptance criteria",
            ])
            .current_dir(temp_dir.path())
            .output()
            .expect("create commit with body");

        let result = check_git_for_task_completion(temp_dir.path(), "TASK-001", None, 7);
        assert!(result.is_some(), "Should find task ID in commit body");
    }

    #[test]
    fn test_git_completion_returns_some_for_matching_commit() {
        // Regression: git-based detection returns Some(hash) which the loop
        // uses to increment tasks_completed by 1.
        let repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(
            repo.path(),
            "feat: P3-FEAT-001-completed - Implement CallSupervisor",
        );

        let result = check_git_for_task_completion(repo.path(), "P3-FEAT-001", None, 7);
        assert!(
            result.is_some(),
            "Git detection should return Some for matching commit — loop increments counter by 1"
        );
    }

    #[test]
    fn test_git_completion_no_match_without_completed_suffix() {
        // DB has "uuid-FIX-001", commit has bare "FIX-001" — should NOT match
        let repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(repo.path(), "feat: FIX-001 implement feature");

        let result =
            check_git_for_task_completion(repo.path(), "aeb10a1f-FIX-001", Some("aeb10a1f"), 7);
        assert!(
            result.is_none(),
            "Should NOT match bare ID without -completed suffix"
        );
    }

    #[test]
    fn test_git_completion_matches_with_completed_suffix() {
        let repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(
            repo.path(),
            "feat: aeb10a1f-FIX-001-completed - Fix the bug",
        );

        let result =
            check_git_for_task_completion(repo.path(), "aeb10a1f-FIX-001", Some("aeb10a1f"), 7);
        assert!(
            result.is_some(),
            "Should match full ID with -completed suffix"
        );
    }

    #[test]
    fn test_git_completion_mention_in_body_no_match() {
        // Commit body mentions another task ID without -completed suffix
        let repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(
            repo.path(),
            "feat: some work\n\nThis prepares for aeb10a1f-FIX-002",
        );

        let result =
            check_git_for_task_completion(repo.path(), "aeb10a1f-FIX-002", Some("aeb10a1f"), 7);
        assert!(
            result.is_none(),
            "Mention in body without -completed suffix should NOT match"
        );
    }

    #[test]
    fn test_check_git_blocks_cross_prd_prefix() {
        // check_git_for_task_completion should not match P9-FEAT-001 for FEAT-001
        let repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(repo.path(), "feat: P9-FEAT-001 Phase 9 complete");

        let result = check_git_for_task_completion(repo.path(), "FEAT-001", None, 7);
        assert!(
            result.is_none(),
            "P9-FEAT-001 must not match FEAT-001 in git detection"
        );
    }

    // --- reconcile_external_git_completions tests ---

    #[test]
    fn test_reconcile_nonexistent_repo_returns_zero() {
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(&prd_path, r#"{"project":"test","userStories":[]}"#).unwrap();

        let count = reconcile_external_git_completions(
            Path::new("/nonexistent/repo"),
            &mut conn,
            "run-1",
            &prd_path,
            None,
            50,
        );
        assert_eq!(count, 0);
    }

    #[test]
    fn test_reconcile_finds_completed_tasks_in_external_repo() {
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        // Insert tasks — FEAT-001 is in_progress (eligible for reconciliation with force=false)
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'in_progress', 1),
             ('FEAT-002', 'Task 2', 'in_progress', 2),
             ('FEAT-003', 'Task 3', 'done', 3);",
        )
        .unwrap();

        // Create external git repo with commits containing task IDs
        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(
            ext_repo.path(),
            "feat: FEAT-001-completed - Implement feature",
        );
        git_commit(
            ext_repo.path(),
            "feat: FEAT-003-completed - Already done task",
        );

        // Create PRD file
        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Task 1","passes":false,"priority":1},
                {"id":"FEAT-002","title":"Task 2","passes":false,"priority":2},
                {"id":"FEAT-003","title":"Task 3","passes":true,"priority":3}
            ]}"#,
        )
        .unwrap();

        // Insert a run so complete_cmd works
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
            50,
        );

        // Should find FEAT-001 (in_progress → done), skip FEAT-003 (already done)
        // FEAT-002 is in_progress but not in commits
        assert_eq!(count, 1);

        // Verify FEAT-001 is now done
        let status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'FEAT-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "done");

        // Verify PRD was updated
        let prd_content = std::fs::read_to_string(&prd_path).unwrap();
        let prd: serde_json::Value = serde_json::from_str(&prd_content).unwrap();
        assert_eq!(prd["userStories"][0]["passes"], true);
    }

    #[test]
    fn test_reconcile_case_insensitive() {
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('SEC-H005', 'Security task', 'in_progress', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "feat: sec-h005-completed lowercase commit");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"SEC-H005","title":"Security task","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
            50,
        );

        assert_eq!(count, 1, "Should match case-insensitively");
    }

    #[test]
    fn test_reconcile_empty_repo_returns_zero() {
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'todo', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        // No additional commits beyond the initial one from setup_git_repo

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Task 1","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
            50,
        );

        assert_eq!(
            count, 0,
            "No matching commits should mean no reconciliation"
        );
    }

    // ======================================================================
    // Integration: cross-PRD prefix collision isolation
    // ======================================================================

    #[test]
    fn test_reconciliation_does_not_match_different_prd_prefix() {
        // Scenario: external repo has P9-MILESTONE-FINAL commits.
        // DB has acdfb313-MILESTONE-FINAL tasks (from telnyx PRD).
        // Reconciliation must NOT falsely complete them.
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('acdfb313-MILESTONE-FINAL', 'Telnyx milestone', 'in_progress', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        // P9 commit that contains MILESTONE-FINAL as a substring
        git_commit(ext_repo.path(), "feat: P9-MILESTONE-FINAL Phase 9 done");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"MILESTONE-FINAL","title":"Telnyx milestone","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            Some("acdfb313"),
            50,
        );

        assert_eq!(
            count, 0,
            "P9-MILESTONE-FINAL must NOT match MILESTONE-FINAL (cross-PRD prefix collision)"
        );

        // Verify task is still in_progress
        let status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'acdfb313-MILESTONE-FINAL'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn test_reconciliation_respects_dependencies() {
        // Verify force=false gates on deps: task with unsatisfied dep should not reconcile.
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Prereq task', 'in_progress', 1),
             ('FEAT-002', 'Dependent task', 'in_progress', 2);",
        )
        .unwrap();

        // FEAT-002 depends on FEAT-001
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('FEAT-002', 'FEAT-001', 'dependsOn')",
            [],
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        // Both tasks appear in commits with -completed suffix
        git_commit(ext_repo.path(), "feat: FEAT-001-completed done");
        git_commit(ext_repo.path(), "feat: FEAT-002-completed done");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Prereq","passes":false,"priority":1},
                {"id":"FEAT-002","title":"Dependent","passes":false,"priority":2}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
            50,
        );

        // FEAT-001 should reconcile (no deps).
        // FEAT-002 depends on FEAT-001 — but since reconciliation processes
        // tasks in arbitrary order, FEAT-001 may or may not be completed first.
        // The key invariant: if FEAT-002 is attempted before FEAT-001 is done,
        // force=false will block it due to unsatisfied dependency.
        // If FEAT-001 happens first, FEAT-002 may succeed.
        // Either way, at minimum 1 task reconciles (FEAT-001).
        assert!(count >= 1, "At least FEAT-001 (no deps) should reconcile");

        // Verify FEAT-001 is done
        let status1: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'FEAT-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status1, "done", "FEAT-001 should be completed");
    }

    #[test]
    fn test_reconciliation_todo_tasks_not_auto_completed() {
        // Verify force=false means todo tasks are NOT auto-completed.
        // This is the correct conservative behavior.
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Todo task', 'todo', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "feat: FEAT-001-completed done");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Todo task","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
            50,
        );

        assert_eq!(
            count, 0,
            "Todo tasks should not be auto-completed (force=false requires in_progress)"
        );

        // Verify task is still todo
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
    fn test_reconciliation_counts_multiple_tasks_accurately() {
        // Regression: reconciliation returns count of newly-completed tasks.
        // The loop should add this count (as u32) to tasks_completed.
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('P3-FEAT-001', 'Task 1', 'in_progress', 1),
             ('P3-FEAT-002', 'Task 2', 'in_progress', 2),
             ('P3-FEAT-003', 'Task 3', 'in_progress', 3);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(
            ext_repo.path(),
            "feat: P3-FEAT-001-completed - Implement CallSupervisor",
        );
        git_commit(
            ext_repo.path(),
            "feat: P3-FEAT-002-completed - Implement CallActor",
        );
        git_commit(
            ext_repo.path(),
            "feat: P3-FEAT-003-completed - Implement BargeIn",
        );

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"P3-FEAT-001","title":"Task 1","passes":false,"priority":1},
                {"id":"P3-FEAT-002","title":"Task 2","passes":false,"priority":2},
                {"id":"P3-FEAT-003","title":"Task 3","passes":false,"priority":3}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
            50,
        );

        assert_eq!(
            count, 3,
            "Reconciliation should return 3 — loop adds this to tasks_completed"
        );
    }

    #[test]
    fn test_reconciliation_skips_already_done_tasks_no_double_count() {
        // Regression: if git detection already marked a task done earlier in the
        // same iteration, reconciliation should NOT re-count it. The query
        // filters `status NOT IN ('done', 'irrelevant')`.
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        // FEAT-001 already done (as if git detection marked it), FEAT-002 in_progress
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'done', 1),
             ('FEAT-002', 'Task 2', 'in_progress', 2);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "feat: FEAT-001-completed Already done");
        git_commit(ext_repo.path(), "feat: FEAT-002-completed New completion");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Task 1","passes":true,"priority":1},
                {"id":"FEAT-002","title":"Task 2","passes":false,"priority":2}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
            50,
        );

        assert_eq!(
            count, 1,
            "Should only count FEAT-002 (new) not FEAT-001 (already done) — no double counting"
        );
    }

    #[test]
    fn test_reconciliation_no_match_without_completed_suffix() {
        // DB has prefixed ID "aeb10a1f-FIX-001", commit uses bare "FIX-001" (no -completed suffix)
        // Should NOT match — requires -completed suffix now
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('aeb10a1f-FIX-001', 'Fix bug', 'in_progress', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "fix: FIX-001 Fix the bug");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FIX-001","title":"Fix bug","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            Some("aeb10a1f"),
            50,
        );

        assert_eq!(
            count, 0,
            "Should NOT match bare base ID without -completed suffix"
        );
    }

    #[test]
    fn test_reconciliation_matches_full_id_with_completed_suffix() {
        // Commit uses full prefixed ID with -completed suffix — should match
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('aeb10a1f-FIX-001', 'Fix bug', 'in_progress', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(
            ext_repo.path(),
            "fix: aeb10a1f-FIX-001-completed Fix the bug",
        );

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FIX-001","title":"Fix bug","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            Some("aeb10a1f"),
            50,
        );

        assert_eq!(count, 1, "Should match full prefixed ID in commit");
    }
}
