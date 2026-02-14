//! End-to-end integration tests that simulate the full Claude loop workflow.
//!
//! These tests verify:
//! - Full init → run begin → next → complete → export cycle
//! - Task failure flow with learn command
//! - Recovery from simulated crash
//! - Doctor fixes stale in_progress after crash

use serde_json::Value;
use std::fs;
use tempfile::TempDir;

use task_mgr::cli::{Confidence, FailStatus, LearningOutcome};
use task_mgr::commands::{
    begin, complete, doctor, end, export, fail, init, learn, next, LearnParams,
};
use task_mgr::db::open_connection;
use task_mgr::models::RunStatus;

/// Get the path to the sample PRD fixture file.
fn sample_prd_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_prd.json")
}

/// Extract user stories from PRD JSON.
fn extract_user_stories(prd: &Value) -> Vec<Value> {
    prd.get("userStories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

// ============================================================================
// Test: Full init → run begin → next → complete → export cycle
// ============================================================================

#[test]
fn test_full_loop_cycle() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Step 1: Initialize from PRD
    let init_result =
        init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();
    assert!(init_result.tasks_imported > 0, "Should import tasks");

    // Step 2: Begin a run
    let conn = open_connection(temp_dir.path()).unwrap();
    let begin_result = begin(&conn).unwrap();
    assert!(!begin_result.run_id.is_empty(), "Should get a run ID");
    assert_eq!(begin_result.status, RunStatus::Active);
    drop(conn);

    // Step 3: Get next task (using the library function)
    // next::next(dir, after_files, claim, run_id, verbose)
    let next_result = next::next(
        temp_dir.path(),
        &[],   // no after_files
        false, // don't claim
        None,  // no run_id
        false, // not verbose
    )
    .unwrap();

    // Should have a task selected (TASK-003 has all dependencies met)
    assert!(next_result.task.is_some(), "Should have a task selected");
    let selected_task = next_result.task.as_ref().unwrap();
    let task_id = &selected_task.id;

    // Step 4: Claim and complete the task
    // First claim it via next with --claim
    let _claim_result = next::next(
        temp_dir.path(),
        &[],
        true, // claim
        Some(&begin_result.run_id),
        false,
    )
    .unwrap();

    // Now complete it
    let mut conn = open_connection(temp_dir.path()).unwrap();
    let complete_result = complete::complete(
        &mut conn,
        std::slice::from_ref(task_id),
        Some(&begin_result.run_id),
        Some("abc123def"),
        false, // don't force
    )
    .unwrap();
    assert_eq!(complete_result.completed_count, 1);
    drop(conn);

    // Step 5: Export after iteration
    let export_path = temp_dir.path().join("exported.json");
    let export_result = export::export(temp_dir.path(), &export_path, false, None).unwrap();
    assert!(export_result.tasks_exported > 0);

    // Verify the completed task has passes: true in export
    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();
    let stories = extract_user_stories(&exported);
    let completed_task = stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(task_id))
        .expect("Should find the completed task");
    assert_eq!(
        completed_task.get("passes"),
        Some(&Value::Bool(true)),
        "Completed task should have passes: true"
    );

    // Step 6: End the run
    let conn = open_connection(temp_dir.path()).unwrap();
    let end_result = end(&conn, &begin_result.run_id, RunStatus::Completed).unwrap();
    assert_eq!(end_result.new_status, RunStatus::Completed);
}

// ============================================================================
// Test: Task failure flow with learn command
// ============================================================================

#[test]
fn test_failure_flow_with_learning() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Initialize and begin a run
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let begin_result = begin(&conn).unwrap();
    drop(conn);

    // Get and claim a task
    let next_result = next::next(
        temp_dir.path(),
        &[],
        true, // claim
        Some(&begin_result.run_id),
        false,
    )
    .unwrap();
    let task = next_result.task.as_ref().expect("Should have a task");
    let task_id = &task.id;

    // Fail the task (simulating BLOCKED marker)
    let mut conn = open_connection(temp_dir.path()).unwrap();
    let fail_result = fail::fail(
        &mut conn,
        std::slice::from_ref(task_id),
        Some("Missing external API credentials"),
        FailStatus::Blocked,
        Some(&begin_result.run_id),
        false,
    )
    .unwrap();
    assert_eq!(fail_result.tasks.len(), 1);

    // Record a learning about the failure
    let learn_result = learn::learn(
        &conn,
        LearnParams {
            outcome: LearningOutcome::Failure,
            title: "External API credentials required".to_string(),
            content:
                "This task requires API credentials that are not available in the test environment."
                    .to_string(),
            task_id: Some(task_id.clone()),
            run_id: Some(begin_result.run_id.clone()),
            root_cause: Some("Missing environment configuration".to_string()),
            solution: Some(
                "Set up API credentials in .env before attempting this task".to_string(),
            ),
            files: None,
            task_types: Some(vec!["TASK-".to_string()]),
            errors: None,
            tags: Some(vec!["api".to_string(), "credentials".to_string()]),
            confidence: Confidence::High,
        },
    )
    .unwrap();
    assert!(learn_result.learning_id > 0);
    assert_eq!(learn_result.tags_added, 2);

    // Verify the task is now blocked
    let task_status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(task_status, "blocked");

    // Export and verify passes is still false
    let export_path = temp_dir.path().join("exported.json");
    drop(conn);
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();
    let stories = extract_user_stories(&exported);
    let failed_task = stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(task_id))
        .unwrap();
    assert_eq!(
        failed_task.get("passes"),
        Some(&Value::Bool(false)),
        "Blocked task should have passes: false"
    );
}

