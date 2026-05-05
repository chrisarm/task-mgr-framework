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

use task_mgr::commands::next::selection::select_parallel_group;
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
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Select next task without any after_files
    let result = next::next(temp_dir.path(), &[], false, None, false, None).unwrap();

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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Pass after_files that overlap with TASK-002
    // TASK-001: priority score = 990, file overlap = 0, total = 990
    // TASK-002: priority score = 985, file overlap = 30 (3 files * 10), total = 1015
    let after_files = vec![
        "src/main.rs".to_string(),
        "src/lib.rs".to_string(),
        "src/cli.rs".to_string(),
    ];

    let result = next::next(temp_dir.path(), &after_files, false, None, false, None).unwrap();

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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // First selection: TASK-002 has higher priority but is blocked by dependency
    let result = next::next(temp_dir.path(), &[], false, None, false, None).unwrap();
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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // TASK-002 should be selected since TASK-001 is done
    let result = next::next(temp_dir.path(), &[], false, None, false, None).unwrap();
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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // First, complete TASK-001 (use force=true since task is in todo status after import)
    let mut conn = open_connection(temp_dir.path()).unwrap();
    complete::complete(&mut conn, &["TASK-001".to_string()], None, None, true).unwrap();
    drop(conn);

    // Now TASK-002 should be selected (dependency satisfied)
    let result = next::next(temp_dir.path(), &[], false, None, false, None).unwrap();
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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // In sample PRD:
    // - TASK-001 is done (passes: true)
    // - TASK-002 is done (passes: true)
    // - TASK-003 depends on TASK-002 and has synergyWith TASK-002
    // - TASK-003 and TASK-004 are eligible (TASK-002 is done)

    // Get initial selection without any context
    let result_initial = next::next(temp_dir.path(), &[], false, None, true, None).unwrap();
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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // conflictsWith relationships are no longer scored; selection is by priority only.
    let conn = open_connection(temp_dir.path()).unwrap();
    let result = next::select_next_task(&conn, &[], None).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    // Without conflict scoring, TASK-002 wins purely on priority:
    // TASK-002: 990 (priority) = 990
    // TASK-003: 980 (priority) = 980
    assert_eq!(
        task.task.id, "TASK-002",
        "TASK-002 should win on priority (conflict scoring removed)"
    );
    assert_eq!(result.eligible_count, 2);
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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // conflictsWith relationships are no longer scored; TASK-002 wins on priority:
    // TASK-002: 986 (priority, no penalty)
    // TASK-003: 983 (priority)

    let conn = open_connection(temp_dir.path()).unwrap();
    let result = next::select_next_task(&conn, &[], None).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    assert_eq!(
        task.task.id, "TASK-002",
        "TASK-002 should win on priority (conflict scoring removed)"
    );
}

