//! Tests for the next command module.

#[cfg(test)]
mod test_helpers {
    use crate::db::migrations::run_migrations;
    use crate::db::{create_schema, open_connection};
    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    pub fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    pub fn insert_test_task(conn: &Connection, id: &str, title: &str, status: &str, priority: i32) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
            params![id, title, status, priority],
        )
        .unwrap();
    }

    pub fn insert_test_task_file(conn: &Connection, task_id: &str, file_path: &str) {
        conn.execute(
            "INSERT INTO task_files (task_id, file_path) VALUES (?, ?)",
            params![task_id, file_path],
        )
        .unwrap();
    }

    pub fn insert_test_relationship(
        conn: &Connection,
        task_id: &str,
        related_id: &str,
        rel_type: &str,
    ) {
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?, ?, ?)",
            params![task_id, related_id, rel_type],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod selection_tests {
    use super::test_helpers::{
        insert_test_relationship, insert_test_task, insert_test_task_file, setup_test_db,
    };
    use crate::commands::next::selection::{
        ScoreBreakdown, ScoredTask, format_text, select_next_task,
    };
    use crate::models::Task;

    #[test]
    fn test_select_no_tasks() {
        let (_temp_dir, conn) = setup_test_db();

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_none());
        assert_eq!(result.eligible_count, 0);
    }

    #[test]
    fn test_select_single_todo_task() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 10);

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        assert_eq!(result.task.unwrap().task.id, "US-001");
        assert_eq!(result.eligible_count, 1);
    }

    #[test]
    fn test_select_by_priority() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Low Priority", "todo", 50);
        insert_test_task(&conn, "US-002", "High Priority", "todo", 10);
        insert_test_task(&conn, "US-003", "Medium Priority", "todo", 30);

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        let task = result.task.unwrap();
        // Higher priority (lower number) should be selected
        assert_eq!(task.task.id, "US-002");
        assert_eq!(task.score_breakdown.priority_score, 990); // 1000 - 10
    }

    #[test]
    fn test_select_ignores_done_tasks() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Done Task", "done", 1);
        insert_test_task(&conn, "US-002", "Todo Task", "todo", 50);

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        assert_eq!(result.task.unwrap().task.id, "US-002");
        assert_eq!(result.eligible_count, 1);
    }

    #[test]
    fn test_select_ignores_blocked_tasks() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Blocked Task", "blocked", 1);
        insert_test_task(&conn, "US-002", "Todo Task", "todo", 50);

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        assert_eq!(result.task.unwrap().task.id, "US-002");
    }

    #[test]
    fn test_dependencies_block_selection() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Prereq Task", "todo", 50);
        insert_test_task(&conn, "US-002", "Dependent Task", "todo", 10);
        insert_test_relationship(&conn, "US-002", "US-001", "dependsOn");

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        // US-002 has higher priority but is blocked, so US-001 should be selected
        assert_eq!(result.task.unwrap().task.id, "US-001");
        assert_eq!(result.eligible_count, 1);
    }

    #[test]
    fn test_dependencies_satisfied_by_done() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Prereq Task", "done", 50);
        insert_test_task(&conn, "US-002", "Dependent Task", "todo", 10);
        insert_test_relationship(&conn, "US-002", "US-001", "dependsOn");

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        // Dependency is satisfied, so US-002 should be selected
        assert_eq!(result.task.unwrap().task.id, "US-002");
    }

    #[test]
    fn test_dependencies_satisfied_by_irrelevant() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Prereq Task", "irrelevant", 50);
        insert_test_task(&conn, "US-002", "Dependent Task", "todo", 10);
        insert_test_relationship(&conn, "US-002", "US-001", "dependsOn");

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        // Dependency is satisfied by irrelevant status
        assert_eq!(result.task.unwrap().task.id, "US-002");
    }

    #[test]
    fn test_file_overlap_scoring() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task A", "todo", 50);
        insert_test_task(&conn, "US-002", "Task B", "todo", 50);
        insert_test_task_file(&conn, "US-001", "src/commands/init.rs");
        insert_test_task_file(&conn, "US-002", "src/models/task.rs");

        // Pass after_files that overlap with US-001
        let after_files = vec!["src/commands/init.rs".to_string()];
        let result = select_next_task(&conn, &after_files, None).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();
        // US-001 should be selected due to file overlap
        assert_eq!(task.task.id, "US-001");
        assert_eq!(task.score_breakdown.file_score, 10);
        assert_eq!(task.score_breakdown.file_overlap_count, 1);
    }

    #[test]
    fn test_multiple_file_overlaps() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task A", "todo", 50);
        insert_test_task_file(&conn, "US-001", "src/commands/init.rs");
        insert_test_task_file(&conn, "US-001", "src/commands/list.rs");
        insert_test_task_file(&conn, "US-001", "src/models/task.rs");

        // Pass after_files with multiple overlaps
        let after_files = vec![
            "src/commands/init.rs".to_string(),
            "src/commands/list.rs".to_string(),
        ];
        let result = select_next_task(&conn, &after_files, None).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();
        assert_eq!(task.score_breakdown.file_score, 20); // 2 overlaps * 10
        assert_eq!(task.score_breakdown.file_overlap_count, 2);
    }

    #[test]
    fn test_synergy_relationships_ignored_in_scoring() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Completed Task", "done", 1);
        insert_test_task(&conn, "US-002", "Task A", "todo", 50);
        insert_test_task(&conn, "US-003", "Task B", "todo", 50);
        insert_test_relationship(&conn, "US-002", "US-001", "synergyWith");

        // synergyWith relationships are no longer scored; both tasks have equal scores
        let result = select_next_task(&conn, &[], None).unwrap();

        assert!(result.task.is_some());
        // Both have same priority (50), US-002 is selected first (stable sort)
        assert_eq!(result.eligible_count, 2);
    }

    #[test]
    fn test_conflicts_relationships_ignored_in_scoring() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Completed Task", "done", 1);
        insert_test_task(&conn, "US-002", "Task A", "todo", 50);
        insert_test_task(&conn, "US-003", "Task B", "todo", 50);
        insert_test_relationship(&conn, "US-002", "US-001", "conflictsWith");

        // conflictsWith relationships are no longer scored; selection is by priority only
        let result = select_next_task(&conn, &[], None).unwrap();

        assert!(result.task.is_some());
        assert_eq!(result.eligible_count, 2);
    }

    #[test]
    fn test_batch_with_relationships_not_in_selection_result() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Main Task", "todo", 10);
        insert_test_task(&conn, "FIX-001", "Former Batch Task", "todo", 50);
        insert_test_relationship(&conn, "US-001", "FIX-001", "batchWith");

        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        let task = result.task.unwrap();
        assert_eq!(task.task.id, "US-001");
        // batchWith is no longer tracked in ScoredTask or SelectionResult
        assert_eq!(result.eligible_count, 2);
    }

    #[test]
    fn test_combined_scoring() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Completed Prereq", "done", 1);
        insert_test_task(&conn, "US-002", "Low Priority With File", "todo", 50);
        insert_test_task(&conn, "US-003", "High Priority No Overlap", "todo", 20);
        insert_test_task_file(&conn, "US-002", "src/main.rs");

        let after_files = vec!["src/main.rs".to_string()];
        let result = select_next_task(&conn, &after_files, None).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();

        // US-002: 950 (priority) + 10 (file) = 960
        // US-003: 980 (priority) + 0 (file) = 980
        // US-003 should win with higher priority
        assert_eq!(task.task.id, "US-003");
    }

    #[test]
    fn test_combined_scoring_file_wins() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-002", "Lower Priority Many Files", "todo", 45);
        insert_test_task(&conn, "US-003", "Higher Priority No Overlap", "todo", 40);
        insert_test_task_file(&conn, "US-002", "src/main.rs");
        insert_test_task_file(&conn, "US-002", "src/lib.rs");
        insert_test_task_file(&conn, "US-002", "src/cli.rs");

        // 3 file overlaps should overcome small priority difference
        let after_files = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "src/cli.rs".to_string(),
        ];
        let result = select_next_task(&conn, &after_files, None).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();

        // US-002: 955 (priority) + 30 (file) = 985
        // US-003: 960 (priority) + 0 (file) = 960
        // US-002 should win
        assert_eq!(task.task.id, "US-002");
    }

    #[test]
    fn test_format_text_with_task() {
        use crate::commands::next::selection::SelectionResult;
        let result = SelectionResult {
            task: Some(ScoredTask {
                task: Task::new("US-001", "Test Task"),
                files: vec!["src/main.rs".to_string()],
                total_score: 960,
                score_breakdown: ScoreBreakdown {
                    priority_score: 950,
                    file_score: 10,
                    file_overlap_count: 1,
                },
            }),
            selection_reason: "Selected task US-001".to_string(),
            eligible_count: 5,
            top_candidates: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("Next Task: US-001"));
        assert!(text.contains("Score:    960"));
        assert!(text.contains("Priority:    +950"));
        assert!(text.contains("File Overlap: +10"));
        assert!(text.contains("src/main.rs"));
    }

    #[test]
    fn test_format_text_no_task() {
        use crate::commands::next::selection::SelectionResult;
        let result = SelectionResult {
            task: None,
            selection_reason: "All tasks completed".to_string(),
            eligible_count: 0,
            top_candidates: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("No tasks available"));
        assert!(text.contains("All tasks completed"));
    }

    // -------------------------------------------------------------------------
    // Parallel group selection (FEAT-002)
    //
    // These tests define the contract for `select_parallel_group()`. They are
    // `#[ignore]`d until FEAT-002 replaces the stub in selection.rs. The test
    // file still compiles because the stub exists.
    //
    // Contract:
    //   select_parallel_group(conn, after_files, task_prefix, max_slots) -> Vec<ScoredTask>
    //   - Greedy selection by descending score
    //   - Two tasks never appear together if their touchesFiles overlap
    //   - Tasks with NO touchesFiles entries have no conflicts → always eligible
    //   - Length capped by max_slots
    // -------------------------------------------------------------------------

    use crate::commands::next::selection::select_parallel_group;

    #[test]
    fn test_parallel_group_two_tasks_sharing_file_returns_one() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task A", "todo", 10);
        insert_test_task(&conn, "US-002", "Task B", "todo", 20);
        insert_test_task_file(&conn, "US-001", "src/shared.rs");
        insert_test_task_file(&conn, "US-002", "src/shared.rs");

        let group = select_parallel_group(&conn, &[], None, 4).unwrap();
        assert_eq!(
            group.len(),
            1,
            "two tasks sharing a file must not parallelize"
        );
        // Higher priority (lower number) wins the slot
        assert_eq!(group[0].task.id, "US-001");
    }

    #[test]
    fn test_parallel_group_two_disjoint_tasks_returns_two() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task A", "todo", 10);
        insert_test_task(&conn, "US-002", "Task B", "todo", 20);
        insert_test_task_file(&conn, "US-001", "src/a.rs");
        insert_test_task_file(&conn, "US-002", "src/b.rs");

        let group = select_parallel_group(&conn, &[], None, 4).unwrap();
        assert_eq!(group.len(), 2, "disjoint files must parallelize");
        let ids: Vec<&str> = group.iter().map(|s| s.task.id.as_str()).collect();
        assert!(ids.contains(&"US-001"));
        assert!(ids.contains(&"US-002"));
    }

    #[test]
    fn test_parallel_group_empty_touches_files_always_parallelize() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "With file", "todo", 10);
        insert_test_task(&conn, "US-002", "No files A", "todo", 20);
        insert_test_task(&conn, "US-003", "No files B", "todo", 30);
        insert_test_task_file(&conn, "US-001", "src/a.rs");
        // US-002 and US-003 have zero task_files rows — no conflicts possible.

        let group = select_parallel_group(&conn, &[], None, 4).unwrap();
        assert_eq!(
            group.len(),
            3,
            "tasks with empty touchesFiles can always parallelize"
        );
        let ids: Vec<&str> = group.iter().map(|s| s.task.id.as_str()).collect();
        assert!(ids.contains(&"US-001"));
        assert!(ids.contains(&"US-002"));
        assert!(ids.contains(&"US-003"));
    }

    #[test]
    fn test_parallel_group_respects_max_slots() {
        let (_temp_dir, conn) = setup_test_db();
        // 5 tasks with fully-disjoint files; max_slots=3 → group truncated to 3.
        for i in 1..=5i32 {
            let id = format!("US-00{i}");
            let title = format!("Task {i}");
            let file = format!("src/file_{i}.rs");
            insert_test_task(&conn, &id, &title, "todo", 10 + i);
            insert_test_task_file(&conn, &id, &file);
        }

        let group = select_parallel_group(&conn, &[], None, 3).unwrap();
        assert_eq!(group.len(), 3, "group size is capped by max_slots");
    }

    #[test]
    fn test_parallel_group_ordered_by_score_descending() {
        let (_temp_dir, conn) = setup_test_db();
        // Disjoint files so all three can parallelize; priorities out of order.
        insert_test_task(&conn, "US-001", "Low prio", "todo", 50);
        insert_test_task(&conn, "US-002", "High prio", "todo", 10);
        insert_test_task(&conn, "US-003", "Mid prio", "todo", 30);
        insert_test_task_file(&conn, "US-001", "src/a.rs");
        insert_test_task_file(&conn, "US-002", "src/b.rs");
        insert_test_task_file(&conn, "US-003", "src/c.rs");

        let group = select_parallel_group(&conn, &[], None, 3).unwrap();
        assert_eq!(group.len(), 3);
        // Descending total_score (highest first)
        assert!(
            group[0].total_score >= group[1].total_score,
            "group must be sorted by score desc"
        );
        assert!(
            group[1].total_score >= group[2].total_score,
            "group must be sorted by score desc"
        );
        assert_eq!(group[0].task.id, "US-002", "highest priority first");
        assert_eq!(group[1].task.id, "US-003");
        assert_eq!(group[2].task.id, "US-001");
    }

    #[test]
    fn test_parallel_group_single_eligible_task_returns_one() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Only Task", "todo", 10);
        insert_test_task_file(&conn, "US-001", "src/a.rs");

        let group = select_parallel_group(&conn, &[], None, 4).unwrap();
        assert_eq!(group.len(), 1);
        assert_eq!(group[0].task.id, "US-001");
    }

    #[test]
    fn test_parallel_group_all_sharing_one_file_forces_sequential() {
        let (_temp_dir, conn) = setup_test_db();
        // 4 eligible tasks all touching the same hot-spot file.
        for i in 1..=4i32 {
            let id = format!("US-00{i}");
            let title = format!("Task {i}");
            insert_test_task(&conn, &id, &title, "todo", 10 + i);
            insert_test_task_file(&conn, &id, "src/hot_spot.rs");
        }

        let group = select_parallel_group(&conn, &[], None, 4).unwrap();
        assert_eq!(
            group.len(),
            1,
            "all tasks sharing one file → group of 1 (sequential)"
        );
        // Highest priority (lowest priority number) wins the slot
        assert_eq!(group[0].task.id, "US-001");
    }

    /// Known-bad discriminator (AC #8): this test fails if the implementation
    /// skips the file-conflict check. Two high-priority tasks share a file;
    /// a lower-priority task has a disjoint file. Correct behavior returns
    /// exactly {US-001, US-003}. A naive "top-N by score" implementation with
    /// no conflict check would return all three (wrong: US-001 and US-002
    /// collide on src/shared.rs).
    #[test]
    fn test_parallel_group_known_bad_requires_conflict_check() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "High A", "todo", 10);
        insert_test_task(&conn, "US-002", "High B (conflicts)", "todo", 11);
        insert_test_task(&conn, "US-003", "Low disjoint", "todo", 99);
        insert_test_task_file(&conn, "US-001", "src/shared.rs");
        insert_test_task_file(&conn, "US-002", "src/shared.rs");
        insert_test_task_file(&conn, "US-003", "src/other.rs");

        let group = select_parallel_group(&conn, &[], None, 4).unwrap();
        let ids: Vec<&str> = group.iter().map(|s| s.task.id.as_str()).collect();

        assert_eq!(
            group.len(),
            2,
            "conflict check must drop US-002 even though score > US-003"
        );
        assert!(
            ids.contains(&"US-001"),
            "US-001 selected first (highest priority)"
        );
        assert!(
            ids.contains(&"US-003"),
            "US-003 selected (disjoint file, no conflict)"
        );
        assert!(
            !ids.contains(&"US-002"),
            "US-002 must be excluded (shares src/shared.rs with US-001)"
        );
    }
}