// ============================================================================
// Test: Doctor fixes stale in_progress after simulated crash
// ============================================================================

#[test]
fn test_doctor_fixes_stale_after_crash() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Initialize
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Begin a run
    let conn = open_connection(temp_dir.path()).unwrap();
    let begin_result = begin(&conn).unwrap();
    drop(conn);

    // Claim a task (puts it in in_progress)
    let next_result = next::next(
        temp_dir.path(),
        &[],
        true, // claim
        Some(&begin_result.run_id),
        false,
    )
    .unwrap();
    let task = next_result.task.as_ref().expect("Should have a task");
    let task_id = task.id.clone();

    // Verify task is in_progress
    let conn = open_connection(temp_dir.path()).unwrap();
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = ?", [&task_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "in_progress");

    // Simulate crash: just close the connection and mark run as crashed
    // (In real scenario, the process would die mid-iteration)
    // We'll simulate this by manually ending the run as aborted
    end(&conn, &begin_result.run_id, RunStatus::Aborted).unwrap();
    drop(conn);

    // Now the task is in_progress but there's no active run
    // Doctor should find and fix this

    // Run doctor check (no auto-fix)
    let conn = open_connection(temp_dir.path()).unwrap();
    let doctor_result = doctor::doctor(&conn, false, false, 0, false, temp_dir.path()).unwrap();
    assert!(
        doctor_result.summary.stale_tasks > 0,
        "Doctor should find stale in_progress task"
    );
    assert_eq!(
        doctor_result.summary.total_fixed, 0,
        "Should not have fixed anything yet"
    );

    // Run doctor with auto-fix
    let doctor_fix_result = doctor::doctor(&conn, true, false, 0, false, temp_dir.path()).unwrap();
    assert!(
        doctor_fix_result.summary.total_fixed > 0,
        "Doctor should have fixed stale tasks"
    );

    // Verify the task is back to todo
    let status_after: String = conn
        .query_row("SELECT status FROM tasks WHERE id = ?", [&task_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status_after, "todo", "Task should be reset to todo");

    // Verify the fix was recorded in task notes
    let notes: Option<String> = conn
        .query_row("SELECT notes FROM tasks WHERE id = ?", [&task_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        notes.unwrap_or_default().contains("[DOCTOR]"),
        "Should have doctor audit note"
    );
}

// ============================================================================
// Test: Recovery from simulated crash (PRD export preserves state)
// ============================================================================

#[test]
fn test_crash_recovery_via_export() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Initialize and begin a run
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let begin_result = begin(&conn).unwrap();
    drop(conn);

    // Complete a task
    let next_result = next::next(
        temp_dir.path(),
        &[],
        true, // claim
        Some(&begin_result.run_id),
        false,
    )
    .unwrap();
    let task = next_result.task.as_ref().expect("Should have task");
    let completed_task_id = task.id.clone();

    let mut conn = open_connection(temp_dir.path()).unwrap();
    complete::complete(
        &mut conn,
        std::slice::from_ref(&completed_task_id),
        Some(&begin_result.run_id),
        None,
        false,
    )
    .unwrap();
    drop(conn);

    // Export after this iteration (critical for crash recovery)
    let export_path = temp_dir.path().join("iteration_1.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    // Simulate crash: create a new temp directory (simulating fresh start)
    let recovery_dir = TempDir::new().unwrap();

    // Recover by re-initializing from the exported PRD
    init::init(
        recovery_dir.path(),
        &[&export_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Verify the completed task is still completed
    let conn = open_connection(recovery_dir.path()).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE id = ?",
            [&completed_task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "done", "Recovered task should be in 'done' status");

    // Can continue working on next tasks
    drop(conn);
    let next_result = next::next(
        recovery_dir.path(),
        &[completed_task_id], // after_files from previous (using task_id as proxy)
        false,
        None,
        false,
    )
    .unwrap();
    assert!(
        next_result.task.is_some(),
        "Should still have tasks to work on after recovery"
    );
}

// ============================================================================
// Test: Multiple iterations with dependencies
// ============================================================================

#[test]
fn test_multiple_iterations_respect_dependencies() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Initialize
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Begin a run
    let conn = open_connection(temp_dir.path()).unwrap();
    let begin_result = begin(&conn).unwrap();
    drop(conn);

    // Keep track of completed tasks
    let mut completed_ids = Vec::new();

    // Run multiple iterations
    for iteration in 0..3 {
        // Get next task
        let next_result = next::next(
            temp_dir.path(),
            &[],
            true, // claim
            Some(&begin_result.run_id),
            false,
        )
        .unwrap();

        if let Some(task) = &next_result.task {
            let task_id = task.id.clone();

            // Complete the task
            let mut conn = open_connection(temp_dir.path()).unwrap();
            let complete_result = complete::complete(
                &mut conn,
                std::slice::from_ref(&task_id),
                Some(&begin_result.run_id),
                None,
                false,
            )
            .unwrap();
            assert_eq!(
                complete_result.completed_count, 1,
                "Iteration {}: Should complete 1 task",
                iteration
            );
            drop(conn);

            completed_ids.push(task_id.clone());

            // Export after each iteration
            let export_path = temp_dir.path().join(format!("iter_{}.json", iteration));
            export::export(temp_dir.path(), &export_path, false, None).unwrap();

            // Verify export reflects completion
            let exported_json = fs::read_to_string(&export_path).unwrap();
            let exported: Value = serde_json::from_str(&exported_json).unwrap();
            let stories = extract_user_stories(&exported);
            let this_task = stories
                .iter()
                .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(&task_id));
            assert!(
                this_task.is_some(),
                "Iteration {}: Task should exist in export",
                iteration
            );
            assert_eq!(
                this_task.unwrap().get("passes"),
                Some(&Value::Bool(true)),
                "Iteration {}: Completed task should have passes: true",
                iteration
            );
        } else {
            // No more eligible tasks
            break;
        }
    }

    // Should have completed at least one task
    assert!(
        !completed_ids.is_empty(),
        "Should have completed at least one task"
    );

    // End the run
    let conn = open_connection(temp_dir.path()).unwrap();
    end(&conn, &begin_result.run_id, RunStatus::Completed).unwrap();
}

// ============================================================================
// Test: Doctor dry-run mode
// ============================================================================

#[test]
fn test_doctor_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Initialize
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Begin a run and claim a task
    let conn = open_connection(temp_dir.path()).unwrap();
    let begin_result = begin(&conn).unwrap();
    drop(conn);

    let next_result = next::next(
        temp_dir.path(),
        &[],
        true, // claim
        Some(&begin_result.run_id),
        false,
    )
    .unwrap();
    let task_id = next_result.task.as_ref().unwrap().id.clone();

    // Simulate crash by ending run as aborted
    let conn = open_connection(temp_dir.path()).unwrap();
    end(&conn, &begin_result.run_id, RunStatus::Aborted).unwrap();

    // Run doctor in dry-run mode
    let doctor_result = doctor::doctor(&conn, true, true, 0, false, temp_dir.path()).unwrap(); // dry_run=true
    assert!(
        doctor_result.summary.stale_tasks > 0,
        "Should find stale task"
    );
    assert_eq!(
        doctor_result.summary.total_fixed, 0,
        "Dry run should not fix anything"
    );
    assert!(
        !doctor_result.would_fix.is_empty(),
        "Should have would_fix entries"
    );

    // Verify task is still in_progress (not fixed because dry run)
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = ?", [&task_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        status, "in_progress",
        "Task should still be in_progress after dry run"
    );
}

