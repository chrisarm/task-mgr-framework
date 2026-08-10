//! Output parsing: extract task completion signals from Claude's raw text output.
//!
//! Functions here are pure or near-pure — they scan strings (and, for
//! `scan_output_for_completed_tasks`, the task DB) to determine which tasks
//! Claude reported as complete in a given iteration.

use rusqlite::Connection;

use crate::db::prefix::prefix_and;

/// Strip the auto-generated task prefix from a DB task ID to recover the base ID.
///
/// e.g., `strip_task_prefix("aeb10a1f-FIX-001", Some("aeb10a1f"))` → `"FIX-001"`
///       `strip_task_prefix("P5.1-FIX-001", Some("P5.1"))` → `"FIX-001"`
///       `strip_task_prefix("FIX-001", None)` → `"FIX-001"`
pub(crate) fn strip_task_prefix<'a>(task_id: &'a str, prefix: Option<&str>) -> &'a str {
    match prefix {
        Some(pfx) => {
            let with_dash = format!("{}-", pfx);
            task_id.strip_prefix(&with_dash).unwrap_or(task_id)
        }
        None => task_id,
    }
}

/// Strip a standard 8-lowercase-hex PRD prefix (`[0-9a-f]{8}-`) from a task id.
///
/// Mirrors the private `model::strip_prd_prefix` rule without coupling to
/// `model.rs`. Only exactly 8 lowercase hex chars followed by `-` are stripped
/// — never a generic first-dash split (would corrupt `REFACTOR-…` ids).
///
/// Returns the body after the prefix, or `id` unchanged when no valid prefix.
fn strip_8hex_prd_prefix(id: &str) -> &str {
    if id.len() > 9
        && id.as_bytes()[8] == b'-'
        && id[..8]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        &id[9..]
    } else {
        id
    }
}

/// True when `tag_id` names this iteration's claimed task (full id, prefix-
/// stripped bare form, or body after a standard 8-hex PRD prefix).
///
/// Match order (first hit wins):
/// 1. Identity: `tag_id == claimed_id`
/// 2. Explicit prefix strip: `strip_task_prefix(claimed, task_prefix)` is a
///    real strip (`bare != claimed`) and `tag_id == bare`
/// 3. 8-hex fallback: claimed has `[0-9a-f]{8}-…` and the body equals `tag_id`
///    (covers missing/wrong `task_prefix` on production ids)
///
/// Never uses unrestricted `ends_with` / substring matching — e.g. tag `"001"`
/// must not match claim `"97be64d7-REFACTOR-001"`.
pub(crate) fn tag_id_refers_to_claimed(
    tag_id: &str,
    claimed_id: &str,
    task_prefix: Option<&str>,
) -> bool {
    // (a) Identity
    if tag_id == claimed_id {
        return true;
    }

    // (b) Explicit prefix strip — only when strip actually removes something
    let bare = strip_task_prefix(claimed_id, task_prefix);
    if bare != claimed_id && tag_id == bare {
        return true;
    }

    // (c) 8-hex PRD prefix fallback (handles missing/wrong task_prefix)
    let body = strip_8hex_prd_prefix(claimed_id);
    body != claimed_id && tag_id == body
}

/// Resolve a status-tag id to the full claimed id when it refers to this
/// claim; otherwise return `tag_id` unchanged (identity on non-match).
///
/// Pure string helper — no DB lookup. Pipeline owns the rewrite loop.
pub(crate) fn resolve_tag_id_to_claimed<'a>(
    tag_id: &'a str,
    claimed_id: &'a str,
    task_prefix: Option<&str>,
) -> &'a str {
    if tag_id_refers_to_claimed(tag_id, claimed_id, task_prefix) {
        claimed_id
    } else {
        tag_id
    }
}