/// Tests for prefix-scoped task selection (TDD: written before implementation).
///
/// Tests marked `#[ignore]` require `select_next_task` to accept an
/// `Option<&str>` task_prefix parameter (SS-FEAT: prefix-scoped queries).
/// They will be un-ignored once that parameter exists.
///
/// The cross-PRD isolation test runs immediately and currently FAILS (red phase),
/// proving isolation is needed.
#[cfg(test)]
mod prefix_selection_tests {
    use super::test_helpers::{
        insert_test_relationship, insert_test_task, insert_test_task_file, setup_test_db,
    };
    use crate::commands::next::selection::select_next_task;

    // -------------------------------------------------------------------------
    // Backwards-compatibility: None prefix returns ALL tasks (existing behaviour)
    // This test must PASS now and continue to pass after the prefix param lands.
    // -------------------------------------------------------------------------

    #[test]
    fn test_select_next_task_none_prefix_returns_all() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "P1-US-001", "P1 Task", "todo", 10);
        insert_test_task(&conn, "P2-US-001", "P2 Task", "todo", 20);

        // Without prefix filtering both tasks are candidates; lowest priority wins.
        let result = select_next_task(&conn, &[], None).unwrap();
        assert!(result.task.is_some());
        assert_eq!(result.eligible_count, 2, "None-prefix must see all tasks");
        assert_eq!(result.task.unwrap().task.id, "P1-US-001");
    }

    // -------------------------------------------------------------------------
    // Known-bad discriminator: cross-PRD isolation is currently BROKEN.
    //
    // When two PRDs share a task ID suffix (P1-US-001 and P2-US-001) and we
    // want only P1 tasks, the current implementation returns both.  This test
    // documents the failure and must be fixed by the prefix-scoping feature.
    //
    // Once select_next_task accepts task_prefix this test should be replaced by
    // `test_cross_prd_isolation_with_prefix` below.
    // -------------------------------------------------------------------------

    #[test]
    fn test_cross_prd_isolation_currently_broken() {
        let (_dir, conn) = setup_test_db();
        // Two PRDs, same local ID suffix.
        insert_test_task(&conn, "P1-US-001", "P1 Task A", "todo", 10);
        insert_test_task(&conn, "P2-US-001", "P2 Task A", "todo", 20);
        insert_test_task(&conn, "P2-US-002", "P2 Task B", "todo", 5);

        // With prefix "P1" only P1-US-001 is eligible.
        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        assert_eq!(result.eligible_count, 1, "P1 scope must exclude P2 tasks");
        assert_eq!(result.task.unwrap().task.id, "P1-US-001");
    }

    // -------------------------------------------------------------------------
    // Prefix-scoped select_next_task
    // -------------------------------------------------------------------------

    #[test]
    fn test_select_next_task_prefix_p1_only() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "P1-US-001", "P1 Task", "todo", 10);
        insert_test_task(&conn, "P2-US-001", "P2 Task", "todo", 5); // higher priority but wrong PRD

        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        assert_eq!(result.eligible_count, 1, "P1 prefix must exclude P2 tasks");
        assert_eq!(result.task.unwrap().task.id, "P1-US-001");
    }

    #[test]
    fn test_select_next_task_prefix_p2_only() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "P1-US-001", "P1 Task", "todo", 5); // higher priority but wrong PRD
        insert_test_task(&conn, "P2-US-001", "P2 Task", "todo", 10);

        let result = select_next_task(&conn, &[], Some("P2")).unwrap();
        assert_eq!(result.eligible_count, 1, "P2 prefix must exclude P1 tasks");
        assert_eq!(result.task.unwrap().task.id, "P2-US-001");
    }

    // -------------------------------------------------------------------------
    // get_completed_task_ids with prefix
    // -------------------------------------------------------------------------

    #[test]
    fn test_get_completed_task_ids_with_prefix() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "P1-US-001", "P1 Done", "done", 1);
        insert_test_task(&conn, "P2-US-001", "P2 Done", "done", 1);
        insert_test_task(&conn, "P1-US-002", "P1 Dependent", "todo", 2);
        // P1-US-002 depends on P1-US-001.  With P1 prefix the dependency is
        // satisfied; P1-US-001 is in completed set so P1-US-002 is eligible.
        use super::test_helpers::insert_test_relationship;
        insert_test_relationship(&conn, "P1-US-002", "P1-US-001", "dependsOn");

        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        assert_eq!(
            result.eligible_count, 1,
            "P1-US-002 should be eligible: its dependency P1-US-001 is done within P1 scope"
        );
        assert_eq!(result.task.unwrap().task.id, "P1-US-002");
    }

    // -------------------------------------------------------------------------
    // get_todo_tasks with prefix
    // -------------------------------------------------------------------------

    #[test]
    fn test_get_todo_tasks_with_prefix() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "P1-US-001", "P1 Todo", "todo", 10);
        insert_test_task(&conn, "P2-US-001", "P2 Todo", "todo", 10);

        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        assert_eq!(
            result.eligible_count, 1,
            "prefix P1 must see only 1 todo task"
        );
        assert_eq!(result.task.unwrap().task.id, "P1-US-001");
    }

    // -------------------------------------------------------------------------
    // get_relationships_by_type with prefix
    // -------------------------------------------------------------------------

    #[test]
    fn test_get_relationships_by_type_with_prefix() {
        let (_dir, conn) = setup_test_db();
        // P1 tasks with a dependency chain.
        insert_test_task(&conn, "P1-US-001", "P1 Prereq", "done", 1);
        insert_test_task(&conn, "P1-US-002", "P1 Dependent", "todo", 2);
        insert_test_relationship(&conn, "P1-US-002", "P1-US-001", "dependsOn");
        // P2 tasks with the same local IDs — must NOT bleed into P1 scope.
        insert_test_task(&conn, "P2-US-001", "P2 Prereq", "todo", 1); // NOT done
        insert_test_task(&conn, "P2-US-002", "P2 Dependent", "todo", 2);
        insert_test_relationship(&conn, "P2-US-002", "P2-US-001", "dependsOn");

        // With prefix "P1":
        //   - P1-US-002's dependency (P1-US-001) is done → eligible
        //   - P2-* tasks are invisible
        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        assert_eq!(
            result.eligible_count, 1,
            "only P1-US-002 eligible in P1 scope"
        );
        assert_eq!(result.task.unwrap().task.id, "P1-US-002");
    }

    // -------------------------------------------------------------------------
    // get_all_task_files with prefix
    // -------------------------------------------------------------------------

    #[test]
    fn test_get_all_task_files_with_prefix() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "P1-US-001", "P1 Task", "todo", 10);
        insert_test_task(&conn, "P2-US-001", "P2 Task", "todo", 10);
        insert_test_task_file(&conn, "P1-US-001", "src/p1/mod.rs");
        insert_test_task_file(&conn, "P2-US-001", "src/p2/mod.rs");

        // With prefix "P1" and after_files=["src/p1/mod.rs"]:
        // Only P1-US-001 is a candidate; it gets a file-overlap bonus.
        let after_files = vec!["src/p1/mod.rs".to_string()];
        let result = select_next_task(&conn, &after_files, Some("P1")).unwrap();
        assert_eq!(result.eligible_count, 1, "P1 prefix must exclude P2 tasks");
        let task = result.task.unwrap();
        assert_eq!(task.task.id, "P1-US-001");
        assert!(
            task.score_breakdown.file_score > 0,
            "file overlap bonus must be applied for P1-US-001"
        );
    }

    // -------------------------------------------------------------------------
    // Cross-PRD isolation discriminator (the canonical correctness test)
    // -------------------------------------------------------------------------

    #[test]
    fn test_cross_prd_isolation_with_prefix() {
        let (_dir, conn) = setup_test_db();

        // P1 PRD: one todo task.
        insert_test_task(&conn, "P1-US-001", "P1 Task", "todo", 10);
        // P2 PRD: tasks with higher priority AND same suffix — must stay invisible to P1.
        insert_test_task(&conn, "P2-US-001", "P2 Task Same Suffix", "todo", 1);
        insert_test_task(&conn, "P2-US-002", "P2 Task", "todo", 2);

        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        assert_eq!(result.eligible_count, 1, "P1 scope must exclude P2 tasks");
        assert_eq!(result.task.unwrap().task.id, "P1-US-001");

        // Symmetry check: P2 scope must exclude P1-US-001
        let result2 = select_next_task(&conn, &[], Some("P2")).unwrap();
        assert_eq!(result2.eligible_count, 2, "P2 scope must see both P2 tasks");
        assert_eq!(result2.task.unwrap().task.id, "P2-US-001");
    }
}

