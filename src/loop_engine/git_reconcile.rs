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
use crate::loop_engine::prd_reconcile::update_prd_task_passes;
use crate::output::ui;

fn query_incomplete_task_ids(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> Result<Vec<String>, rusqlite::Error> {
    let (pfx_clause, pfx_param) = prefix_and(task_prefix);
    let sql = format!(
        "SELECT id FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL {pfx_clause}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = match &pfx_param {
        Some(p) => vec![p],
        None => vec![],
    };
    let ids = stmt
        .query_map(params.as_slice(), |row| row.get(0))
        .ok()
        .map(|rows| {
            rows.filter_map(|r: rusqlite::Result<String>| r.ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(ids)
}

/// Scan recent commits in an external git repo for task completion evidence.
///
/// Queries all incomplete task IDs from the DB, then checks recent git commits
/// in the external repo for any that contain a task ID (case-insensitive).
/// Matches are marked as done and the PRD JSON is updated.
///
/// Returns the list of reconciled task IDs (NOT a count), mirroring
/// [`reconcile_merged_slot_completions`]. Callers that only need the count use
/// `.len()`; the converged post-completion coordinator
/// (`reactions::post_completion::react_to_completions`) folds these ids into
/// its human-review set so a `requires_human` task completed out-of-band in the
/// external repo still triggers review. Empty Vec means nothing matched.
pub(crate) fn reconcile_external_git_completions(
    external_repo: &Path,
    conn: &mut Connection,
    run_id: &str,
    prd_path: &Path,
    task_prefix: Option<&str>,
    scan_depth: usize,
) -> Vec<String> {
    use std::process::Command;

    // Validate the external repo exists
    if !external_repo.exists() {
        tracing::warn!(
            "external git repo not found at {}, skipping reconciliation",
            external_repo.display()
        );
        return Vec::new();
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
            tracing::warn!(
                "git log failed in {}: {}",
                external_repo.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!("could not run git in {}: {}", external_repo.display(), e);
            return Vec::new();
        }
    };

    if output.is_empty() {
        return Vec::new();
    }

    // Per-commit processing: split into individual lines instead of bulk string
    // to prevent cross-commit substring collisions.
    let commit_lines: Vec<String> = output.lines().map(|l| l.to_uppercase()).collect();

    // Query all incomplete task IDs, scoped to this PRD's prefix.
    let task_ids = match query_incomplete_task_ids(conn, task_prefix) {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("could not query tasks for reconciliation: {}", e);
            return Vec::new();
        }
    };

    let mut reconciled: Vec<String> = Vec::new();

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
                tracing::warn!("reconciliation skipped for {}: {}", task_id, e);
                continue;
            }

            // Update PRD JSON
            if let Err(e) = update_prd_task_passes(prd_path, task_id, true, task_prefix) {
                tracing::warn!(
                    "failed to update PRD for reconciled task {}: {}",
                    task_id,
                    e
                );
            }

            ui::emit(&format!(
                "Reconciled task {} (found in external repo commits)",
                task_id
            ));
            reconciled.push(task_id.clone());
        }
    }

    reconciled
}

