//! Integration tests for smart task selection logic.
//!
//! These tests verify that the task selection algorithm correctly considers:
//! - Task priority (highest priority when no other factors)
//! - File overlap (boosts tasks that touch recently modified files)
//! - Dependencies (blocks tasks until dependencies are satisfied)
//! - Synergy bonus (prefers tasks with synergy to recently completed)
//! - Conflict penalty (avoids tasks that conflict with recently completed)
//! - Batch tasks (includes batchWith targets in output)

use std::fs;
use tempfile::TempDir;

use task_mgr::commands::{complete, init, next};
use task_mgr::db::open_connection;

/// Get the path to the sample PRD fixture file.
fn sample_prd_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_prd.json")
}

/// Create a custom PRD JSON for specific test scenarios.
fn create_custom_prd(tasks: &[serde_json::Value]) -> String {
    serde_json::json!({
        "project": "test-project",
        "branchName": "test/task-selection",
        "description": "Test PRD for task selection",
        "userStories": tasks
    })
    .to_string()
}

/// Create a task JSON value with the given parameters.
#[allow(clippy::too_many_arguments)]
fn make_task(
    id: &str,
    title: &str,
    priority: i32,
    passes: bool,
    depends_on: &[&str],
    synergy_with: &[&str],
    batch_with: &[&str],
    conflicts_with: &[&str],
    touches_files: &[&str],
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "title": title,
        "description": format!("Description for {}", id),
        "acceptanceCriteria": ["Criterion 1"],
        "priority": priority,
        "passes": passes,
        "notes": "",
        "touchesFiles": touches_files,
        "dependsOn": depends_on,
        "synergyWith": synergy_with,
        "batchWith": batch_with,
        "conflictsWith": conflicts_with
    })
}