#[cfg(test)]
mod next_command_tests {
    use super::test_helpers::{insert_test_relationship, insert_test_task, setup_test_db};
    use crate::commands::next::next;
    use crate::commands::next::output::{
        CandidateSummary, ClaimMetadata, LearningSummaryOutput, NextResult, NextTaskOutput,
        ScoreOutput, SelectionMetadata, build_task_output, format_next_text, format_next_verbose,
    };
    use crate::commands::next::selection::{ScoreBreakdown, ScoredTask};
    use crate::db::open_connection;
    use crate::loop_engine::model::{OPUS_MODEL, SONNET_MODEL};

    #[test]
    fn test_next_no_tasks() {
        let (temp_dir, conn) = setup_test_db();
        drop(conn);

        let result = next(temp_dir.path(), &[], false, None, false, None).unwrap();
        assert!(result.task.is_none());
        assert!(result.learnings.is_empty());
        assert!(result.claim.is_none());
        assert_eq!(result.selection.eligible_count, 0);
    }

    #[test]
    fn test_next_selects_task() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 10);
        drop(conn);

        let result = next(temp_dir.path(), &[], false, None, false, None).unwrap();
        assert!(result.task.is_some());
        let task = result.task.unwrap();
        assert_eq!(task.id, "US-001");
        assert_eq!(task.title, "First Task");
        assert_eq!(task.status, "todo"); // Not claimed, stays todo
        assert!(result.claim.is_none());
    }

    #[test]
    fn test_next_with_claim() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 10);
        drop(conn);

        let result = next(temp_dir.path(), &[], true, None, false, None).unwrap();
        assert!(result.task.is_some());
        let task = result.task.unwrap();
        assert_eq!(task.status, "in_progress"); // Claimed
        assert!(result.claim.is_some());
        let claim = result.claim.unwrap();
        assert!(claim.claimed);
        assert!(claim.run_id.is_none());
        assert_eq!(claim.iteration, 1);

        // Verify task status was updated in database
        let conn = open_connection(temp_dir.path()).unwrap();
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn test_next_with_claim_increments_iteration() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        insert_test_task(&conn, "US-002", "Task 2", "todo", 20);
        drop(conn);

        // First claim
        let result1 = next(temp_dir.path(), &[], true, None, false, None).unwrap();
        assert_eq!(result1.claim.unwrap().iteration, 1);

        // Complete first task so US-002 is next
        let conn = open_connection(temp_dir.path()).unwrap();
        conn.execute("UPDATE tasks SET status = 'done' WHERE id = 'US-001'", [])
            .unwrap();
        drop(conn);

        // Second claim
        let result2 = next(temp_dir.path(), &[], true, None, false, None).unwrap();
        assert_eq!(result2.claim.unwrap().iteration, 2);
    }

    #[test]
    fn test_next_with_claim_and_run_id() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 10);
        // Create an active run
        conn.execute(
            "INSERT INTO runs (run_id, status, started_at) VALUES ('test-run-123', 'active', datetime('now'))",
            [],
        )
        .unwrap();
        drop(conn);

        let result = next(
            temp_dir.path(),
            &[],
            true,
            Some("test-run-123"),
            false,
            None,
        )
        .unwrap();
        assert!(result.task.is_some());
        assert!(result.claim.is_some());
        let claim = result.claim.unwrap();
        assert!(claim.claimed);
        assert_eq!(claim.run_id, Some("test-run-123".to_string()));

        // Verify run_tasks entry was created
        let conn = open_connection(temp_dir.path()).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_tasks WHERE run_id = 'test-run-123' AND task_id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_next_claim_with_invalid_run_id() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 10);
        drop(conn);

        // Attempt to claim with a non-existent run_id
        let result = next(
            temp_dir.path(),
            &[],
            true,
            Some("nonexistent-run"),
            false,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Run not found"));
    }

    #[test]
    fn test_next_claim_with_inactive_run() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 10);
        // Create a completed (not active) run
        conn.execute(
            "INSERT INTO runs (run_id, status, started_at, ended_at) VALUES ('completed-run', 'completed', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        drop(conn);

        // Claiming with an inactive run should succeed (graceful degradation)
        // but without run linkage — the task gets claimed without a run_tasks entry.
        let result = next(
            temp_dir.path(),
            &[],
            true,
            Some("completed-run"),
            false,
            None,
        );
        assert!(result.is_ok(), "should succeed with warning, not error");

        // Verify task was claimed but no run_tasks entry was created
        let conn = crate::db::open_connection(temp_dir.path()).unwrap();
        let task_status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(task_status, "in_progress");

        let run_task_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_tasks WHERE run_id = 'completed-run'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(run_task_count, 0, "no run_tasks entry for inactive run");
    }

    #[test]
    fn test_next_output_includes_score_breakdown() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 20);
        drop(conn);

        let result = next(temp_dir.path(), &[], false, None, false, None).unwrap();
        let task = result.task.unwrap();
        assert_eq!(task.score.priority, 980); // 1000 - 20
        assert_eq!(task.score.file_overlap, 0);
        assert_eq!(task.score.total, 980);
    }

    #[test]
    fn test_next_batch_with_relationships_not_exposed() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Main Task", "todo", 10);
        insert_test_task(&conn, "FIX-001", "Former Batch Task", "todo", 20);
        insert_test_relationship(&conn, "US-001", "FIX-001", "batchWith");
        drop(conn);

        let result = next(temp_dir.path(), &[], false, None, false, None).unwrap();
        assert!(result.task.is_some());
        // batchWith is no longer tracked; both tasks are simply eligible
        assert_eq!(result.selection.eligible_count, 2);
    }

    #[test]
    fn test_format_next_text_with_claim() {
        let result = NextResult {
            task: Some(NextTaskOutput {
                id: "US-001".to_string(),
                title: "Test Task".to_string(),
                description: Some("A test description".to_string()),
                priority: 10,
                status: "in_progress".to_string(),
                acceptance_criteria: vec!["Criterion 1".to_string()],
                notes: None,
                files: vec!["src/main.rs".to_string()],
                model: None,
                difficulty: None,
                escalation_note: None,
                requires_human: false,
                score: ScoreOutput {
                    total: 990,
                    priority: 990,
                    file_overlap: 0,
                    file_overlap_count: 0,
                },
            }),
            learnings: vec![],
            selection: SelectionMetadata {
                reason: "Selected by priority".to_string(),
                eligible_count: 5,
            },
            claim: Some(ClaimMetadata {
                claimed: true,
                run_id: Some("run-123".to_string()),
                iteration: 3,
            }),
            top_candidates: vec![],
        };

        let text = format_next_text(&result);
        assert!(text.contains("Next Task: US-001 - Test Task"));
        assert!(text.contains("Status:   in_progress"));
        assert!(text.contains("Claimed: Yes (iteration: 3, run: run-123)"));
        assert!(text.contains("Acceptance Criteria:"));
        assert!(text.contains("[ ] Criterion 1"));
    }

    #[test]
    fn test_format_next_text_with_learnings() {
        let result = NextResult {
            task: Some(NextTaskOutput {
                id: "US-001".to_string(),
                title: "Test Task".to_string(),
                description: None,
                priority: 10,
                status: "todo".to_string(),
                acceptance_criteria: vec![],
                notes: None,
                files: vec![],
                model: None,
                difficulty: None,
                escalation_note: None,
                requires_human: false,
                score: ScoreOutput {
                    total: 990,
                    priority: 990,
                    file_overlap: 0,
                    file_overlap_count: 0,
                },
            }),
            learnings: vec![LearningSummaryOutput {
                id: 1,
                title: "Important pattern".to_string(),
                outcome: "pattern".to_string(),
                confidence: "high".to_string(),
                content: Some("This is the learning content".to_string()),
                applies_to_files: None,
                applies_to_task_types: Some(vec!["US-".to_string()]),
            }],
            selection: SelectionMetadata {
                reason: "Selected by priority".to_string(),
                eligible_count: 1,
            },
            claim: None,
            top_candidates: vec![],
        };

        let text = format_next_text(&result);
        assert!(text.contains("Relevant Learnings (1):"));
        assert!(text.contains("[pattern] Important pattern"));
        assert!(text.contains("high confidence"));
    }

    #[test]
    fn test_format_next_text_no_task() {
        let result = NextResult {
            task: None,
            learnings: vec![],
            selection: SelectionMetadata {
                reason: "All tasks have been completed".to_string(),
                eligible_count: 0,
            },
            claim: None,
            top_candidates: vec![],
        };

        let text = format_next_text(&result);
        assert!(text.contains("No tasks available"));
        assert!(text.contains("All tasks have been completed"));
    }

    #[test]
    fn test_next_with_verbose_includes_top_candidates() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "High Priority", "todo", 10);
        insert_test_task(&conn, "US-002", "Medium Priority", "todo", 20);
        insert_test_task(&conn, "US-003", "Low Priority", "todo", 30);
        drop(conn);

        // With verbose=true, should include top candidates
        let result = next(temp_dir.path(), &[], false, None, true, None).unwrap();
        assert!(result.task.is_some());
        assert!(!result.top_candidates.is_empty());
        assert_eq!(result.top_candidates.len(), 3); // All 3 tasks

        // First candidate should be the selected one (highest priority)
        assert_eq!(result.top_candidates[0].id, "US-001");
        assert_eq!(result.top_candidates[1].id, "US-002");
        assert_eq!(result.top_candidates[2].id, "US-003");
    }

    #[test]
    fn test_next_without_verbose_excludes_top_candidates() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Task 1", "todo", 10);
        insert_test_task(&conn, "US-002", "Task 2", "todo", 20);
        drop(conn);

        // With verbose=false, top_candidates should be empty
        let result = next(temp_dir.path(), &[], false, None, false, None).unwrap();
        assert!(result.task.is_some());
        assert!(result.top_candidates.is_empty());
    }

    // ===== TEST-003: build_task_output model field tests =====

    #[test]
    fn test_build_task_output_populates_model_fields() {
        let mut task = crate::models::Task::new("FEAT-001", "Model test task");
        task.model = Some(OPUS_MODEL.to_string());
        task.difficulty = Some("high".to_string());
        task.escalation_note = Some("Complex architectural decision".to_string());

        let scored = ScoredTask {
            task,
            files: vec!["src/lib.rs".to_string()],
            total_score: 990,
            score_breakdown: ScoreBreakdown {
                priority_score: 950,
                file_score: 0,
                file_overlap_count: 0,
            },
        };

        let output = build_task_output(&scored, false);

        assert_eq!(
            output.model,
            Some(OPUS_MODEL.to_string()),
            "model should be populated from scored_task"
        );
        assert_eq!(
            output.difficulty,
            Some("high".to_string()),
            "difficulty should be populated from scored_task"
        );
        assert_eq!(
            output.escalation_note,
            Some("Complex architectural decision".to_string()),
            "escalation_note should be populated from scored_task"
        );
    }

    #[test]
    fn test_build_task_output_none_model_fields() {
        let task = crate::models::Task::new("FEAT-002", "No model fields");

        let scored = ScoredTask {
            task,
            files: vec![],
            total_score: 950,
            score_breakdown: ScoreBreakdown {
                priority_score: 950,
                file_score: 0,
                file_overlap_count: 0,
            },
        };

        let output = build_task_output(&scored, false);

        assert!(output.model.is_none(), "model should be None when not set");
        assert!(
            output.difficulty.is_none(),
            "difficulty should be None when not set"
        );
        assert!(
            output.escalation_note.is_none(),
            "escalation_note should be None when not set"
        );
    }

    #[test]
    fn test_build_task_output_claimed_preserves_model_fields() {
        let mut task = crate::models::Task::new("FEAT-003", "Claimed with model");
        task.model = Some(SONNET_MODEL.to_string());
        task.difficulty = Some("medium".to_string());
        task.escalation_note = None;

        let scored = ScoredTask {
            task,
            files: vec![],
            total_score: 960,
            score_breakdown: ScoreBreakdown {
                priority_score: 960,
                file_score: 0,
                file_overlap_count: 0,
            },
        };

        let output = build_task_output(&scored, true);

        assert_eq!(
            output.status, "in_progress",
            "claimed task should be in_progress"
        );
        assert_eq!(
            output.model,
            Some(SONNET_MODEL.to_string()),
            "model should be preserved when claimed"
        );
        assert_eq!(
            output.difficulty,
            Some("medium".to_string()),
            "difficulty should be preserved when claimed"
        );
        assert!(
            output.escalation_note.is_none(),
            "escalation_note should remain None"
        );
    }

    // ===== TEST-003: format_next_text regression with model fields =====

    #[test]
    fn test_format_next_text_with_model_fields_no_regression() {
        let result = NextResult {
            task: Some(NextTaskOutput {
                id: "FEAT-001".to_string(),
                title: "Task with model".to_string(),
                description: Some("A test task with model fields".to_string()),
                priority: 10,
                status: "in_progress".to_string(),
                acceptance_criteria: vec!["AC1".to_string()],
                notes: None,
                files: vec!["src/lib.rs".to_string()],
                model: Some(OPUS_MODEL.to_string()),
                difficulty: Some("high".to_string()),
                escalation_note: Some("Complex task needing opus".to_string()),
                requires_human: false,
                score: ScoreOutput {
                    total: 990,
                    priority: 990,
                    file_overlap: 0,
                    file_overlap_count: 0,
                },
            }),
            learnings: vec![],
            selection: SelectionMetadata {
                reason: "Selected by priority".to_string(),
                eligible_count: 1,
            },
            claim: None,
            top_candidates: vec![],
        };

        let text = format_next_text(&result);
        // Core format_next_text output should still work with model fields populated
        assert!(
            text.contains("Next Task: FEAT-001 - Task with model"),
            "Task header should still render"
        );
        assert!(
            text.contains("Priority: 10"),
            "Priority should still render"
        );
        assert!(
            text.contains("Status:   in_progress"),
            "Status should still render"
        );
        assert!(
            text.contains("[ ] AC1"),
            "Acceptance criteria should still render"
        );
        assert!(text.contains("src/lib.rs"), "Files should still render");
    }

    #[test]
    fn test_format_next_verbose_output() {
        let result = NextResult {
            task: Some(NextTaskOutput {
                id: "US-001".to_string(),
                title: "Test Task".to_string(),
                description: None,
                priority: 10,
                status: "todo".to_string(),
                acceptance_criteria: vec![],
                notes: None,
                files: vec![],
                model: None,
                difficulty: None,
                escalation_note: None,
                requires_human: false,
                score: ScoreOutput {
                    total: 990,
                    priority: 990,
                    file_overlap: 0,
                    file_overlap_count: 0,
                },
            }),
            learnings: vec![],
            selection: SelectionMetadata {
                reason: "Selected by priority".to_string(),
                eligible_count: 3,
            },
            claim: None,
            top_candidates: vec![
                CandidateSummary {
                    id: "US-001".to_string(),
                    title: "Test Task".to_string(),
                    priority: 10,
                    total_score: 990,
                    score: ScoreOutput {
                        total: 990,
                        priority: 990,
                        file_overlap: 0,
                        file_overlap_count: 0,
                    },
                },
                CandidateSummary {
                    id: "US-002".to_string(),
                    title: "Second Task".to_string(),
                    priority: 20,
                    total_score: 980,
                    score: ScoreOutput {
                        total: 980,
                        priority: 980,
                        file_overlap: 0,
                        file_overlap_count: 0,
                    },
                },
            ],
        };

        let verbose_output = format_next_verbose(&result);
        assert!(verbose_output.contains("[verbose] Task Selection Scoring"));
        assert!(verbose_output.contains("US-001 - Test Task <- SELECTED"));
        assert!(verbose_output.contains("US-002 - Second Task"));
        assert!(verbose_output.contains("Total Score: 990"));
        assert!(verbose_output.contains("3 eligible tasks total"));
    }
}