/// Scan commits merged back into slot 0 (the loop's main worktree) for task
/// completion markers and reconcile any matches in the DB + PRD JSON.
///
/// This is the **post-merge** sibling of [`reconcile_external_git_completions`].
/// In wave / parallel-slot mode, a slot agent may commit `feat: <TASK-ID>-completed`
/// and merge back into slot 0 yet still exit before flushing its `<completed>` tag
/// (output drop, watchdog kill, deadline). Without this scan, the task stays
/// `in_progress` until loop exit, where `pending_slot_tasks` drain resets it to
/// `todo` and the next loop reselects the same already-merged work.
///
/// `pre_merge_head` is slot 0's HEAD captured **before** the per-wave merge-back
/// (see `MergeOutcomes.pre_merge_head`). Scanning `{pre_merge_head}..HEAD`
/// bounds the range to commits this wave introduced.
///
/// `--no-merges` is **load-bearing**: it excludes merge commits produced by
/// `ClaudeMergeResolver`, whose `git commit --no-edit` carries merged-in commit
/// bodies and would otherwise let a resolver merge commit on slot A mark
/// slot B's task done. Slot agents write single-parent commits; the resolver
/// writes merge commits.
///
/// Returns the list of reconciled task IDs (NOT a count) so the caller can
/// drain `pending_slot_tasks` via `retain`. Empty Vec means no matches —
/// either no commits in range (no-op merge), no markers in the bodies, or
/// every match was already-done / blocked by dependency gating.
///
/// Every IO/DB/git failure is warn-and-continue; this function never panics.
pub(crate) fn reconcile_merged_slot_completions(
    slot0_path: &Path,
    pre_merge_head: &str,
    conn: &mut Connection,
    run_id: &str,
    prd_path: &Path,
    task_prefix: Option<&str>,
) -> Vec<String> {
    use std::process::Command;

    // Short-circuit on the FEAT-001 capture-failed sentinel: when slot 0's
    // pre-merge HEAD couldn't be captured, there's no range to scan. Emit a
    // single warn line so the operator sees why no reconcile fired.
    if pre_merge_head.is_empty() {
        tracing::warn!(
            "Post-merge reconcile: skipped (pre-merge HEAD was not captured for slot 0)"
        );
        return Vec::new();
    }

    // Get commits in {pre_merge_head}..HEAD, excluding merge commits.
    // Body match is required (slot agents put the marker in the body, not the subject),
    // so we use --format=%H%x00%B%x00 to preserve newlines between hash and message.
    let range = format!("{pre_merge_head}..HEAD");
    let output = match Command::new("git")
        .args(["log", "--no-merges", &range, "--format=%H%x00%B%x00"])
        .current_dir(slot0_path)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(o) => {
            tracing::warn!(
                "Post-merge reconcile: git log failed in {}: {}",
                slot0_path.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!(
                "Post-merge reconcile: could not run git in {}: {}",
                slot0_path.display(),
                e
            );
            return Vec::new();
        }
    };

    if output.is_empty() {
        return Vec::new();
    }

    // Parse hash/message pairs separated by NUL. Layout: [hash, msg, hash, msg, ..., trailing-empty]
    let parts: Vec<&str> = output.split('\0').collect();
    let mut commit_bodies_upper: Vec<String> = Vec::new();
    for chunk in parts.chunks(2) {
        if chunk.len() < 2 {
            break;
        }
        let hash = chunk[0].trim();
        if hash.is_empty() {
            continue;
        }
        commit_bodies_upper.push(chunk[1].to_uppercase());
    }

    if commit_bodies_upper.is_empty() {
        return Vec::new();
    }

    // Enumerate incomplete tasks, scoped to this PRD's prefix.
    let task_ids = match query_incomplete_task_ids(conn, task_prefix) {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("Post-merge reconcile: could not query tasks: {}", e);
            return Vec::new();
        }
    };

    let mut reconciled: Vec<String> = Vec::new();

    for task_id in &task_ids {
        let completed_marker = format!("{}-COMPLETED", task_id.to_uppercase());
        let matched = commit_bodies_upper
            .iter()
            .any(|body| contains_task_id(body, &completed_marker));
        if !matched {
            continue;
        }

        let ids = [task_id.clone()];
        if let Err(e) = complete_cmd::complete(conn, &ids, Some(run_id), None, false) {
            tracing::warn!("Post-merge reconcile: skipped {} ({})", task_id, e);
            continue;
        }

        if let Err(e) = update_prd_task_passes(prd_path, task_id, true, task_prefix) {
            tracing::warn!(
                "Post-merge reconcile: failed to update PRD for {}: {}",
                task_id,
                e
            );
        }

        ui::emit(&format!(
            "Post-merge reconcile: marked {} done (found in merged-back commits)",
            task_id
        ));
        reconciled.push(task_id.clone());
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

/// Commit uncommitted changes on behalf of the subprocess when it couldn't.
///
/// In scoped permission mode (`--permission-mode dontAsk`), the Claude subprocess
/// may be unable to run `git commit` even when `Bash(git:*)` is allowed (e.g. due
/// to session learnings or format mismatches). This function is called by the loop
/// engine after detecting task completion when no git commit was made.
///
/// Returns `Some(commit_hash)` on success, `None` if nothing to commit or on error.
pub(crate) fn wrapper_commit(
    working_root: &Path,
    task_id: &str,
    message_suffix: &str,
) -> Option<String> {
    use std::process::Command;

    // Check for uncommitted changes
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(working_root)
        .output()
        .ok()?;

    let changes = String::from_utf8_lossy(&status.stdout);
    if changes.trim().is_empty() {
        return None; // Nothing to commit
    }

    // Stage all changes
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(working_root)
        .output()
        .ok()?;

    if !add.status.success() {
        tracing::warn!(
            "wrapper git add failed: {}",
            String::from_utf8_lossy(&add.stderr).trim()
        );
        return None;
    }

    // Commit with task ID in the message
    let commit_msg = format!("feat: {}-completed - {}", task_id, message_suffix);
    let commit = Command::new("git")
        .args(["commit", "-m", &commit_msg])
        .current_dir(working_root)
        .output()
        .ok()?;

    if !commit.status.success() {
        tracing::warn!(
            "wrapper git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        );
        return None;
    }

    // Get the commit hash
    let hash = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(working_root)
        .output()
        .ok()?;

    if hash.status.success() {
        let h = String::from_utf8_lossy(&hash.stdout).trim().to_string();
        ui::emit(&format!(
            "Wrapper committed changes for task {} ({})",
            task_id,
            &h[..7.min(h.len())]
        ));
        Some(h)
    } else {
        None
    }
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

    /// Count of tasks reconciled — the pre-FEAT-010 `usize` shape these tests
    /// assert against. `reconcile_external_git_completions` now returns the
    /// reconciled ids (so the post-completion coordinator can review
    /// externally-completed `requires_human` tasks); these unit tests still
    /// verify the count via `.len()`.
    fn reconcile_external_count(
        external_repo: &Path,
        conn: &mut Connection,
        run_id: &str,
        prd_path: &Path,
        task_prefix: Option<&str>,
        scan_depth: usize,
    ) -> usize {
        reconcile_external_git_completions(
            external_repo,
            conn,
            run_id,
            prd_path,
            task_prefix,
            scan_depth,
        )
        .len()
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

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", 7);
        assert!(
            result.is_some(),
            "Should find task ID with -completed suffix in commit message"
        );
    }

    #[test]
    fn test_check_git_completion_case_insensitive() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: sec-h005-Completed lowercase");

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", 7);
        assert!(result.is_some(), "Should find task ID case-insensitively");
    }

    #[test]
    fn test_check_git_completion_returns_none_when_not_found() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: unrelated commit");

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", 7);
        assert!(
            result.is_none(),
            "Should return None when task ID not in commit"
        );
    }

    #[test]
    fn test_check_git_completion_returns_commit_hash() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: TASK-001-completed test");

        let result = check_git_for_task_completion(temp_dir.path(), "TASK-001", 7);
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

        let result = check_git_for_task_completion(temp_dir.path(), "TASK-001", 7);
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

        let result = check_git_for_task_completion(temp_dir.path(), "TASK-001", 7);
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

        let result = check_git_for_task_completion(repo.path(), "P3-FEAT-001", 7);
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

        let result = check_git_for_task_completion(repo.path(), "aeb10a1f-FIX-001", 7);
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

        let result = check_git_for_task_completion(repo.path(), "aeb10a1f-FIX-001", 7);
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

        let result = check_git_for_task_completion(repo.path(), "aeb10a1f-FIX-002", 7);
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

        let result = check_git_for_task_completion(repo.path(), "FEAT-001", 7);
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

        let count = reconcile_external_count(
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
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count =
            reconcile_external_count(ext_repo.path(), &mut conn, "run-1", &prd_path, None, 50);

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

        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count =
            reconcile_external_count(ext_repo.path(), &mut conn, "run-1", &prd_path, None, 50);

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

        let count =
            reconcile_external_count(ext_repo.path(), &mut conn, "run-1", &prd_path, None, 50);

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

        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count = reconcile_external_count(
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

        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count =
            reconcile_external_count(ext_repo.path(), &mut conn, "run-1", &prd_path, None, 50);

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

        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count =
            reconcile_external_count(ext_repo.path(), &mut conn, "run-1", &prd_path, None, 50);

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

        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count =
            reconcile_external_count(ext_repo.path(), &mut conn, "run-1", &prd_path, None, 50);

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

        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count =
            reconcile_external_count(ext_repo.path(), &mut conn, "run-1", &prd_path, None, 50);

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

        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count = reconcile_external_count(
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

        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let count = reconcile_external_count(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            Some("aeb10a1f"),
            50,
        );

        assert_eq!(count, 1, "Should match full prefixed ID in commit");
    }

    // ======================================================================
    // reconcile_merged_slot_completions() tests
    // ======================================================================

    /// Capture HEAD of `dir` as a 40-char SHA — analogue of the production
    /// `rev_parse_head` helper, kept local to tests to avoid pulling
    /// `worktree.rs` into the test dependency surface.
    fn rev_parse_head(dir: &std::path::Path) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("rev-parse HEAD");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Append a commit with a body containing `body_lines` separated by `\n\n`
    /// (subject + blank + body), exercising the body-match path.
    fn git_commit_with_body(dir: &std::path::Path, subject: &str, body: &str) {
        let msg = format!("{}\n\n{}", subject, body);
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", &msg])
            .current_dir(dir)
            .output()
            .expect("create commit with body");
    }

    fn write_simple_prd(dir: &std::path::Path, stories: &str) -> std::path::PathBuf {
        let prd_path = dir.join("prd.json");
        let content = format!(r#"{{"project":"test","userStories":[{}]}}"#, stories);
        std::fs::write(&prd_path, content).unwrap();
        prd_path
    }

    #[test]
    fn test_reconcile_merged_happy_path_body_match() {
        // (a) Slot worktree has commit with FEAT-001-COMPLETED in body, pre→HEAD range covers it.
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Feat one', 'in_progress', 1);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let pre = rev_parse_head(repo.path());
        git_commit_with_body(
            repo.path(),
            "feat: implement thing",
            "Body line.\n\nfeat: FEAT-001-completed - Implement feature",
        );

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"Feat one","passes":false,"priority":1}"#,
        );

        let reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert_eq!(reconciled, vec!["FEAT-001".to_string()]);
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id='FEAT-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");
        let prd: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&prd_path).unwrap()).unwrap();
        assert_eq!(prd["userStories"][0]["passes"], true);
    }

    #[test]
    fn test_reconcile_merged_no_match_returns_empty() {
        // (b) pre→HEAD has commits but none contain the marker.
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Feat one', 'in_progress', 1);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let pre = rev_parse_head(repo.path());
        git_commit(repo.path(), "chore: unrelated change");
        git_commit(repo.path(), "fix: another commit");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"Feat one","passes":false,"priority":1}"#,
        );

        let reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert!(reconciled.is_empty());
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id='FEAT-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress", "no DB write on no-match");
        let prd: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&prd_path).unwrap()).unwrap();
        assert_eq!(prd["userStories"][0]["passes"], false, "no PRD write");
    }

    #[test]
    fn test_reconcile_merged_no_op_merge_empty_range() {
        // (c) pre_merge_head == HEAD: empty range, git log emits empty stdout.
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Feat one', 'in_progress', 1);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let head = rev_parse_head(repo.path());

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"Feat one","passes":false,"priority":1}"#,
        );

        let reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &head,
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert!(reconciled.is_empty());
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id='FEAT-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn test_reconcile_merged_body_only_marker_matches() {
        // (d) Marker is in commit body, NOT subject; function still finds it.
        // The body of the previous "subject" line is the subject line; here we make
        // the subject deliberately marker-free.
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Feat one', 'in_progress', 1);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let pre = rev_parse_head(repo.path());
        git_commit_with_body(
            repo.path(),
            "chore: misc updates",
            "Some text. Resolves FEAT-001-COMPLETED on review.",
        );

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"Feat one","passes":false,"priority":1}"#,
        );

        let reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert_eq!(reconciled, vec!["FEAT-001".to_string()]);
    }

    #[test]
    fn test_reconcile_merged_cross_prd_prefix_isolation() {
        // (e) task_prefix Some("acdfb313") does NOT let `P9-FEAT-001-COMPLETED`
        // match `acdfb313-FEAT-001`.
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('acdfb313-FEAT-001', 'My task', 'in_progress', 1);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let pre = rev_parse_head(repo.path());
        // Foreign-PRD marker in body — must not poach acdfb313 row.
        git_commit_with_body(
            repo.path(),
            "feat: phase 9 sweep",
            "P9-FEAT-001-COMPLETED — completed in unrelated PRD",
        );

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"My task","passes":false,"priority":1}"#,
        );

        let reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            Some("acdfb313"),
        );

        assert!(
            reconciled.is_empty(),
            "P9-FEAT-001 must not match acdfb313-FEAT-001 under prefix scoping"
        );
        let status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id='acdfb313-FEAT-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn test_reconcile_merged_empty_pre_head_short_circuits() {
        // (f) Pass "" as pre_merge_head — function returns vec![] immediately
        // WITHOUT invoking git. Use a non-existent path to assert git wasn't called
        // (any git invocation would surface a git-log error in stderr but here we
        // also assert the early-return semantics).
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Feat one', 'in_progress', 1);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"Feat one","passes":false,"priority":1}"#,
        );

        // Path deliberately non-existent — the function must short-circuit on
        // the empty `pre_merge_head` before any `Command::new("git")` call, so
        // the path never matters. The assertions below only inspect the return
        // value and DB state.
        let bogus = std::path::Path::new("/nonexistent/path/that/does/not/exist");
        let reconciled =
            reconcile_merged_slot_completions(bogus, "", &mut conn, "run-1", &prd_path, None);

        assert!(reconciled.is_empty());
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id='FEAT-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn test_reconcile_merged_force_false_respects_dep_gating() {
        // (g) Task whose dependency is incomplete does NOT get marked done even
        // when marker is present (force=false invariant).
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Prereq', 'todo', 1),
             ('FEAT-002', 'Dependent', 'in_progress', 2);",
        )
        .unwrap();
        // FEAT-002 depends on FEAT-001 (which is todo, NOT done).
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('FEAT-002', 'FEAT-001', 'dependsOn')",
            [],
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let pre = rev_parse_head(repo.path());
        // Marker for FEAT-002 only; FEAT-001 prereq is still todo.
        git_commit_with_body(
            repo.path(),
            "feat: do dependent work",
            "feat: FEAT-002-completed - implement dependent",
        );

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"Prereq","passes":false,"priority":1},
               {"id":"FEAT-002","title":"Dependent","passes":false,"priority":2}"#,
        );

        let reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert!(
            reconciled.is_empty(),
            "FEAT-002 must not reconcile with unmet dep on FEAT-001"
        );
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id='FEAT-002'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            status, "in_progress",
            "Dep-gated task must remain in_progress"
        );
    }

    #[test]
    fn test_reconcile_merged_no_merges_excludes_resolver_merge() {
        // (h) Merge-commit poisoning defended: synthesize a merge commit whose
        // body carries FEAT-001-COMPLETED (e.g. via `git merge -m "Merge slot-1\n\nFEAT-001-completed"`);
        // confirm --no-merges filter excludes it and the task is NOT marked done.
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Feat one', 'in_progress', 1);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let pre = rev_parse_head(repo.path());

        // Create a side branch with a commit (NO marker) and merge it back with
        // a merge commit whose body carries the marker copied from another slot.
        std::process::Command::new("git")
            .args(["checkout", "-b", "side-branch"])
            .current_dir(repo.path())
            .output()
            .expect("checkout side-branch");
        git_commit(repo.path(), "chore: side work without marker");
        std::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(repo.path())
            .output()
            .expect("checkout main");
        // Force a merge commit (--no-ff) with a body containing the marker.
        std::process::Command::new("git")
            .args([
                "merge",
                "--no-ff",
                "-m",
                "Merge side-branch\n\nfeat: FEAT-001-completed - poisoned by resolver",
                "side-branch",
            ])
            .current_dir(repo.path())
            .output()
            .expect("create merge commit");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"Feat one","passes":false,"priority":1}"#,
        );

        let reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert!(
            reconciled.is_empty(),
            "--no-merges must exclude the merge commit; FEAT-001 stays in_progress"
        );
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id='FEAT-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn test_reconcile_merged_non_utf8_body_does_not_panic() {
        // (i) Non-UTF-8 commit body via `git commit-tree`. The String::from_utf8_lossy
        // path handles this; assert the call returns cleanly (Vec::new is fine).
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Feat one', 'in_progress', 1);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let pre = rev_parse_head(repo.path());

        // Build a commit with invalid UTF-8 in the message body via git commit-tree.
        // First, grab the current tree SHA and parent SHA.
        let tree_out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD^{tree}"])
            .current_dir(repo.path())
            .output()
            .expect("rev-parse tree");
        let tree = String::from_utf8_lossy(&tree_out.stdout).trim().to_string();
        let parent = pre.clone();

        // Compose a message with invalid UTF-8 bytes (0xFF 0xFE — not valid UTF-8 start).
        let mut msg_bytes: Vec<u8> = b"chore: invalid bytes\n\nbody-with-".to_vec();
        msg_bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD]);
        msg_bytes.extend_from_slice(b"-bytes\n");

        let mut child = std::process::Command::new("git")
            .args(["commit-tree", &tree, "-p", &parent])
            .current_dir(repo.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn commit-tree");
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(&msg_bytes)
            .expect("write msg");
        let out = child.wait_with_output().expect("commit-tree wait");
        assert!(out.status.success(), "commit-tree should succeed");
        let new_sha = String::from_utf8_lossy(&out.stdout).trim().to_string();

        // Update HEAD to the new commit.
        std::process::Command::new("git")
            .args(["update-ref", "HEAD", &new_sha])
            .current_dir(repo.path())
            .output()
            .expect("update-ref HEAD");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"Feat one","passes":false,"priority":1}"#,
        );

        // Must not panic; return empty vec (no marker in the lossy-decoded body).
        let reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert!(
            reconciled.is_empty(),
            "Non-UTF-8 body must not crash and must not produce a false match"
        );
    }

    #[test]
    fn test_reconcile_merged_returns_vec_string_contract() {
        // Pin the Vec<String> contract: if return type were usize, caller (FEAT-003)
        // couldn't drain pending_slot_tasks without a re-query. Two reconciliations
        // must yield exactly the two IDs.
        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'One', 'in_progress', 1),
             ('FEAT-002', 'Two', 'in_progress', 2);",
        )
        .unwrap();
        crate::loop_engine::test_utils::insert_run(&conn, "run-1");

        let repo = crate::loop_engine::test_utils::setup_git_repo();
        let pre = rev_parse_head(repo.path());
        git_commit_with_body(
            repo.path(),
            "feat: do one",
            "feat: FEAT-001-completed - first",
        );
        git_commit_with_body(
            repo.path(),
            "feat: do two",
            "feat: FEAT-002-completed - second",
        );

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = write_simple_prd(
            prd_dir.path(),
            r#"{"id":"FEAT-001","title":"One","passes":false,"priority":1},
               {"id":"FEAT-002","title":"Two","passes":false,"priority":2}"#,
        );

        let mut reconciled = reconcile_merged_slot_completions(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );
        reconciled.sort();

        assert_eq!(
            reconciled,
            vec!["FEAT-001".to_string(), "FEAT-002".to_string()],
            "Return value must contain the IDs (not just a count)"
        );
    }
}
