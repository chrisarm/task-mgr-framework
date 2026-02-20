//! Tests for the next command module.

#[cfg(test)]
mod selection_tests {
    use crate::commands::next::selection::{
        format_text, select_next_task, ScoreBreakdown, ScoredTask, SelectionResult,
    };
    use crate::db::{create_schema, open_connection};
    use crate::models::Task;
    use rusqlite::{params, Connection};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_test_task(conn: &Connection, id: &str, title: &str, status: &str, priority: i32) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
            params![id, title, status, priority],
        )
        .unwrap();
    }

    fn insert_test_task_file(conn: &Connection, task_id: &str, file_path: &str) {
        conn.execute(
            "INSERT INTO task_files (task_id, file_path) VALUES (?, ?)",
            params![task_id, file_path],
        )
        .unwrap();
    }

    fn insert_test_relationship(
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

    #[test]
    fn test_select_no_tasks() {
        let (_temp_dir, conn) = setup_test_db();

        let result = select_next_task(&conn, &[], &[]).unwrap();
        assert!(result.task.is_none());
        assert_eq!(result.eligible_count, 0);
    }

    #[test]
    fn test_select_single_todo_task() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 10);

        let result = select_next_task(&conn, &[], &[]).unwrap();
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

        let result = select_next_task(&conn, &[], &[]).unwrap();
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

        let result = select_next_task(&conn, &[], &[]).unwrap();
        assert!(result.task.is_some());
        assert_eq!(result.task.unwrap().task.id, "US-002");
        assert_eq!(result.eligible_count, 1);
    }

    #[test]
    fn test_select_ignores_blocked_tasks() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Blocked Task", "blocked", 1);
        insert_test_task(&conn, "US-002", "Todo Task", "todo", 50);

        let result = select_next_task(&conn, &[], &[]).unwrap();
        assert!(result.task.is_some());
        assert_eq!(result.task.unwrap().task.id, "US-002");
    }

    #[test]
    fn test_dependencies_block_selection() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Prereq Task", "todo", 50);
        insert_test_task(&conn, "US-002", "Dependent Task", "todo", 10);
        insert_test_relationship(&conn, "US-002", "US-001", "dependsOn");

        let result = select_next_task(&conn, &[], &[]).unwrap();
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

        let result = select_next_task(&conn, &[], &[]).unwrap();
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

        let result = select_next_task(&conn, &[], &[]).unwrap();
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
        let result = select_next_task(&conn, &after_files, &[]).unwrap();

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
        let result = select_next_task(&conn, &after_files, &[]).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();
        assert_eq!(task.score_breakdown.file_score, 20); // 2 overlaps * 10
        assert_eq!(task.score_breakdown.file_overlap_count, 2);
    }

    #[test]
    fn test_synergy_scoring() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Completed Task", "done", 1);
        insert_test_task(&conn, "US-002", "Task A", "todo", 50);
        insert_test_task(&conn, "US-003", "Task B", "todo", 50);
        insert_test_relationship(&conn, "US-002", "US-001", "synergyWith");

        // Pass US-001 as recently completed
        let recently_completed = vec!["US-001".to_string()];
        let result = select_next_task(&conn, &[], &recently_completed).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();
        // US-002 should be selected due to synergy bonus
        assert_eq!(task.task.id, "US-002");
        assert_eq!(task.score_breakdown.synergy_score, 3);
        assert_eq!(task.score_breakdown.synergy_from, vec!["US-001"]);
    }

    #[test]
    fn test_conflict_scoring() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Completed Task", "done", 1);
        insert_test_task(&conn, "US-002", "Task A", "todo", 50);
        insert_test_task(&conn, "US-003", "Task B", "todo", 50);
        insert_test_relationship(&conn, "US-002", "US-001", "conflictsWith");

        // Pass US-001 as recently completed
        let recently_completed = vec!["US-001".to_string()];
        let result = select_next_task(&conn, &[], &recently_completed).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();
        // US-003 should be selected due to conflict penalty on US-002
        assert_eq!(task.task.id, "US-003");

        // Verify US-002 would have had conflict penalty if selected
        // (we can check by changing priorities to force US-002 selection)
    }

    #[test]
    fn test_conflict_penalty_calculation() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Completed Task", "done", 1);
        insert_test_task(&conn, "US-002", "Conflicting Task", "todo", 10); // Higher priority
        insert_test_relationship(&conn, "US-002", "US-001", "conflictsWith");

        let recently_completed = vec!["US-001".to_string()];
        let result = select_next_task(&conn, &[], &recently_completed).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();
        assert_eq!(task.task.id, "US-002"); // Still selected (only one option)
        assert_eq!(task.score_breakdown.conflict_score, -5);
        assert_eq!(task.score_breakdown.conflict_from, vec!["US-001"]);
    }

    #[test]
    fn test_batch_tasks_identified() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Main Task", "todo", 10);
        insert_test_task(&conn, "FIX-001", "Batch Task", "todo", 50);
        insert_test_relationship(&conn, "US-001", "FIX-001", "batchWith");

        let result = select_next_task(&conn, &[], &[]).unwrap();
        assert!(result.task.is_some());
        let task = result.task.unwrap();
        assert_eq!(task.task.id, "US-001");
        assert_eq!(task.batch_with, vec!["FIX-001"]);
        assert_eq!(result.batch_tasks, vec!["FIX-001"]);
    }

    #[test]
    fn test_batch_tasks_excludes_done() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Main Task", "todo", 10);
        insert_test_task(&conn, "FIX-001", "Completed Batch Task", "done", 50);
        insert_test_relationship(&conn, "US-001", "FIX-001", "batchWith");

        let result = select_next_task(&conn, &[], &[]).unwrap();
        assert!(result.task.is_some());
        let task = result.task.unwrap();
        assert_eq!(task.batch_with, vec!["FIX-001"]);
        // batch_tasks should be empty since FIX-001 is done
        assert!(result.batch_tasks.is_empty());
    }

    #[test]
    fn test_combined_scoring() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Completed Prereq", "done", 1);
        insert_test_task(&conn, "US-002", "Low Priority", "todo", 50);
        insert_test_task(&conn, "US-003", "High Priority No Overlap", "todo", 20);
        insert_test_task_file(&conn, "US-002", "src/main.rs");
        insert_test_relationship(&conn, "US-002", "US-001", "synergyWith");

        // File overlap + synergy should overcome priority difference
        let after_files = vec!["src/main.rs".to_string()];
        let recently_completed = vec!["US-001".to_string()];
        let result = select_next_task(&conn, &after_files, &recently_completed).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();

        // US-002: 950 (priority) + 10 (file) + 3 (synergy) = 963
        // US-003: 980 (priority) + 0 (file) + 0 (synergy) = 980
        // US-003 should still win with higher priority
        assert_eq!(task.task.id, "US-003");
    }

    #[test]
    fn test_combined_scoring_file_wins() {
        let (_temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Completed Prereq", "done", 1);
        insert_test_task(&conn, "US-002", "Lower Priority Many Files", "todo", 45);
        insert_test_task(&conn, "US-003", "Higher Priority No Overlap", "todo", 40);
        insert_test_task_file(&conn, "US-002", "src/main.rs");
        insert_test_task_file(&conn, "US-002", "src/lib.rs");
        insert_test_task_file(&conn, "US-002", "src/cli.rs");
        insert_test_relationship(&conn, "US-002", "US-001", "synergyWith");

        // 3 file overlaps + synergy should overcome priority difference
        let after_files = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "src/cli.rs".to_string(),
        ];
        let recently_completed = vec!["US-001".to_string()];
        let result = select_next_task(&conn, &after_files, &recently_completed).unwrap();

        assert!(result.task.is_some());
        let task = result.task.unwrap();

        // US-002: 955 (priority) + 30 (file) + 3 (synergy) = 988
        // US-003: 960 (priority) + 0 (file) + 0 (synergy) = 960
        // US-002 should win
        assert_eq!(task.task.id, "US-002");
    }

    #[test]
    fn test_format_text_with_task() {
        let result = SelectionResult {
            task: Some(ScoredTask {
                task: Task::new("US-001", "Test Task"),
                files: vec!["src/main.rs".to_string()],
                batch_with: vec!["FIX-001".to_string()],
                total_score: 963,
                score_breakdown: ScoreBreakdown {
                    priority_score: 950,
                    file_score: 10,
                    synergy_score: 3,
                    conflict_score: 0,
                    file_overlap_count: 1,
                    synergy_from: vec!["US-000".to_string()],
                    conflict_from: vec![],
                },
            }),
            batch_tasks: vec!["FIX-001".to_string()],
            selection_reason: "Selected task US-001".to_string(),
            eligible_count: 5,
            top_candidates: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("Next Task: US-001"));
        assert!(text.contains("Score:    963"));
        assert!(text.contains("Priority:    +950"));
        assert!(text.contains("File Overlap: +10"));
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("Batch With:"));
        assert!(text.contains("FIX-001"));
    }

    #[test]
    fn test_format_text_no_task() {
        let result = SelectionResult {
            task: None,
            batch_tasks: vec![],
            selection_reason: "All tasks completed".to_string(),
            eligible_count: 0,
            top_candidates: vec![],
        };

        let text = format_text(&result);
        assert!(text.contains("No tasks available"));
        assert!(text.contains("All tasks completed"));
    }
}