#[cfg(test)]
mod decay_tests {
    use super::test_helpers::{insert_test_task, setup_test_db};
    use crate::commands::next::decay::{apply_decay, find_decay_warnings};

    #[test]
    fn test_apply_decay_with_zero_threshold() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Blocked Task", "blocked", 10);

        // Set the task as blocked at iteration 0
        conn.execute(
            "UPDATE tasks SET blocked_at_iteration = 0 WHERE id = 'US-001'",
            [],
        )
        .unwrap();

        // Set current iteration to 100
        conn.execute(
            "UPDATE global_state SET iteration_counter = 100 WHERE id = 1",
            [],
        )
        .unwrap();

        // With threshold 0, nothing should decay
        let decayed = apply_decay(&conn, 0, false, None).unwrap();
        assert!(decayed.is_empty());

        // Verify task is still blocked
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "blocked");
    }

    #[test]
    fn test_apply_decay_resets_blocked_task() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Blocked Task", "blocked", 10);

        // Set the task as blocked at iteration 5
        conn.execute(
            "UPDATE tasks SET blocked_at_iteration = 5 WHERE id = 'US-001'",
            [],
        )
        .unwrap();

        // Set current iteration to 40 (35 iterations since blocked, threshold is 32)
        conn.execute(
            "UPDATE global_state SET iteration_counter = 40 WHERE id = 1",
            [],
        )
        .unwrap();

        // Should decay the task
        let decayed = apply_decay(&conn, 32, false, None).unwrap();
        assert_eq!(decayed.len(), 1);
        assert_eq!(decayed[0].0, "US-001");
        assert_eq!(decayed[0].1, "blocked");

        // Verify task is now todo
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "todo");

        // Verify iteration tracking was cleared
        let blocked_at: Option<i64> = conn
            .query_row(
                "SELECT blocked_at_iteration FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(blocked_at.is_none());
    }

    #[test]
    fn test_apply_decay_resets_skipped_task() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-002", "Skipped Task", "skipped", 10);

        // Set the task as skipped at iteration 10
        conn.execute(
            "UPDATE tasks SET skipped_at_iteration = 10 WHERE id = 'US-002'",
            [],
        )
        .unwrap();

        // Set current iteration to 50 (40 iterations since skipped)
        conn.execute(
            "UPDATE global_state SET iteration_counter = 50 WHERE id = 1",
            [],
        )
        .unwrap();

        // Should decay the task
        let decayed = apply_decay(&conn, 32, false, None).unwrap();
        assert_eq!(decayed.len(), 1);
        assert_eq!(decayed[0].0, "US-002");
        assert_eq!(decayed[0].1, "skipped");

        // Verify task is now todo
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-002'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "todo");
    }

    #[test]
    fn test_apply_decay_does_not_reset_irrelevant_tasks() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-003", "Irrelevant Task", "irrelevant", 10);

        // Even if the task was blocked at iteration 0 and we set a very high iteration,
        // irrelevant tasks should never decay
        conn.execute(
            "UPDATE global_state SET iteration_counter = 1000 WHERE id = 1",
            [],
        )
        .unwrap();

        let decayed = apply_decay(&conn, 32, false, None).unwrap();
        assert!(decayed.is_empty());

        // Verify task is still irrelevant
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-003'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "irrelevant");
    }

    #[test]
    fn test_apply_decay_does_not_reset_tasks_within_threshold() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-004", "Recent Blocked", "blocked", 10);

        // Set the task as blocked at iteration 30
        conn.execute(
            "UPDATE tasks SET blocked_at_iteration = 30 WHERE id = 'US-004'",
            [],
        )
        .unwrap();

        // Set current iteration to 50 (only 20 iterations since blocked, threshold is 32)
        conn.execute(
            "UPDATE global_state SET iteration_counter = 50 WHERE id = 1",
            [],
        )
        .unwrap();

        let decayed = apply_decay(&conn, 32, false, None).unwrap();
        assert!(decayed.is_empty());

        // Verify task is still blocked
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-004'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "blocked");
    }

    #[test]
    fn test_apply_decay_adds_audit_note() {
        let (_dir, conn) = setup_test_db();
        // Create task with existing notes
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, notes, blocked_at_iteration) VALUES ('US-005', 'Task with Notes', 'blocked', 10, 'Original notes', 0)",
            [],
        )
        .unwrap();

        // Set current iteration to trigger decay
        conn.execute(
            "UPDATE global_state SET iteration_counter = 100 WHERE id = 1",
            [],
        )
        .unwrap();

        apply_decay(&conn, 32, false, None).unwrap();

        // Verify notes contain audit message
        let notes: String = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'US-005'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(notes.contains("Original notes"));
        assert!(notes.contains("[DECAY]"));
    }

    #[test]
    fn test_find_decay_warnings_returns_approaching_tasks() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-006", "Approaching Decay", "blocked", 10);

        // Set the task as blocked at iteration 20
        conn.execute(
            "UPDATE tasks SET blocked_at_iteration = 20 WHERE id = 'US-006'",
            [],
        )
        .unwrap();

        // Set current iteration to 48 (28 iterations since blocked, threshold 32, warning at 8)
        conn.execute(
            "UPDATE global_state SET iteration_counter = 48 WHERE id = 1",
            [],
        )
        .unwrap();

        let warnings = find_decay_warnings(&conn, 32, 8, None).unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].task_id, "US-006");
        assert_eq!(warnings[0].iterations_until_decay, 4);
    }

    #[test]
    fn test_find_decay_warnings_excludes_tasks_outside_warning_range() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-007", "Far from Decay", "blocked", 10);

        // Set the task as blocked at iteration 30
        conn.execute(
            "UPDATE tasks SET blocked_at_iteration = 30 WHERE id = 'US-007'",
            [],
        )
        .unwrap();

        // Set current iteration to 40 (10 iterations since blocked)
        // Warning window is iterations 24-32 before decay
        conn.execute(
            "UPDATE global_state SET iteration_counter = 40 WHERE id = 1",
            [],
        )
        .unwrap();

        let warnings = find_decay_warnings(&conn, 32, 8, None).unwrap();
        // 40 - 30 = 10 iterations since blocked
        // Would decay at 30 + 32 = 62
        // Warning window is 62 - 8 = 54 to 62
        // Current is 40, which is before 54, so no warning
        assert!(warnings.is_empty());
    }
}

