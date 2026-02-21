//! Integration tests for the full model selection pipeline.
//!
//! Tests the end-to-end flow: PRD with model fields → init → DB → build_prompt → resolved_model.
//! Also verifies escalation template injection, iteration header formatting, and progress logging.

use std::fs;
use std::path::Path;
use tempfile::TempDir;

use task_mgr::commands::init;
use task_mgr::db::open_connection;
use task_mgr::loop_engine::display::format_iteration_header;
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
use task_mgr::loop_engine::prompt::{build_prompt, BuildPromptParams};

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Initialize a PRD fixture into a temp directory and return (temp_dir, conn).
fn init_prd(fixture_name: &str) -> (TempDir, rusqlite::Connection) {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path(fixture_name);

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

    let conn = open_connection(temp_dir.path()).unwrap();
    (temp_dir, conn)
}

/// Create a base prompt file and return its path.
fn create_base_prompt(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("prompt.md");
    fs::write(&path, "# Agent Instructions\n\nImplement the task.\n").unwrap();
    path
}

/// Create an escalation template file under base_prompt_dir/scripts/.
fn create_escalation_template(base_prompt_dir: &Path, content: &str) {
    let scripts_dir = base_prompt_dir.join("scripts");
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::write(scripts_dir.join("escalation-policy.md"), content).unwrap();
}

// ============================================================================
// AC: PRD with default_model=haiku + high-difficulty task → resolved_model is opus
// ============================================================================

#[test]
fn test_e2e_high_difficulty_resolves_to_opus() {
    let (temp_dir, conn) = init_prd("prd_model_resolution_integration.json");
    let base_prompt_path = create_base_prompt(temp_dir.path());

    // Read default_model from prd_metadata (should be haiku)
    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        default_model.as_deref(),
        Some(HAIKU_MODEL),
        "PRD default_model should be haiku"
    );

    // MR-001 is highest priority (1), has difficulty='high', no explicit model
    let params = BuildPromptParams {
        dir: temp_dir.path(),
        project_root: temp_dir.path(),
        conn: &conn,
        after_files: &[],
        run_id: None,
        iteration: 1,
        reorder_hint: None,
        session_guidance: "",
        base_prompt_path: &base_prompt_path,
        steering_path: None,
        verbose: false,
        default_model: default_model.as_deref(),
    };

    let result = build_prompt(&params)
        .unwrap()
        .expect("Should return a prompt");

    assert_eq!(
        result.task_id, "MR-001",
        "Should select highest priority task"
    );
    assert_eq!(
        result.resolved_model,
        Some(OPUS_MODEL.to_string()),
        "High difficulty with haiku default should resolve to opus"
    );
}

// ============================================================================
// AC: PRD with default_model=sonnet + task with explicit opus model → opus
// ============================================================================

#[test]
fn test_e2e_explicit_model_overrides_default() {
    let (temp_dir, conn) = init_prd("prd_with_all_model_fields.json");
    let base_prompt_path = create_base_prompt(temp_dir.path());

    // This PRD has default_model=sonnet. MT-001 has explicit opus model.
    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(default_model.as_deref(), Some(SONNET_MODEL));

    let params = BuildPromptParams {
        dir: temp_dir.path(),
        project_root: temp_dir.path(),
        conn: &conn,
        after_files: &[],
        run_id: None,
        iteration: 1,
        reorder_hint: None,
        session_guidance: "",
        base_prompt_path: &base_prompt_path,
        steering_path: None,
        verbose: false,
        default_model: default_model.as_deref(),
    };

    let result = build_prompt(&params)
        .unwrap()
        .expect("Should return a prompt");

    assert_eq!(
        result.task_id, "MT-001",
        "Should select MT-001 (highest priority)"
    );
    assert_eq!(
        result.resolved_model,
        Some(OPUS_MODEL.to_string()),
        "Explicit opus model should override sonnet default"
    );
}

// ============================================================================
// AC: PRD with no default_model + task with no model → resolved_model is None
// ============================================================================