/// Parse `<completed>TASK-ID</completed>` tags from Claude's output.
///
/// Returns a vec of full task IDs found. Multiple tags per iteration are supported.
/// This is the primary completion signal — explicit declaration vs. mere mention.
pub(crate) fn parse_completed_tasks(output: &str) -> Vec<String> {
    let mut results = Vec::new();
    let start_tag = "<completed>";
    let end_tag = "</completed>";
    let mut search_from = 0;

    while let Some(start_pos) = output[search_from..].find(start_tag) {
        let abs_start = search_from + start_pos;
        let content_start = abs_start + start_tag.len();
        if let Some(end_pos) = output[content_start..].find(end_tag) {
            let task_id = output[content_start..content_start + end_pos].trim();
            if !task_id.is_empty() {
                results.push(task_id.to_string());
            }
            search_from = content_start + end_pos + end_tag.len();
        } else {
            break;
        }
    }

    results
}

/// Check Claude's output for evidence the task was completed (commit message containing task ID).
///
/// Fallback for when Claude commits in a different repo than the working directory.
/// Looks for the task ID in brackets, e.g. `[FEAT-005]` in a commit message.
/// Requires full prefixed ID — no base ID fallback.
pub(crate) fn check_output_for_task_completion(output: &str, task_id: &str) -> bool {
    let pattern = format!("[{}]", task_id);
    output.contains(&pattern)
}