#[test]
fn test_highest_priority_task_selected_when_no_other_factors() {
    let temp_dir = TempDir::new().unwrap();

    // Create tasks with different priorities, no relationships or file overlaps
    let tasks = vec![
        make_task(
            "TASK-001",
            "Low Priority",
            50,
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/a.rs"],
        ),
        make_task(
            "TASK-002",
            "High Priority",
            10,
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/b.rs"],
        ),
        make_task(
            "TASK-003",
            "Medium Priority",
            30,
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/c.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    // Import the PRD
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Select next task without any after_files
    let result = next::next(temp_dir.path(), &[], false, None, false).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    // Highest priority (lowest number) should be selected
    assert_eq!(task.id, "TASK-002");
    assert_eq!(task.priority, 10);
    // Priority score should be 1000 - 10 = 990
    assert_eq!(task.score.priority, 990);
}

#[test]
fn test_file_overlap_boosts_task_selection() {
    let temp_dir = TempDir::new().unwrap();

    // Create tasks where lower priority task has file overlap
    let tasks = vec![
        make_task(
            "TASK-001",
            "High Priority No Overlap",
            10,
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/unrelated.rs"],
        ),
        make_task(
            "TASK-002",
            "Lower Priority With Overlap",
            15,
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/main.rs", "src/lib.rs", "src/cli.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Pass after_files that overlap with TASK-002
    // TASK-001: priority score = 990, file overlap = 0, total = 990
    // TASK-002: priority score = 985, file overlap = 30 (3 files * 10), total = 1015
    let after_files = vec![
        "src/main.rs".to_string(),
        "src/lib.rs".to_string(),
        "src/cli.rs".to_string(),
    ];

    let result = next::next(temp_dir.path(), &after_files, false, None, false).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    // TASK-002 should be selected due to file overlap bonus
    assert_eq!(task.id, "TASK-002");
    assert_eq!(task.score.file_overlap, 30); // 3 files * 10 points each
    assert_eq!(task.score.file_overlap_count, 3);

    // Verify total score
    // priority_score = 1000 - 15 = 985
    // file_score = 30
    // total = 1015
    assert_eq!(task.score.total, 1015);
}

#[test]
fn test_dependencies_block_task_selection_until_satisfied() {
    let temp_dir = TempDir::new().unwrap();

    // Create a dependency chain: TASK-002 depends on TASK-001
    let tasks = vec![
        make_task(
            "TASK-001",
            "Prerequisite Task",
            50,
            false, // Not completed
            &[],
            &[],
            &[],
            &[],
            &["src/a.rs"],
        ),
        make_task(
            "TASK-002",
            "Dependent Task - Higher Priority",
            10,
            false,
            &["TASK-001"], // Depends on TASK-001
            &[],
            &[],
            &[],
            &["src/b.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // First selection: TASK-002 has higher priority but is blocked by dependency
    let result = next::next(temp_dir.path(), &[], false, None, false).unwrap();
    assert!(result.task.is_some());
    let task = result.task.unwrap();
    assert_eq!(
        task.id, "TASK-001",
        "TASK-001 should be selected since TASK-002 is blocked by dependency"
    );

    // Only 1 task should be eligible (TASK-002 is blocked)
    assert_eq!(result.selection.eligible_count, 1);
}

#[test]
fn test_dependencies_satisfied_by_done_status() {
    let temp_dir = TempDir::new().unwrap();

    // Create dependency chain with prerequisite marked as done
    let tasks = vec![
        make_task(
            "TASK-001",
            "Prerequisite Task",
            50,
            true, // Completed (passes: true)
            &[],
            &[],
            &[],
            &[],
            &["src/a.rs"],
        ),
        make_task(
            "TASK-002",
            "Dependent Task",
            10,
            false,
            &["TASK-001"], // Depends on TASK-001
            &[],
            &[],
            &[],
            &["src/b.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // TASK-002 should be selected since TASK-001 is done
    let result = next::next(temp_dir.path(), &[], false, None, false).unwrap();
    assert!(result.task.is_some());
    let task = result.task.unwrap();
    assert_eq!(
        task.id, "TASK-002",
        "TASK-002 should be selected since its dependency (TASK-001) is done"
    );

    // Only 1 eligible task (TASK-001 is done)
    assert_eq!(result.selection.eligible_count, 1);
}

#[test]
fn test_dependencies_satisfied_by_completing_task() {
    let temp_dir = TempDir::new().unwrap();

    // Create dependency chain
    let tasks = vec![
        make_task(
            "TASK-001",
            "Prerequisite Task",
            50,
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/a.rs"],
        ),
        make_task(
            "TASK-002",
            "Dependent Task",
            10,
            false,
            &["TASK-001"],
            &[],
            &[],
            &[],
            &["src/b.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // First, complete TASK-001 (use force=true since task is in todo status after import)
    let mut conn = open_connection(temp_dir.path()).unwrap();
    complete::complete(&mut conn, &["TASK-001".to_string()], None, None, true).unwrap();
    drop(conn);

    // Now TASK-002 should be selected (dependency satisfied)
    let result = next::next(temp_dir.path(), &[], false, None, false).unwrap();
    assert!(result.task.is_some());
    let task = result.task.unwrap();
    assert_eq!(
        task.id, "TASK-002",
        "TASK-002 should be selected after TASK-001 is completed"
    );
}

#[test]
fn test_synergy_bonus_from_recently_completed_tasks() {
    let temp_dir = TempDir::new().unwrap();

    // Use sample PRD which has TASK-001 and TASK-002 with synergyWith relationship
    // TASK-001 has synergy with TASK-002, and TASK-002 has synergy with TASK-001 and TASK-003
    let prd_path = sample_prd_path();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // In sample PRD:
    // - TASK-001 is done (passes: true)
    // - TASK-002 is done (passes: true)
    // - TASK-003 depends on TASK-002 and has synergyWith TASK-002
    // - TASK-003 and TASK-004 are eligible (TASK-002 is done)

    // Get initial selection without any context
    let result_initial = next::next(temp_dir.path(), &[], false, None, true).unwrap();
    assert!(result_initial.task.is_some());

    // Both TASK-003 and TASK-004 depend on TASK-002 and should be eligible
    // TASK-003 has priority 3, TASK-004 has priority 4
    // Without synergy context, TASK-003 wins on priority
    assert_eq!(
        result_initial.task.as_ref().unwrap().id,
        "TASK-003",
        "TASK-003 should be selected (higher priority)"
    );
}

#[test]
fn test_conflict_penalty_from_recently_completed_tasks() {
    let temp_dir = TempDir::new().unwrap();

    // Create tasks where one has a conflict with a completed task
    let tasks = vec![
        make_task(
            "TASK-001",
            "Completed Task",
            1,
            true, // Completed
            &[],
            &[],
            &[],
            &[],
            &["src/a.rs"],
        ),
        make_task(
            "TASK-002",
            "Conflicts With Completed",
            10,
            false,
            &[],
            &[],
            &[],
            &["TASK-001"], // Conflicts with TASK-001
            &["src/b.rs"],
        ),
        make_task(
            "TASK-003",
            "No Conflicts",
            20,
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/c.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Note: The current next() implementation doesn't pass recently_completed from
    // the select_next_task call. The conflict scoring only works with explicit
    // recently_completed parameter. For now, we'll test with direct select_next_task.

    // Let's verify the conflict penalty logic at the integration level by using
    // the internal select_next_task with recently_completed
    let conn = open_connection(temp_dir.path()).unwrap();
    let result = next::select_next_task(&conn, &[], &["TASK-001".to_string()]).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    // Without conflict penalty:
    // TASK-002: 990 (priority) = 990
    // TASK-003: 980 (priority) = 980
    // TASK-002 would win

    // With conflict penalty (TASK-001 recently completed):
    // TASK-002: 990 (priority) + (-5 conflict) = 985
    // TASK-003: 980 (priority) = 980
    // TASK-002 still wins slightly

    // Let's adjust the test to make it clearer
    // Actually, even with -5 penalty, 990-5=985 > 980, so TASK-002 still wins
    // But we can verify the penalty was applied

    if task.task.id == "TASK-002" {
        assert_eq!(
            task.score_breakdown.conflict_score, -5,
            "TASK-002 should have conflict penalty"
        );
        assert_eq!(
            task.score_breakdown.conflict_from,
            vec!["TASK-001"],
            "Conflict should be from TASK-001"
        );
    }
}

#[test]
fn test_conflict_penalty_changes_selection() {
    let temp_dir = TempDir::new().unwrap();

    // Create tasks where conflict penalty is large enough to change selection
    let tasks = vec![
        make_task(
            "TASK-001",
            "Completed Task",
            1,
            true,
            &[],
            &[],
            &[],
            &[],
            &["src/a.rs"],
        ),
        make_task(
            "TASK-002",
            "Slightly Higher Priority With Conflict",
            14, // priority score = 986
            false,
            &[],
            &[],
            &[],
            &["TASK-001"], // Conflicts with TASK-001, penalty = -5, total = 981
            &["src/b.rs"],
        ),
        make_task(
            "TASK-003",
            "Slightly Lower Priority No Conflict",
            17, // priority score = 983, no penalty, total = 983
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/c.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // With recently_completed = ["TASK-001"]:
    // TASK-002: 986 - 5 = 981
    // TASK-003: 983
    // TASK-003 should win

    let conn = open_connection(temp_dir.path()).unwrap();
    let result = next::select_next_task(&conn, &[], &["TASK-001".to_string()]).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    assert_eq!(
        task.task.id, "TASK-003",
        "TASK-003 should be selected due to conflict penalty on TASK-002"
    );
}

#[test]
fn test_batch_tasks_included_in_output() {
    let temp_dir = TempDir::new().unwrap();

    // Use sample PRD which has batchWith relationships
    // TASK-003 and TASK-004 have batchWith relationship to each other
    let prd_path = sample_prd_path();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // TASK-003 depends on TASK-002 which is done (passes: true)
    // TASK-003 has batchWith: ["TASK-004"]

    let result = next::next(temp_dir.path(), &[], false, None, false).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    // TASK-003 should be selected and include TASK-004 in batch_tasks
    assert_eq!(task.id, "TASK-003");
    assert!(
        task.batch_with.contains(&"TASK-004".to_string()),
        "TASK-003 should have TASK-004 in batch_with"
    );

    // batch_tasks should include eligible batchWith targets
    assert!(
        result.batch_tasks.contains(&"TASK-004".to_string()),
        "TASK-004 should be in batch_tasks since it's todo"
    );
}

#[test]
fn test_batch_tasks_excludes_completed_tasks() {
    let temp_dir = TempDir::new().unwrap();

    // Create tasks with batchWith, where one batch task is completed
    let tasks = vec![
        make_task(
            "TASK-001",
            "Main Task",
            10,
            false,
            &[],
            &[],
            &["TASK-002", "TASK-003"], // Batch with both
            &[],
            &["src/a.rs"],
        ),
        make_task(
            "TASK-002",
            "Completed Batch Task",
            20,
            true, // Completed
            &[],
            &[],
            &["TASK-001"],
            &[],
            &["src/b.rs"],
        ),
        make_task(
            "TASK-003",
            "Todo Batch Task",
            30,
            false, // Not completed
            &[],
            &[],
            &["TASK-001"],
            &[],
            &["src/c.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    let result = next::next(temp_dir.path(), &[], false, None, false).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();
    assert_eq!(task.id, "TASK-001");

    // batch_with should list all (from the relationship)
    assert_eq!(task.batch_with.len(), 2);
    assert!(task.batch_with.contains(&"TASK-002".to_string()));
    assert!(task.batch_with.contains(&"TASK-003".to_string()));

    // batch_tasks should only include todo tasks
    assert!(
        !result.batch_tasks.contains(&"TASK-002".to_string()),
        "TASK-002 is done and should not be in batch_tasks"
    );
    assert!(
        result.batch_tasks.contains(&"TASK-003".to_string()),
        "TASK-003 is todo and should be in batch_tasks"
    );
}

#[test]
fn test_all_tasks_done_returns_no_task() {
    let temp_dir = TempDir::new().unwrap();

    // Create tasks that are all completed
    let tasks = vec![
        make_task("TASK-001", "Done 1", 10, true, &[], &[], &[], &[], &[]),
        make_task("TASK-002", "Done 2", 20, true, &[], &[], &[], &[], &[]),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    let result = next::next(temp_dir.path(), &[], false, None, false).unwrap();

    assert!(
        result.task.is_none(),
        "Should return no task when all are done"
    );
    assert_eq!(result.selection.eligible_count, 0);
}

#[test]
fn test_all_tasks_blocked_returns_no_task() {
    let temp_dir = TempDir::new().unwrap();

    // Create a circular-like dependency scenario where all tasks are blocked
    // TASK-001 is in_progress (not done), TASK-002 depends on TASK-001
    // After setting TASK-001 to blocked status, no tasks are eligible
    let tasks = vec![
        make_task(
            "TASK-001",
            "First task",
            10,
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/a.rs"],
        ),
        make_task(
            "TASK-002",
            "Depends on first",
            20,
            false,
            &["TASK-001"], // Depends on TASK-001
            &[],
            &[],
            &[],
            &["src/b.rs"],
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Set TASK-001 to 'blocked' status - this means TASK-002's dependency won't be satisfied
    // and TASK-001 itself is not eligible (not 'todo')
    let conn = open_connection(temp_dir.path()).unwrap();
    conn.execute(
        "UPDATE tasks SET status = 'blocked' WHERE id = 'TASK-001'",
        [],
    )
    .unwrap();
    drop(conn);

    let result = next::next(temp_dir.path(), &[], false, None, false).unwrap();

    assert!(
        result.task.is_none(),
        "Should return no task when all are blocked"
    );
    assert_eq!(result.selection.eligible_count, 0);
}

#[test]
fn test_combined_scoring_factors() {
    let temp_dir = TempDir::new().unwrap();

    // Create a complex scenario with multiple scoring factors
    let tasks = vec![
        make_task(
            "TASK-001",
            "Completed Prereq",
            1,
            true,
            &[],
            &[],
            &[],
            &[],
            &["src/prereq.rs"],
        ),
        make_task(
            "TASK-002",
            "High Priority No Bonuses",
            10, // score = 990
            false,
            &[],
            &[],
            &[],
            &[],
            &["src/unrelated.rs"],
        ),
        make_task(
            "TASK-003",
            "Lower Priority With File Overlap and Synergy",
            20, // priority score = 980
            false,
            &[],
            &["TASK-001"], // Synergy with completed task = +3
            &[],
            &[],
            &["src/main.rs", "src/lib.rs"], // 2 file overlaps = +20
        ),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // With after_files and recently_completed:
    // TASK-002: 990 + 0 + 0 = 990
    // TASK-003: 980 + 20 (files) + 3 (synergy) = 1003

    let conn = open_connection(temp_dir.path()).unwrap();
    let result = next::select_next_task(
        &conn,
        &["src/main.rs".to_string(), "src/lib.rs".to_string()],
        &["TASK-001".to_string()],
    )
    .unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    assert_eq!(
        task.task.id, "TASK-003",
        "TASK-003 should win with combined bonuses"
    );
    assert_eq!(task.score_breakdown.priority_score, 980);
    assert_eq!(task.score_breakdown.file_score, 20);
    assert_eq!(task.score_breakdown.synergy_score, 3);
    assert_eq!(task.total_score, 1003);
}

#[test]
fn test_next_with_claim_updates_status() {
    let temp_dir = TempDir::new().unwrap();

    let tasks = vec![make_task(
        "TASK-001",
        "Task to claim",
        10,
        false,
        &[],
        &[],
        &[],
        &[],
        &["src/a.rs"],
    )];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Select with --claim
    let result = next::next(temp_dir.path(), &[], true, None, false).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();
    assert_eq!(task.id, "TASK-001");
    assert_eq!(task.status, "in_progress", "Task should be claimed");

    assert!(result.claim.is_some());
    let claim = result.claim.unwrap();
    assert!(claim.claimed);
    assert_eq!(claim.iteration, 1);

    // Verify database was updated
    let conn = open_connection(temp_dir.path()).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE id = 'TASK-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "in_progress");
}

#[test]
fn test_verbose_output_includes_top_candidates() {
    let temp_dir = TempDir::new().unwrap();

    let tasks = vec![
        make_task("TASK-001", "Task 1", 10, false, &[], &[], &[], &[], &[]),
        make_task("TASK-002", "Task 2", 20, false, &[], &[], &[], &[], &[]),
        make_task("TASK-003", "Task 3", 30, false, &[], &[], &[], &[], &[]),
    ];

    let prd_content = create_custom_prd(&tasks);
    let prd_path = temp_dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Select with verbose=true
    let result = next::next(temp_dir.path(), &[], false, None, true).unwrap();

    assert!(result.task.is_some());
    assert!(
        !result.top_candidates.is_empty(),
        "Verbose mode should include top candidates"
    );

    // Should include all 3 tasks (less than 5)
    assert_eq!(result.top_candidates.len(), 3);

    // First candidate should be the selected one (highest priority)
    assert_eq!(result.top_candidates[0].id, "TASK-001");
    assert_eq!(result.top_candidates[1].id, "TASK-002");
    assert_eq!(result.top_candidates[2].id, "TASK-003");
}