#[test]
fn test_e2e_no_model_fields_resolves_to_none() {
    let (temp_dir, conn) = init_prd("prd_no_model_fields.json");
    let base_prompt_path = create_base_prompt(temp_dir.path());

    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        default_model, None,
        "Legacy PRD should have no default_model"
    );

    let params = BuildPromptParams {
        dir: temp_dir.path(),
        project_root: temp_dir.path(),
        conn: &conn,
        after_files: &[],
        run_id: None,
        iteration: 1,
        reorder_hint: None,
        session_guidance: "",
        base_prompt_path: &base_prompt_path,
        steering_path: None,
        verbose: false,
        default_model: None,
    };

    let result = build_prompt(&params)
        .unwrap()
        .expect("Should return a prompt");

    assert_eq!(
        result.resolved_model, None,
        "No model fields at any level should resolve to None"
    );
}

// ============================================================================
// AC: Escalation template present for non-opus, absent for opus
// ============================================================================

#[test]
fn test_e2e_escalation_template_present_for_haiku_absent_for_opus() {
    let (temp_dir, conn) = init_prd("prd_model_resolution_integration.json");
    let base_prompt_path = create_base_prompt(temp_dir.path());
    create_escalation_template(temp_dir.path(), "ESCALATION_INTEGRATION_MARKER");

    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    // MR-001 resolves to opus (high difficulty) — escalation should be ABSENT
    let params = BuildPromptParams {
        dir: temp_dir.path(),
        project_root: temp_dir.path(),
        conn: &conn,
        after_files: &[],
        run_id: None,
        iteration: 1,
        reorder_hint: None,
        session_guidance: "",
        base_prompt_path: &base_prompt_path,
        steering_path: None,
        verbose: false,
        default_model: default_model.as_deref(),
    };

    let result = build_prompt(&params)
        .unwrap()
        .expect("Should return a prompt");

    assert_eq!(result.resolved_model.as_deref(), Some(OPUS_MODEL));
    assert!(
        !result.prompt.contains("ESCALATION_INTEGRATION_MARKER"),
        "Escalation template must be absent for opus-resolved task"
    );

    // Now mark MR-001 done, MR-003 will be selected (no model, default=haiku)
    conn.execute("UPDATE tasks SET status = 'done' WHERE id = 'MR-001'", [])
        .unwrap();
    conn.execute("UPDATE tasks SET status = 'done' WHERE id = 'MR-002'", [])
        .unwrap();

    let result2 = build_prompt(&params)
        .unwrap()
        .expect("Should return a prompt");

    assert_eq!(result2.task_id, "MR-003");
    assert_eq!(
        result2.resolved_model,
        Some(HAIKU_MODEL.to_string()),
        "MR-003 with no model/difficulty should fall back to haiku default"
    );
    assert!(
        result2.prompt.contains("ESCALATION_INTEGRATION_MARKER"),
        "Escalation template must be present for haiku-resolved task"
    );
}

// ============================================================================
// AC: Iteration header displays correct model name
// ============================================================================

#[test]
fn test_e2e_iteration_header_displays_model() {
    // format_iteration_header is a pure function — verify it formats correctly
    let header_opus = format_iteration_header(1, 10, "MR-001", 60, Some(OPUS_MODEL));
    assert!(
        header_opus.contains("Model: claude-opus-4-6"),
        "Header should display opus model name"
    );

    let header_none = format_iteration_header(2, 10, "MR-003", 120, None);
    assert!(
        header_none.contains("Model: (default)"),
        "Header should display '(default)' when model is None"
    );
}

// ============================================================================
// AC: Progress.txt model field contains correct model name
// ============================================================================

#[test]
fn test_e2e_progress_log_records_model() {
    let temp_dir = TempDir::new().unwrap();
    let progress_path = temp_dir.path().join("progress.txt");

    task_mgr::loop_engine::progress::log_iteration(
        &progress_path,
        1,
        Some("MR-001"),
        &task_mgr::loop_engine::config::IterationOutcome::Completed,
        &["src/core.rs".to_string()],
        Some(OPUS_MODEL),
    );

    let content = fs::read_to_string(&progress_path).unwrap();
    assert!(
        content.contains("- Model: claude-opus-4-6"),
        "Progress log should contain model name"
    );

    // Second iteration with no model
    task_mgr::loop_engine::progress::log_iteration(
        &progress_path,
        2,
        Some("MR-003"),
        &task_mgr::loop_engine::config::IterationOutcome::Completed,
        &[],
        None,
    );

    let content = fs::read_to_string(&progress_path).unwrap();
    assert!(
        content.contains("- Model: (default)"),
        "Progress log should show '(default)' for None model"
    );
}