/// Scan Claude's output for any completed task IDs from the database.
///
/// Returns a list of task IDs found in the output (in bracket format like `[FEAT-005]`).
/// This catches cases where Claude completes tasks other than the one that was claimed,
/// or completes multiple tasks in a single iteration.
pub(crate) fn scan_output_for_completed_tasks(
    output: &str,
    conn: &Connection,
    task_prefix: Option<&str>,
) -> Vec<String> {
    let mut completed = Vec::new();

    // Query all non-done task IDs, scoped to this PRD's prefix.
    let (soct_pfx_clause, soct_pfx_param) = prefix_and(task_prefix);
    let soct_sql = format!(
        "SELECT id FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL {soct_pfx_clause}"
    );
    let mut stmt = match conn.prepare(&soct_sql) {
        Ok(s) => s,
        Err(_) => return completed,
    };

    let soct_params: Vec<&dyn rusqlite::types::ToSql> = match &soct_pfx_param {
        Some(p) => vec![p],
        None => vec![],
    };
    let task_ids: Vec<String> = stmt
        .query_map(soct_params.as_slice(), |row| row.get(0))
        .ok()
        .map(|rows| {
            rows.filter_map(|r: rusqlite::Result<String>| r.ok())
                .collect()
        })
        .unwrap_or_default();

    for task_id in task_ids {
        if check_output_for_task_completion(output, &task_id) {
            completed.push(task_id);
        }
    }

    completed
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- strip_task_prefix tests ---

    #[test]
    fn test_strip_task_prefix_with_uuid() {
        assert_eq!(
            strip_task_prefix("aeb10a1f-FIX-001", Some("aeb10a1f")),
            "FIX-001"
        );
    }

    #[test]
    fn test_strip_task_prefix_with_human_prefix() {
        assert_eq!(strip_task_prefix("P5.1-FIX-001", Some("P5.1")), "FIX-001");
    }

    #[test]
    fn test_strip_task_prefix_no_prefix() {
        assert_eq!(strip_task_prefix("FIX-001", None), "FIX-001");
    }

    #[test]
    fn test_strip_task_prefix_mismatch() {
        // Prefix doesn't match — returns original
        assert_eq!(
            strip_task_prefix("OTHER-FIX-001", Some("aeb10a1f")),
            "OTHER-FIX-001"
        );
    }

    // --- tag_id_refers_to_claimed / resolve_tag_id_to_claimed ---
    // Table rows from FEAT-001 acceptance criteria. Known-bad: ends_with-only
    // implementations must FAIL the '001' negative test.

    #[test]
    fn resolve_bare_story_with_matching_prefix_to_full_claim() {
        // Positive: resolve('REFACTOR-001', '97be64d7-REFACTOR-001', Some('97be64d7')) → full
        let claimed = "97be64d7-REFACTOR-001";
        assert!(tag_id_refers_to_claimed(
            "REFACTOR-001",
            claimed,
            Some("97be64d7")
        ));
        assert_eq!(
            resolve_tag_id_to_claimed("REFACTOR-001", claimed, Some("97be64d7")),
            claimed
        );
    }

    #[test]
    fn resolve_full_id_identity() {
        // Positive: resolve(full, full, Some(prefix)) → full (identity)
        let claimed = "97be64d7-REFACTOR-001";
        assert!(tag_id_refers_to_claimed(claimed, claimed, Some("97be64d7")));
        assert_eq!(
            resolve_tag_id_to_claimed(claimed, claimed, Some("97be64d7")),
            claimed
        );
    }

    #[test]
    fn resolve_bare_story_via_8hex_fallback_when_prefix_none() {
        // Positive: resolve('REFACTOR-001', '97be64d7-REFACTOR-001', None) → full via 8-hex
        let claimed = "97be64d7-REFACTOR-001";
        assert!(tag_id_refers_to_claimed("REFACTOR-001", claimed, None));
        assert_eq!(
            resolve_tag_id_to_claimed("REFACTOR-001", claimed, None),
            claimed
        );
    }

    #[test]
    fn resolve_bare_equals_claimed_identity() {
        // Positive: resolve('REFACTOR-001', 'REFACTOR-001', None) → identity
        assert!(tag_id_refers_to_claimed(
            "REFACTOR-001",
            "REFACTOR-001",
            None
        ));
        assert_eq!(
            resolve_tag_id_to_claimed("REFACTOR-001", "REFACTOR-001", None),
            "REFACTOR-001"
        );
    }

    #[test]
    fn resolve_other_story_unchanged() {
        // Negative: resolve('OTHER-001', '97be64d7-REFACTOR-001', Some('97be64d7')) → OTHER-001
        let claimed = "97be64d7-REFACTOR-001";
        assert!(!tag_id_refers_to_claimed(
            "OTHER-001",
            claimed,
            Some("97be64d7")
        ));
        assert_eq!(
            resolve_tag_id_to_claimed("OTHER-001", claimed, Some("97be64d7")),
            "OTHER-001"
        );
    }

    #[test]
    fn resolve_partial_suffix_001_unchanged_no_loose_ends_with() {
        // Negative (Known-bad guard): resolve('001', full, Some(prefix)) → '001' unchanged.
        // An implementation using claimed.ends_with(tag_id) alone would FAIL this.
        let claimed = "97be64d7-REFACTOR-001";
        assert!(!tag_id_refers_to_claimed("001", claimed, Some("97be64d7")));
        assert_eq!(
            resolve_tag_id_to_claimed("001", claimed, Some("97be64d7")),
            "001"
        );
        // Also guard pure ends_with would have matched:
        assert!(
            claimed.ends_with("001"),
            "precondition: ends_with alone would falsely match"
        );
    }

    #[test]
    fn resolve_non_hex_custom_prefix_without_task_prefix_no_rewrite() {
        // Negative: resolve('FEAT-001', 'P5.1-FEAT-001', None) → FEAT-001 unchanged
        // (non-hex custom prefix + no task_prefix → 8-hex fallback does not apply)
        let claimed = "P5.1-FEAT-001";
        assert!(!tag_id_refers_to_claimed("FEAT-001", claimed, None));
        assert_eq!(
            resolve_tag_id_to_claimed("FEAT-001", claimed, None),
            "FEAT-001"
        );
    }

    #[test]
    fn resolve_bare_story_via_8hex_when_task_prefix_wrong() {
        // Positive: resolve('REFACTOR-001', full, Some('wrongpfx')) → full via 8-hex fallback
        let claimed = "97be64d7-REFACTOR-001";
        assert!(tag_id_refers_to_claimed(
            "REFACTOR-001",
            claimed,
            Some("wrongpfx")
        ));
        assert_eq!(
            resolve_tag_id_to_claimed("REFACTOR-001", claimed, Some("wrongpfx")),
            claimed
        );
    }

    #[test]
    fn resolve_identity_when_no_strip_and_no_8hex() {
        // Failure mode: strip returns claimed unchanged and 8-hex does not apply → identity, no panic
        let claimed = "P5.1-FEAT-001";
        assert!(!tag_id_refers_to_claimed(
            "OTHER-001",
            claimed,
            Some("wrong")
        ));
        assert_eq!(
            resolve_tag_id_to_claimed("OTHER-001", claimed, Some("wrong")),
            "OTHER-001"
        );
        // bare claimed with wrong tag
        assert!(!tag_id_refers_to_claimed("OTHER-001", "FEAT-001", None));
        assert_eq!(
            resolve_tag_id_to_claimed("OTHER-001", "FEAT-001", None),
            "OTHER-001"
        );
    }

    // --- check_output_for_task_completion tests ---

    #[test]
    fn test_check_output_finds_task_id_in_brackets() {
        let output =
            "Some output\nfeat: [FEAT-005] Implement Tool Declarations module\nMore output";
        assert!(check_output_for_task_completion(output, "FEAT-005"));
    }

    #[test]
    fn test_check_output_returns_false_when_not_found() {
        let output = "Some output without any task references";
        assert!(!check_output_for_task_completion(output, "FEAT-005"));
    }

    #[test]
    fn test_check_output_requires_brackets() {
        // Task ID without brackets should NOT match
        let output = "feat: FEAT-005 Implement something";
        assert!(!check_output_for_task_completion(output, "FEAT-005"));
    }

    #[test]
    fn test_check_output_empty_output() {
        assert!(!check_output_for_task_completion("", "FEAT-005"));
    }

    #[test]
    fn test_check_output_no_match_base_id() {
        // Output has "[FIX-001]", DB ID is "aeb10a1f-FIX-001" — should NOT match
        let output = "feat: [FIX-001] Fix the bug";
        assert!(
            !check_output_for_task_completion(output, "aeb10a1f-FIX-001"),
            "Should NOT match base ID without prefix in brackets"
        );
    }

    // --- parse_completed_tasks tests ---

    #[test]
    fn test_parse_completed_single_tag() {
        let output = "Done!\n<completed>11dc526c-FEAT-001</completed>\n";
        let result = parse_completed_tasks(output);
        assert_eq!(result, vec!["11dc526c-FEAT-001"]);
    }

    #[test]
    fn test_parse_completed_multiple_tags() {
        let output = "<completed>ID-001</completed> and <completed>ID-002</completed>";
        let result = parse_completed_tasks(output);
        assert_eq!(result, vec!["ID-001", "ID-002"]);
    }

    #[test]
    fn test_parse_completed_no_tags() {
        let output = "Just mentioning 11dc526c-FEAT-001 in output";
        let result = parse_completed_tasks(output);
        assert!(result.is_empty(), "Bare mention should not match");
    }

    #[test]
    fn test_parse_completed_empty_tag() {
        let output = "<completed></completed>";
        let result = parse_completed_tasks(output);
        assert!(result.is_empty(), "Empty tag should be ignored");
    }

    #[test]
    fn test_parse_completed_malformed_no_close() {
        let output = "<completed>FEAT-001 some text";
        let result = parse_completed_tasks(output);
        assert!(
            result.is_empty(),
            "Malformed tag without close should be ignored"
        );
    }

    #[test]
    fn test_parse_completed_whitespace_trimmed() {
        let output = "<completed>  FEAT-001  </completed>";
        let result = parse_completed_tasks(output);
        assert_eq!(result, vec!["FEAT-001"]);
    }

    // --- scan_output_for_completed_tasks tests ---

    #[test]
    fn test_scan_output_finds_multiple_task_ids() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        // Insert some tasks
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'todo', 1),
             ('FEAT-002', 'Task 2', 'in_progress', 2),
             ('FEAT-003', 'Task 3', 'done', 3),
             ('FEAT-004', 'Task 4', 'todo', 4);",
        )
        .unwrap();

        let output = "feat: [FEAT-001] First task\nfeat: [FEAT-002] Second task\nfeat: [FEAT-003] Already done";
        let completed = scan_output_for_completed_tasks(output, &conn, None);

        // Should find FEAT-001 and FEAT-002 (not done), skip FEAT-003 (already done), miss FEAT-004 (not in output)
        assert_eq!(completed.len(), 2);
        assert!(completed.contains(&"FEAT-001".to_string()));
        assert!(completed.contains(&"FEAT-002".to_string()));
    }

    #[test]
    fn test_scan_output_returns_empty_when_no_matches() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES ('FEAT-001', 'Task 1', 'todo', 1);",
        )
        .unwrap();

        let output = "No task IDs in brackets here";
        let completed = scan_output_for_completed_tasks(output, &conn, None);
        assert!(completed.is_empty());
    }

    #[test]
    fn test_output_scan_counts_multiple_completed_tasks() {
        // Regression: output scanning may find N>1 tasks completed in a single
        // iteration. The loop should increment tasks_completed by N, not 0.
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('P3-FEAT-001', 'Task 1', 'todo', 1),
             ('P3-FEAT-002', 'Task 2', 'todo', 2),
             ('P3-FEAT-003', 'Task 3', 'todo', 3);",
        )
        .unwrap();

        let output = "Completed [P3-FEAT-001] and [P3-FEAT-002] in same iteration\n\
                      Also finished [P3-FEAT-003] as a bonus";
        let completed = scan_output_for_completed_tasks(output, &conn, None);

        assert_eq!(
            completed.len(),
            3,
            "Output scan should find all 3 tasks — loop increments counter by 3"
        );
    }

    #[test]
    fn test_output_scan_skips_already_done_tasks() {
        // Regression: output scanning only finds non-done tasks, so completing
        // a task via git detection then running output scan won't double-count.
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'done', 1),
             ('FEAT-002', 'Task 2', 'todo', 2);",
        )
        .unwrap();

        let output = "Completed [FEAT-001] and [FEAT-002]";
        let completed = scan_output_for_completed_tasks(output, &conn, None);

        assert_eq!(
            completed.len(),
            1,
            "Should only find FEAT-002, not already-done FEAT-001"
        );
        assert_eq!(completed[0], "FEAT-002");
    }

    #[test]
    fn test_output_scan_no_match_base_id() {
        // DB has prefixed ID, output has unprefixed bracket tag — should NOT match
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('aeb10a1f-FIX-001', 'Fix bug', 'todo', 1);",
        )
        .unwrap();

        let output = "Completed [FIX-001] successfully";
        let completed = scan_output_for_completed_tasks(output, &conn, Some("aeb10a1f"));

        assert!(
            completed.is_empty(),
            "Should NOT match base ID without prefix in brackets"
        );
    }

    #[test]
    fn test_scan_output_scoped_to_p1_only() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('P1-TASK-001', 'P1 task', 'todo', 1),
             ('P2-TASK-001', 'P2 task', 'todo', 1);",
        )
        .unwrap();

        // Output mentions both task IDs in bracket format
        let output = "Completed [P1-TASK-001] and [P2-TASK-001]";

        let completed = scan_output_for_completed_tasks(output, &conn, Some("P1"));

        assert_eq!(
            completed,
            vec!["P1-TASK-001".to_string()],
            "Only P1 task should be detected when prefix is P1"
        );
    }

    #[test]
    fn test_scan_output_none_prefix_matches_all() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('P1-TASK-001', 'P1 task', 'todo', 1),
             ('P2-TASK-001', 'P2 task', 'todo', 1);",
        )
        .unwrap();

        let output = "Completed [P1-TASK-001] and [P2-TASK-001]";
        let mut completed = scan_output_for_completed_tasks(output, &conn, None);
        completed.sort();

        assert_eq!(
            completed,
            vec!["P1-TASK-001".to_string(), "P2-TASK-001".to_string()],
            "None prefix should match all task IDs in output"
        );
    }

    #[test]
    fn test_scan_output_excludes_done_tasks() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('P1-TASK-001', 'P1 done',  'done', 1),
             ('P1-TASK-002', 'P1 todo',  'todo', 2);",
        )
        .unwrap();

        // Both are mentioned in output but done tasks should not appear
        let output = "Completed [P1-TASK-001] and [P1-TASK-002]";
        let completed = scan_output_for_completed_tasks(output, &conn, Some("P1"));

        assert_eq!(
            completed,
            vec!["P1-TASK-002".to_string()],
            "Done tasks should not appear in scan results"
        );
    }
}