/// Tests for task selection with a mixed-PRD database (SS-SS-TEST-004).
///
/// These tests verify that all selection behaviors (scoring, dependency checking,
/// synergy, conflict, batch grouping, decay) correctly respect prefix boundaries
/// when a task_prefix is set.
#[cfg(test)]
mod mixed_prd_selection_tests {
    use super::test_helpers::{insert_test_relationship, insert_test_task_file, setup_test_db};
    use crate::commands::next::decay::{apply_decay, find_decay_warnings};
    use crate::commands::next::selection::select_next_task;

    // ---------------------------------------------------------------------------
    // Fixture helpers
    // ---------------------------------------------------------------------------

    /// Insert a minimal task with a given ID prefix, e.g. "P1-US-001" or "P2-US-001".
    fn insert_prd_task(conn: &rusqlite::Connection, id: &str, status: &str, priority: i32) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
            rusqlite::params![id, format!("Task {id}"), status, priority],
        )
        .unwrap();
    }

    /// Build a rich fixture with P1 and P2 tasks sharing the same DB.
    ///
    /// P1 tasks:
    ///   P1-US-001  todo  priority=10  (synergy with P1-US-003; batch with P2-US-003)
    ///   P1-US-002  todo  priority=20  (depends on P1-US-003 which is done)
    ///   P1-US-003  done  priority=30
    ///   P1-US-004  todo  priority=10  (depends on P1-US-005 todo → blocked; also depends on P2-US-002 done)
    ///   P1-US-005  todo  priority=50  (conflict with P1-US-003)
    ///   P1-US-006  blocked
    ///   P1-US-007  skipped
    ///
    /// P2 tasks:
    ///   P2-US-001  todo  priority=1   (highest — wins when no prefix)
    ///   P2-US-002  done  priority=5
    ///   P2-US-003  todo  priority=50  (cross-PRD synergy: synergyWith P1-US-001)
    fn setup_mixed_prd_db() -> (tempfile::TempDir, rusqlite::Connection) {
        let (tmp, conn) = setup_test_db();

        // P1 tasks
        insert_prd_task(&conn, "P1-US-001", "todo", 10);
        insert_prd_task(&conn, "P1-US-002", "todo", 20);
        insert_prd_task(&conn, "P1-US-003", "done", 30);
        insert_prd_task(&conn, "P1-US-004", "todo", 10);
        insert_prd_task(&conn, "P1-US-005", "todo", 50);
        insert_prd_task(&conn, "P1-US-006", "blocked", 15);
        insert_prd_task(&conn, "P1-US-007", "skipped", 20);

        // P2 tasks
        insert_prd_task(&conn, "P2-US-001", "todo", 1);
        insert_prd_task(&conn, "P2-US-002", "done", 5);
        insert_prd_task(&conn, "P2-US-003", "todo", 50);

        // Intra-P1 relationships
        insert_test_relationship(&conn, "P1-US-002", "P1-US-003", "dependsOn");
        insert_test_relationship(&conn, "P1-US-001", "P1-US-003", "synergyWith");
        insert_test_relationship(&conn, "P1-US-005", "P1-US-003", "conflictsWith");
        insert_test_relationship(&conn, "P1-US-004", "P1-US-005", "dependsOn");

        // Cross-PRD: P1-US-004 also depends on P2-US-002 (done) — must be ignored when prefix=P1
        insert_test_relationship(&conn, "P1-US-004", "P2-US-002", "dependsOn");

        // Cross-PRD batch: P1-US-001 batchWith P2-US-003
        insert_test_relationship(&conn, "P1-US-001", "P2-US-003", "batchWith");

        // Cross-PRD synergy: P2-US-003 synergyWith P1-US-001
        insert_test_relationship(&conn, "P2-US-003", "P1-US-001", "synergyWith");

        // Files
        insert_test_task_file(&conn, "P1-US-001", "src/p1/main.rs");
        insert_test_task_file(&conn, "P1-US-002", "src/p1/util.rs");
        insert_test_task_file(&conn, "P2-US-001", "src/p2/main.rs");

        (tmp, conn)
    }

    // ---------------------------------------------------------------------------
    // Scoring: prefix filters P2 tasks out
    // ---------------------------------------------------------------------------

    #[test]
    fn test_prefix_filters_p2_tasks_from_selection() {
        let (_tmp, conn) = setup_mixed_prd_db();

        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        let task = result.task.expect("should select a P1 task");
        assert!(
            task.task.id.starts_with("P1-"),
            "expected P1 task, got {}",
            task.task.id
        );
    }

    /// P1-US-001 has file overlap; P2-OVERLAP would also match but must be excluded by prefix.
    #[test]
    fn test_file_overlap_score_respects_prefix() {
        let (_tmp, conn) = setup_test_db();

        insert_prd_task(&conn, "P1-HIGH-OVERLAP", "todo", 20);
        insert_prd_task(&conn, "P1-LOW-OVERLAP", "todo", 20);
        insert_test_task_file(&conn, "P1-HIGH-OVERLAP", "src/foo.rs");

        insert_prd_task(&conn, "P2-OVERLAP", "todo", 20);
        insert_test_task_file(&conn, "P2-OVERLAP", "src/foo.rs");

        let after_files = vec!["src/foo.rs".to_string()];
        let result = select_next_task(&conn, &after_files, Some("P1")).unwrap();
        let task = result.task.expect("should select a task");
        assert_eq!(task.task.id, "P1-HIGH-OVERLAP");
        assert!(task.score_breakdown.file_overlap_count > 0);
    }

    // ---------------------------------------------------------------------------
    // Dependency: cross-PRD deps are ignored when prefix is set
    // ---------------------------------------------------------------------------

    /// P1-US-004 depends on P1-US-005 (todo, unmet) AND P2-US-002 (done, out of scope).
    /// When prefix=P1, only intra-P1 deps are checked: P1-US-005 is unmet → P1-US-004 blocked.
    #[test]
    fn test_cross_prd_dep_ignored_for_prefix_session() {
        let (_tmp, conn) = setup_mixed_prd_db();

        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        let task = result.task.expect("should have eligible P1 tasks");
        assert_ne!(
            task.task.id, "P1-US-004",
            "P1-US-004 must be blocked by its P1 dep on P1-US-005"
        );
    }

    /// Without prefix (None), P2-US-001 wins (priority=1).
    #[test]
    fn test_no_prefix_all_tasks_eligible() {
        let (_tmp, conn) = setup_mixed_prd_db();

        let result = select_next_task(&conn, &[], None).unwrap();
        let task = result.task.expect("should select a task");
        assert_eq!(task.task.id, "P2-US-001");
    }

    // ---------------------------------------------------------------------------
    // Synergy: intra-PRD synergy works; cross-PRD synergy is not loaded
    // ---------------------------------------------------------------------------

    /// P1-US-001 synergyWith P1-US-003 — synergy is no longer scored but P1-US-001 still wins by priority.
    #[test]
    fn test_p1_task_selection_ignores_synergy_scoring() {
        let (_tmp, conn) = setup_mixed_prd_db();

        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        let task = result.task.expect("should select a task");

        // P1-US-001 and P1-US-004 both have priority=10; P1-US-001 wins by stable sort
        // (P1-US-004 is blocked by P1-US-005 dep)
        assert_eq!(task.task.id, "P1-US-001");
        assert_eq!(task.score_breakdown.file_score, 0);
    }

    /// In a P2 session, P2-US-003 has synergyWith P1-US-001.
    /// But the synergy relationships loaded are scoped to P2- task_ids, and the
    /// P2-US-001 (priority=1) wins by priority regardless.
    #[test]
    fn test_cross_prd_synergy_not_applied_in_p2_session() {
        let (_tmp, conn) = setup_mixed_prd_db();

        let result = select_next_task(&conn, &[], Some("P2")).unwrap();
        let task = result.task.expect("should select a P2 task");

        assert!(task.task.id.starts_with("P2-"), "should select a P2 task");
        // P2-US-001 wins by priority=1
        assert_eq!(task.task.id, "P2-US-001");
    }

    // ---------------------------------------------------------------------------
    // Batch: cross-PRD batchWith targets are excluded
    // ---------------------------------------------------------------------------

    /// batchWith relationships are no longer tracked in SelectionResult.
    #[test]
    fn test_cross_prd_batch_not_in_selection_result() {
        let (_tmp, conn) = setup_mixed_prd_db();

        let result = select_next_task(&conn, &[], Some("P1")).unwrap();
        // batch_tasks field removed; simply verify a P1 task is selected
        assert!(result.task.is_some());
        assert!(result.task.unwrap().task.id.starts_with("P1-"));
    }

    // ---------------------------------------------------------------------------
    // Decay: only P1-prefixed tasks are decayed in a P1 session
    // ---------------------------------------------------------------------------

    #[test]
    fn test_decay_only_affects_p1_tasks() {
        let (_tmp, conn) = setup_mixed_prd_db();

        insert_prd_task(&conn, "P2-US-BLOCKED", "blocked", 99);

        conn.execute(
            "UPDATE tasks SET blocked_at_iteration = 0 WHERE id IN ('P1-US-006', 'P2-US-BLOCKED')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET skipped_at_iteration = 0 WHERE id = 'P1-US-007'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE global_state SET iteration_counter = 10 WHERE id = 1",
            [],
        )
        .unwrap();

        let decayed = apply_decay(&conn, 5, false, Some("P1")).unwrap();
        let decayed_ids: Vec<&str> = decayed.iter().map(|(id, _)| id.as_str()).collect();

        assert!(decayed_ids.contains(&"P1-US-006"), "P1-US-006 should decay");
        assert!(decayed_ids.contains(&"P1-US-007"), "P1-US-007 should decay");
        assert!(
            !decayed_ids.contains(&"P2-US-BLOCKED"),
            "P2 task must not be decayed in a P1 session; decayed: {decayed_ids:?}"
        );

        let p1_status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'P1-US-006'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(p1_status, "todo");

        let p2_status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'P2-US-BLOCKED'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(p2_status, "blocked", "P2 task must remain blocked");
    }

    #[test]
    fn test_decay_warnings_scoped_to_prefix() {
        let (_tmp, conn) = setup_mixed_prd_db();

        insert_prd_task(&conn, "P2-US-WARN", "blocked", 99);

        conn.execute(
            "UPDATE tasks SET blocked_at_iteration = 0 WHERE id IN ('P1-US-006', 'P2-US-WARN')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE global_state SET iteration_counter = 8 WHERE id = 1",
            [],
        )
        .unwrap();

        // threshold=10, warning=5 → warning zone is iterations 5..10 since blocking
        // Both tasks blocked at 0, current=8 → 8 iterations since → in warning zone
        let warnings = find_decay_warnings(&conn, 10, 5, Some("P1")).unwrap();
        let warned_ids: Vec<&str> = warnings.iter().map(|w| w.task_id.as_str()).collect();

        assert!(
            warned_ids.contains(&"P1-US-006"),
            "P1-US-006 should appear in warnings"
        );
        assert!(
            !warned_ids.contains(&"P2-US-WARN"),
            "P2 task must not appear in P1 warnings"
        );
    }

    // ---------------------------------------------------------------------------
    // Backwards compatibility
    // ---------------------------------------------------------------------------

    /// Single-PRD DB with no prefix: behavior identical to prefix set to that PRD.
    #[test]
    fn test_single_prd_no_prefix_identical_to_prefix() {
        let (_tmp, conn) = setup_test_db();

        insert_prd_task(&conn, "P1-US-010", "todo", 10);
        insert_prd_task(&conn, "P1-US-020", "todo", 20);
        insert_prd_task(&conn, "P1-US-030", "done", 30);
        insert_test_relationship(&conn, "P1-US-020", "P1-US-030", "dependsOn");

        let result_no_prefix = select_next_task(&conn, &[], None).unwrap();
        let result_with_prefix = select_next_task(&conn, &[], Some("P1")).unwrap();

        assert_eq!(
            result_no_prefix.task.unwrap().task.id,
            result_with_prefix.task.unwrap().task.id,
            "single-PRD DB: None and Some(prefix) must select the same task"
        );
    }

    /// Eligible count is scoped to the prefix; P1 + P2 counts = all-tasks count.
    #[test]
    fn test_eligible_count_respects_prefix() {
        let (_tmp, conn) = setup_mixed_prd_db();

        let count_p1 = select_next_task(&conn, &[], Some("P1"))
            .unwrap()
            .eligible_count;
        let count_p2 = select_next_task(&conn, &[], Some("P2"))
            .unwrap()
            .eligible_count;
        let count_all = select_next_task(&conn, &[], None).unwrap().eligible_count;

        assert!(count_p1 < count_all, "P1 scope < all-scope");
        assert!(count_p2 < count_all, "P2 scope < all-scope");
        assert_eq!(
            count_p1 + count_p2,
            count_all,
            "P1 + P2 eligible counts must equal all-tasks count"
        );
    }
}