#[cfg(test)]
mod next_command_tests {
    use crate::commands::next::next;
    use crate::commands::next::output::{
        format_next_text, format_next_verbose, CandidateSummary, ClaimMetadata,
        LearningSummaryOutput, NextResult, NextTaskOutput, ScoreOutput, SelectionMetadata,
    };
    use crate::db::{create_schema, open_connection};
    use rusqlite::{params, Connection};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_test_task(conn: &Connection, id: &str, title: &str, status: &str, priority: i32) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
            params![id, title, status, priority],
        )
        .unwrap();
    }

    fn insert_test_relationship(
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

    #[test]
    fn test_next_no_tasks() {
        let (temp_dir, conn) = setup_test_db();
        drop(conn);

        let result = next(temp_dir.path(), &[], false, None, false).unwrap();
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

        let result = next(temp_dir.path(), &[], false, None, false).unwrap();
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

        let result = next(temp_dir.path(), &[], true, None, false).unwrap();
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
        let result1 = next(temp_dir.path(), &[], true, None, false).unwrap();
        assert_eq!(result1.claim.unwrap().iteration, 1);

        // Complete first task so US-002 is next
        let conn = open_connection(temp_dir.path()).unwrap();
        conn.execute("UPDATE tasks SET status = 'done' WHERE id = 'US-001'", [])
            .unwrap();
        drop(conn);

        // Second claim
        let result2 = next(temp_dir.path(), &[], true, None, false).unwrap();
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

        let result = next(temp_dir.path(), &[], true, Some("test-run-123"), false).unwrap();
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
        let result = next(temp_dir.path(), &[], true, Some("nonexistent-run"), false);
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

        // Attempt to claim with inactive run
        let result = next(temp_dir.path(), &[], true, Some("completed-run"), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expected active"));
    }

    #[test]
    fn test_next_output_includes_score_breakdown() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "First Task", "todo", 20);
        drop(conn);

        let result = next(temp_dir.path(), &[], false, None, false).unwrap();
        let task = result.task.unwrap();
        assert_eq!(task.score.priority, 980); // 1000 - 20
        assert_eq!(task.score.file_overlap, 0);
        assert_eq!(task.score.synergy, 0);
        assert_eq!(task.score.conflict, 0);
        assert_eq!(task.score.total, 980);
    }

    #[test]
    fn test_next_includes_batch_tasks() {
        let (temp_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "Main Task", "todo", 10);
        insert_test_task(&conn, "FIX-001", "Batch Task", "todo", 20);
        insert_test_relationship(&conn, "US-001", "FIX-001", "batchWith");
        drop(conn);

        let result = next(temp_dir.path(), &[], false, None, false).unwrap();
        assert!(result.task.is_some());
        assert_eq!(result.batch_tasks, vec!["FIX-001".to_string()]);
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
                batch_with: vec![],
                model: None,
                difficulty: None,
                escalation_note: None,
                score: ScoreOutput {
                    total: 990,
                    priority: 990,
                    file_overlap: 0,
                    synergy: 0,
                    conflict: 0,
                    file_overlap_count: 0,
                    synergy_from: vec![],
                    conflict_from: vec![],
                },
            }),
            batch_tasks: vec![],
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
                batch_with: vec![],
                model: None,
                difficulty: None,
                escalation_note: None,
                score: ScoreOutput {
                    total: 990,
                    priority: 990,
                    file_overlap: 0,
                    synergy: 0,
                    conflict: 0,
                    file_overlap_count: 0,
                    synergy_from: vec![],
                    conflict_from: vec![],
                },
            }),
            batch_tasks: vec![],
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
            batch_tasks: vec![],
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
        let result = next(temp_dir.path(), &[], false, None, true).unwrap();
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
        let result = next(temp_dir.path(), &[], false, None, false).unwrap();
        assert!(result.task.is_some());
        assert!(result.top_candidates.is_empty());
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
                batch_with: vec![],
                model: None,
                difficulty: None,
                escalation_note: None,
                score: ScoreOutput {
                    total: 990,
                    priority: 990,
                    file_overlap: 0,
                    synergy: 0,
                    conflict: 0,
                    file_overlap_count: 0,
                    synergy_from: vec![],
                    conflict_from: vec![],
                },
            }),
            batch_tasks: vec![],
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
                        synergy: 0,
                        conflict: 0,
                        file_overlap_count: 0,
                        synergy_from: vec![],
                        conflict_from: vec![],
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
                        synergy: 0,
                        conflict: 0,
                        file_overlap_count: 0,
                        synergy_from: vec![],
                        conflict_from: vec![],
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
    use crate::commands::next::decay::{apply_decay, find_decay_warnings};
    use crate::db::{create_schema, open_connection};
    use rusqlite::{params, Connection};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_test_task(conn: &Connection, id: &str, title: &str, status: &str, priority: i32) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
            params![id, title, status, priority],
        )
        .unwrap();
    }

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
        let decayed = apply_decay(&conn, 0, false).unwrap();
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
        let decayed = apply_decay(&conn, 32, false).unwrap();
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
        let decayed = apply_decay(&conn, 32, false).unwrap();
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

        let decayed = apply_decay(&conn, 32, false).unwrap();
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

        let decayed = apply_decay(&conn, 32, false).unwrap();
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

        apply_decay(&conn, 32, false).unwrap();

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

        let warnings = find_decay_warnings(&conn, 32, 8).unwrap();
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

        let warnings = find_decay_warnings(&conn, 32, 8).unwrap();
        // 40 - 30 = 10 iterations since blocked
        // Would decay at 30 + 32 = 62
        // Warning window is 62 - 8 = 54 to 62
        // Current is 40, which is before 54, so no warning
        assert!(warnings.is_empty());
    }
}