#[test]
fn test_batch_tasks_included_in_output() {
    let temp_dir = TempDir::new().unwrap();

    // Use sample PRD which has batchWith relationships
    // TASK-003 and TASK-004 have batchWith relationship to each other
    let prd_path = sample_prd_path();

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // TASK-003 depends on TASK-002 which is done (passes: true)
    // batchWith relationships are no longer tracked in selection output

    let result = next::next(temp_dir.path(), &[], false, None, false, None).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    // TASK-003 should be selected (higher priority among eligible)
    assert_eq!(task.id, "TASK-003");
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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // batchWith relationships are no longer tracked in selection output
    let result = next::next(temp_dir.path(), &[], false, None, false, None).unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();
    assert_eq!(task.id, "TASK-001");
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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    let result = next::next(temp_dir.path(), &[], false, None, false, None).unwrap();

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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Set TASK-001 to 'blocked' status - this means TASK-002's dependency won't be satisfied
    // and TASK-001 itself is not eligible (not 'todo')
    let conn = open_connection(temp_dir.path()).unwrap();
    conn.execute(
        "UPDATE tasks SET status = 'blocked' WHERE id = 'TASK-001'",
        [],
    )
    .unwrap();
    drop(conn);

    let result = next::next(temp_dir.path(), &[], false, None, false, None).unwrap();

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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Synergy scoring removed; with after_files only:
    // TASK-002: 990 (priority) + 0 (files) = 990
    // TASK-003: 980 (priority) + 20 (2 file overlaps * 10) = 1000

    let conn = open_connection(temp_dir.path()).unwrap();
    let result = next::select_next_task(
        &conn,
        &["src/main.rs".to_string(), "src/lib.rs".to_string()],
        None,
    )
    .unwrap();

    assert!(result.task.is_some());
    let task = result.task.unwrap();

    assert_eq!(
        task.task.id, "TASK-003",
        "TASK-003 should win with file overlap bonus"
    );
    assert_eq!(task.score_breakdown.priority_score, 980);
    assert_eq!(task.score_breakdown.file_score, 20);
    assert_eq!(task.total_score, 1000);
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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Select with --claim
    let result = next::next(temp_dir.path(), &[], true, None, false, None).unwrap();

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

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Select with verbose=true
    let result = next::next(temp_dir.path(), &[], false, None, true, None).unwrap();

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

/// Reproduce the cbd7d081-MILESTONE-FINAL production bug.
///
/// A parallel-slot wave dispatched the milestone while REFACTOR-N-001
/// (in_progress) and REFACTOR-N-002 (todo) were still active.  The fix
/// defers any milestone-class candidate whose acceptance criteria mention a
/// known spawned-fixup prefix while a same-prefix active sibling exists.
///
/// Acceptance criteria strings are taken verbatim from the real PRD that
/// triggered the bug (tasks/overflow-recovery-and-diagnostics.json).
#[test]
fn test_milestone_soft_dep_cbd7d081_scenario() {
    let temp_dir = TempDir::new().unwrap();

    // Real-world AC strings from the production bug.
    let milestone_ac = serde_json::json!([
        "All tasks (ANALYSIS, TEST-INIT, FEAT, CODE-REVIEW-1, MILESTONE-1, TEST-001/002, MILESTONE-2, INT-001, REFACTOR-REVIEW-FINAL, REFACTOR-N-xxx if any, VERIFY-001) have passes=true",
        "Any spawned CODE-FIX/WIRE-FIX/IMPL-FIX/REFACTOR-N tasks have passes=true"
    ]);

    let prd_content = serde_json::json!({
        "project": "cbd7d081-test",
        "branchName": "test/milestone-soft-dep",
        "description": "Reproduce cbd7d081 scenario",
        "userStories": [
            {
                "id": "cbd7d081-MILESTONE-FINAL",
                "title": "Final milestone",
                "description": "Milestone task that should be deferred while REFACTOR-N siblings are active",
                "acceptanceCriteria": milestone_ac,
                "priority": 100,
                "passes": false,
                "notes": "",
                "touchesFiles": ["src/milestone.rs"],
                "dependsOn": [],
                "synergyWith": [],
                "batchWith": [],
                "conflictsWith": []
            },
            {
                "id": "cbd7d081-REFACTOR-N-001",
                "title": "Spawned refactor fixup 1",
                "description": "Active sibling — will be set to in_progress",
                "acceptanceCriteria": ["Fixup complete"],
                "priority": 10,
                "passes": false,
                "notes": "",
                "touchesFiles": ["src/refactor_a.rs"],
                "dependsOn": [],
                "synergyWith": [],
                "batchWith": [],
                "conflictsWith": []
            },
            {
                "id": "cbd7d081-REFACTOR-N-002",
                "title": "Spawned refactor fixup 2",
                "description": "Active sibling — disjoint file so co-schedulable with REFACTOR-N-001",
                "acceptanceCriteria": ["Fixup complete"],
                "priority": 11,
                "passes": false,
                "notes": "",
                "touchesFiles": ["src/refactor_b.rs"],
                "dependsOn": [],
                "synergyWith": [],
                "batchWith": [],
                "conflictsWith": []
            }
        ]
    })
    .to_string();

    let prd_path = temp_dir.path().join("cbd7d081_prd.json");
    fs::write(&prd_path, prd_content).unwrap();

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Replicate the production state: REFACTOR-N-001 is already in_progress
    // (the wave claimed it in a prior iteration).
    let conn = open_connection(temp_dir.path()).unwrap();
    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = 'cbd7d081-REFACTOR-N-001'",
        [],
    )
    .unwrap();

    // Phase 1 — milestone must be excluded while active fixup siblings exist.
    let group =
        select_parallel_group(&conn, &[], Some("cbd7d081"), 2).expect("select_parallel_group");
    let ids: Vec<&str> = group.iter().map(|s| s.task.id.as_str()).collect();

    assert!(
        !ids.contains(&"cbd7d081-MILESTONE-FINAL"),
        "MILESTONE-FINAL must be deferred while REFACTOR-N-001/002 are active, got: {ids:?}"
    );
    // REFACTOR-N-001 is in_progress so it's not a todo candidate; REFACTOR-N-002
    // (todo, disjoint file) must be schedulable.
    assert!(
        ids.contains(&"cbd7d081-REFACTOR-N-002"),
        "REFACTOR-N-002 (todo, disjoint file) must be eligible for dispatch, got: {ids:?}"
    );

    // Phase 2 — once both fixups are done the milestone becomes re-eligible.
    conn.execute(
        "UPDATE tasks SET status = 'done' WHERE id IN ('cbd7d081-REFACTOR-N-001', 'cbd7d081-REFACTOR-N-002')",
        [],
    )
    .unwrap();

    let group =
        select_parallel_group(&conn, &[], Some("cbd7d081"), 2).expect("select_parallel_group");
    let ids: Vec<&str> = group.iter().map(|s| s.task.id.as_str()).collect();

    assert!(
        ids.contains(&"cbd7d081-MILESTONE-FINAL"),
        "MILESTONE-FINAL must be selectable once REFACTOR-N-001/002 are done, got: {ids:?}"
    );
}