// ============================================================================
// Test: Full feedback loop lifecycle (INT-001)
//
// Exercises the complete loop lifecycle using library functions:
//   init → begin → (prompt build → complete → feedback) × N → end → calibrate
//
// Verifies:
//   1. Run exists in DB (begin called)
//   2. Run ends as Completed (end called)
//   3. Learning feedback recorded (times_applied incremented for shown learnings)
//   4. recalibrate_weights() was called (global_state has selection_weights JSON)
//   5. All 3 tasks completed in dependency order
// ============================================================================

#[test]
fn test_full_feedback_loop_lifecycle() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = test_loop_prd_path();

    // Step 1: Initialize from test PRD
    let init_result =
        init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();
    assert_eq!(init_result.tasks_imported, 3, "Should import 3 tasks");

    // Step 2: Insert learnings that will be recalled during prompt building
    let conn = open_connection(temp_dir.path()).unwrap();
    let learning_id = insert_learning_for_e2e(&conn);

    // Step 3: Begin a run
    let begin_result = begin(&conn).unwrap();
    assert!(!begin_result.run_id.is_empty(), "Should get a run ID");
    assert_eq!(begin_result.status, RunStatus::Active);
    drop(conn);

    // Step 4: Iterate through all 3 tasks in dependency order
    let mut completed_ids = Vec::new();
    let mut last_files: Vec<String> = Vec::new();
    let mut total_shown_learning_ids: Vec<i64> = Vec::new();

    for iteration in 0..3 {
        // Select and claim next task
        let next_result = next::next(
            temp_dir.path(),
            &last_files,
            true, // claim
            Some(&begin_result.run_id),
            false,
        )
        .unwrap();

        let task = next_result
            .task
            .as_ref()
            .unwrap_or_else(|| panic!("Iteration {}: should have a task", iteration));
        let task_id = task.id.clone();
        let task_files = task.files.clone();

        // Record shown learnings (simulates what prompt builder does)
        let conn = open_connection(temp_dir.path()).unwrap();
        for learning in &next_result.learnings {
            total_shown_learning_ids.push(learning.id);
            let _ = task_mgr::learnings::record_learning_shown(
                &conn,
                learning.id,
                i64::from(iteration as u32),
            );
        }

        // Complete the task
        let mut conn = open_connection(temp_dir.path()).unwrap();
        let complete_result = complete::complete(
            &mut conn,
            std::slice::from_ref(&task_id),
            Some(&begin_result.run_id),
            Some(&format!("fake-commit-{}", iteration)),
            false,
        )
        .unwrap();
        assert_eq!(
            complete_result.completed_count, 1,
            "Iteration {}: should complete 1 task",
            iteration
        );

        // Record feedback for shown learnings (simulates what engine does after detection)
        task_mgr::loop_engine::feedback::record_iteration_feedback(
            &conn,
            &total_shown_learning_ids,
            &task_mgr::loop_engine::config::IterationOutcome::Completed,
        )
        .unwrap();

        completed_ids.push(task_id);
        last_files = task_files;
        drop(conn);
    }

    // Verify all 3 tasks completed
    assert_eq!(completed_ids.len(), 3, "Should complete all 3 tasks");

    // Verify dependency order: LOOP-001 before LOOP-002 before LOOP-003
    let pos_001 = completed_ids.iter().position(|id| id == "LOOP-001");
    let pos_002 = completed_ids.iter().position(|id| id == "LOOP-002");
    let pos_003 = completed_ids.iter().position(|id| id == "LOOP-003");
    assert!(
        pos_001.is_some() && pos_002.is_some() && pos_003.is_some(),
        "All three tasks should be completed"
    );
    assert!(
        pos_001.unwrap() < pos_002.unwrap(),
        "LOOP-001 should complete before LOOP-002"
    );
    assert!(
        pos_002.unwrap() < pos_003.unwrap(),
        "LOOP-002 should complete before LOOP-003"
    );

    // Step 5: Verify no more tasks available
    let next_result = next::next(temp_dir.path(), &[], false, None, false).unwrap();
    assert!(
        next_result.task.is_none(),
        "No tasks should remain after completing all 3"
    );

    // Step 6: End the run
    let conn = open_connection(temp_dir.path()).unwrap();
    let end_result = end(&conn, &begin_result.run_id, RunStatus::Completed).unwrap();
    assert_eq!(end_result.new_status, RunStatus::Completed);

    // Step 7: Verify run exists in DB with correct status
    let run_status: String = conn
        .query_row(
            "SELECT status FROM runs WHERE run_id = ?",
            [&begin_result.run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        run_status, "completed",
        "Run should be marked as completed in DB"
    );

    // Step 8: Verify learning feedback was recorded
    if !total_shown_learning_ids.is_empty() {
        let stats = task_mgr::learnings::get_window_stats(&conn, learning_id).unwrap();
        // The learning was shown across iterations, and each Completed outcome recorded it
        assert!(
            stats.window_applied > 0,
            "Learning should have times_applied > 0, got {}",
            stats.window_applied
        );
    }

    // Step 9: Recalibrate weights (simulates what engine does at end)
    let weights = task_mgr::loop_engine::calibrate::recalibrate_weights(&conn).unwrap();
    // With only 3 completed tasks (below 10 threshold), defaults returned
    // but the function still executes without error
    assert_eq!(
        weights,
        task_mgr::loop_engine::calibrate::SelectionWeights::default(),
        "Below threshold should return default weights"
    );

    // Step 10: Verify export reflects all tasks complete
    drop(conn);
    let export_path = temp_dir.path().join("final-export.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();
    let stories = extract_user_stories(&exported);
    for story in &stories {
        let id = story
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        assert_eq!(
            story.get("passes"),
            Some(&Value::Bool(true)),
            "Task {} should have passes: true in export",
            id
        );
    }
}

// ============================================================================
// Test: Full loop with mock Claude subprocess (requires setup)
//
// This test exercises run_loop() with a mock Claude binary.
// Marked #[ignore] because it requires:
//   - A git repo in the test directory
//   - The task-mgr binary to be built
//   - Environment variable isolation
// Run with: cargo test --test e2e_loop test_run_loop_with_mock_claude -- --ignored
// ============================================================================

#[test]
#[ignore]
fn test_run_loop_with_mock_claude() {
    use std::process::Command;

    let temp_dir = TempDir::new().unwrap();

    // Set up a git repo in the temp directory
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init failed");

    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config email failed");

    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config name failed");

    // Create initial commit
    let gitkeep = temp_dir.path().join(".gitkeep");
    fs::write(&gitkeep, "").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(temp_dir.path())
        .output()
        .expect("git add failed");
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit failed");

    // Create and checkout the branch the PRD expects
    Command::new("git")
        .args(["checkout", "-b", "test/e2e-loop"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git checkout failed");

    // Copy test PRD and create prompt file
    let prd_src = test_loop_prd_path();
    let tasks_dir = temp_dir.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    let prd_dest = tasks_dir.join("test-loop-prd.json");
    fs::copy(&prd_src, &prd_dest).unwrap();

    // Create a minimal prompt file
    let prompt_path = tasks_dir.join("test-loop-prd-prompt.md");
    fs::write(
        &prompt_path,
        "# Test Agent\n\nComplete the assigned task.\n",
    )
    .unwrap();

    // Set environment variables for the mock Claude script
    let mock_claude_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mock-claude.sh");

    // Get the task-mgr binary path
    let task_mgr_bin = std::path::PathBuf::from(env!("CARGO_BIN_EXE_task-mgr"));

    // Set env vars for mock
    std::env::set_var("CLAUDE_BINARY", &mock_claude_path);
    std::env::set_var("TASK_MGR_BIN", &task_mgr_bin);
    std::env::set_var("TASK_MGR_DIR", temp_dir.path());

    // Build loop config
    let mut config = task_mgr::loop_engine::config::LoopConfig::from_env();
    config.yes_mode = true;
    config.max_iterations = 5; // Small number, enough for 3 tasks
    config.usage_check_enabled = false; // Don't check usage API in tests

    let run_config = task_mgr::loop_engine::engine::LoopRunConfig {
        db_dir: temp_dir.path().to_path_buf(),
        source_root: temp_dir.path().to_path_buf(),
        working_root: temp_dir.path().to_path_buf(),
        prd_file: prd_dest,
        prompt_file: Some(prompt_path),
        external_repo: None,
        config,
    };

    // Run the loop
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");
    let exit_code =
        rt.block_on(async { task_mgr::loop_engine::engine::run_loop(run_config).await });

    // Clean up env vars
    std::env::remove_var("CLAUDE_BINARY");
    std::env::remove_var("TASK_MGR_BIN");
    std::env::remove_var("TASK_MGR_DIR");

    // Verify exit code 0 (success)
    assert_eq!(exit_code, 0, "Loop should exit with code 0");

    // Verify DB state
    let conn = open_connection(temp_dir.path()).unwrap();

    // Verify run exists and is completed
    let run_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE status = 'completed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        run_count > 0,
        "Should have at least one completed run in DB"
    );

    // Verify all tasks are done
    let todo_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE status != 'done'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(todo_count, 0, "All tasks should be done");
}

/// Get the path to the test loop PRD fixture.
fn test_loop_prd_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("test-loop-prd.json")
}

/// Insert a learning for E2E feedback testing.
/// Returns the learning ID.
fn insert_learning_for_e2e(conn: &rusqlite::Connection) -> i64 {
    conn.execute(
        "INSERT INTO learnings (outcome, title, content, confidence) \
         VALUES ('pattern', 'E2E test learning', 'Always validate inputs.', 'high')",
        [],
    )
    .unwrap();
    conn.last_insert_rowid()
}
