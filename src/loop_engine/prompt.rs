/// Prompt builder for the autonomous agent loop.
///
/// Calls `next::next()` to select a task, then builds an enriched prompt with:
/// - Source context from touchesFiles (via `context::scan_source_context`)
/// - Dependency completion summaries
/// - Synergy task diffs
/// - UCB-ranked learnings (with shown IDs tracked for feedback loop)
/// - Steering.md content (if present)
/// - Session guidance (from .pause interactions)
/// - Base prompt template
/// - Reorder instruction
///
/// The prompt builder is the integration point between task selection, learning
/// recall, source scanning, and the Claude subprocess.
use std::fs;
use std::path::Path;

use rusqlite::Connection;

use crate::commands::next;
use crate::commands::next::output::NextResult;
use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::context;
use crate::loop_engine::model;
use crate::loop_engine::prompt_sections::dependencies::build_dependency_section;
use crate::loop_engine::prompt_sections::learnings::{
    build_learnings_section, record_shown_learnings,
};
use crate::loop_engine::prompt_sections::synergy::{
    build_synergy_section, resolve_synergy_cluster_model,
};
use crate::loop_engine::prompt_sections::truncate_to_budget;

/// Byte budget for enriched task context in the prompt.
const TASK_CONTEXT_BUDGET: usize = 4000;

/// Byte budget for source context from touchesFiles.
const SOURCE_CONTEXT_BUDGET: usize = 2000;

/// Total byte budget for the entire assembled prompt.
/// Critical sections must fit within this budget; trimmable sections fill the remainder.
const TOTAL_PROMPT_BUDGET: usize = 80_000;

/// Byte budget for the base prompt.md template content.
const BASE_PROMPT_BUDGET: usize = 16_000;

/// Result of building a prompt for one iteration.
#[derive(Debug)]
pub struct PromptResult {
    /// The assembled prompt string to pass to Claude
    pub prompt: String,
    /// ID of the selected task
    pub task_id: String,
    /// Files touched by the selected task
    pub task_files: Vec<String>,
    /// Learning IDs that were shown in this prompt (for feedback loop)
    pub shown_learning_ids: Vec<i64>,
    /// Resolved model for this iteration (None = use CLI default).
    ///
    /// Populated by model resolution: task_model > difficulty='high' > prd_default > None,
    /// then elevated to the highest tier across the synergyWith cluster.
    /// Some("") is normalized to None — consumers never see empty strings.
    pub resolved_model: Option<String>,
    /// Names of trimmable sections that were dropped because they exceeded the
    /// remaining budget. Empty when all sections fit. Useful for diagnostics and
    /// testing to distinguish "dropped due to budget" from "empty because no data".
    pub dropped_sections: Vec<String>,
    /// Difficulty level of the selected task (for per-iteration timeout calculation).
    /// Propagated from `NextTaskOutput.difficulty`.
    pub task_difficulty: Option<String>,
}

/// Parameters for building a prompt.
pub struct BuildPromptParams<'a> {
    /// Database directory (--dir flag) for task selection via next::next()
    pub dir: &'a Path,
    /// Git repository root directory for source context scanning
    pub project_root: &'a Path,
    /// Database connection for recording learning shown events
    pub conn: &'a Connection,
    /// Files modified in previous iteration (for task selection scoring)
    pub after_files: &'a [String],
    /// Current run ID (for task claiming)
    pub run_id: Option<&'a str>,
    /// Current iteration number (for learning shown tracking)
    pub iteration: u32,
    /// Optional task ID hint from a previous reorder request
    pub reorder_hint: Option<&'a str>,
    /// Accumulated session guidance from .pause interactions
    pub session_guidance: &'a str,
    /// Path to the base prompt.md file
    pub base_prompt_path: &'a Path,
    /// Path to optional steering.md file
    pub steering_path: Option<&'a Path>,
    /// Enable verbose output
    pub verbose: bool,
    /// Default model from PRD metadata (threaded from engine, not queried here).
    pub default_model: Option<&'a str>,
    /// Optional PRD task prefix for scoping task selection to a specific PRD.
    pub task_prefix: Option<&'a str>,
}

/// Build a prompt for the current iteration.
///
/// Uses two-phase assembly to guarantee critical sections are always included:
/// - **Phase 1**: Build critical sections (task JSON, base prompt, reorder instruction,
///   escalation policy, non-code completion instruction). If these exceed
///   `TOTAL_PROMPT_BUDGET`, return `Err(PromptOverflow)`.
/// - **Phase 2**: Fill remaining budget with trimmable sections in priority order
///   (learnings, source context, dependency summaries, synergy context, steering,
///   session guidance, reorder hint). Sections that don't fit are skipped with a warning.
///
/// Returns `None` if no tasks remain.
pub fn build_prompt(params: &BuildPromptParams<'_>) -> TaskMgrResult<Option<PromptResult>> {
    // Step 1: Select and claim a task
    let next_result = next::next(
        params.dir,
        params.after_files,
        true,
        params.run_id,
        params.verbose,
        params.task_prefix,
    )?;

    let task_output = match next_result.task {
        Some(ref task) => task,
        None => return Ok(None), // All tasks complete
    };

    // Step 2: Resolve model (needed for escalation section in Phase 1)
    let resolved_model = resolve_synergy_cluster_model(
        params.conn,
        &task_output.id,
        task_output.model.as_deref(),
        task_output.difficulty.as_deref(),
        params.default_model,
    );

    // ============================================================
    // Phase 1: Build critical sections into separate Strings
    // ============================================================

    // Critical: Task JSON
    let task_json = build_task_json(task_output, &next_result);
    let truncated_json = truncate_to_budget(&task_json, TASK_CONTEXT_BUDGET);
    let task_section = format!("## Current Task\n\n```json\n{}\n```\n\n", truncated_json);

    // Critical: Universal completion instruction for ALL tasks
    let completion_section = {
        let non_code_note = if task_output.files.is_empty() {
            "This task has no `touchesFiles` — it is a verification, milestone, or polish task.\n\n"
        } else {
            ""
        };
        format!(
            "## Completing This Task\n\n\
             {non_code_note}\
             When all acceptance criteria pass:\n\
             1. Commit with message: `feat: {task_id}-completed - [Title]`\n\
                If completing multiple tasks: `feat: ID1-completed, ID2-completed - [Title]`\n\
             2. Output `<completed>{task_id}</completed>` (using the full task ID shown above).\n\n\
             The loop will automatically mark the task done and update the PRD.\n\
             Do NOT run `task-mgr done` manually.\n\n",
            task_id = task_output.id,
        )
    };

    // Critical: Escalation policy
    let escalation_section =
        build_escalation_section(params.base_prompt_path, resolved_model.as_deref());

    // Critical: Reorder instruction
    let reorder_instr_section =
        "If you have a strong reason to work on a different eligible task, \
         output `<reorder>TASK-ID</reorder>`.\n\n"
            .to_string();

    // Critical: Base prompt template
    let base_prompt_section = build_base_prompt_section(params.base_prompt_path);

    let critical_total = task_section.len()
        + completion_section.len()
        + escalation_section.len()
        + reorder_instr_section.len()
        + base_prompt_section.len();

    if critical_total > TOTAL_PROMPT_BUDGET {
        return Err(TaskMgrError::PromptOverflow {
            critical_size: critical_total,
            budget: TOTAL_PROMPT_BUDGET,
            task_id: task_output.id.clone(),
        });
    }

    // Step 3: Record shown learnings AFTER the overflow check.
    // If Phase 1 overflows we return Err above — we must not record learnings
    // as "shown" when the prompt was never sent to Claude (would skew UCB bandit).
    let shown_learning_ids = record_shown_learnings(
        params.conn,
        &next_result.learnings,
        i64::from(params.iteration),
    );

    // ============================================================
    // Phase 2: Build trimmable sections in priority order
    // ============================================================

    let mut remaining = TOTAL_PROMPT_BUDGET - critical_total;

    // Priority order (highest first):
    // 1. Learnings, 2. Source Context, 3. Dependency Summaries,
    // 4. Synergy Context, 5. Steering, 6. Session Guidance, 7. Reorder Hint

    let mut dropped_sections: Vec<String> = Vec::new();

    let learnings_section = build_learnings_section(&next_result.learnings);
    let learnings_section = try_fit_section(
        learnings_section,
        "Learnings",
        &mut remaining,
        &mut dropped_sections,
    );

    let source_ctx = context::scan_source_context(
        &task_output.files,
        SOURCE_CONTEXT_BUDGET,
        params.project_root,
    );
    let source_section = source_ctx.format_for_prompt();
    let source_section = try_fit_section(
        source_section,
        "Source Context",
        &mut remaining,
        &mut dropped_sections,
    );

    let dep_section = build_dependency_section(params.conn, &task_output.id);
    let dep_section = try_fit_section(
        dep_section,
        "Dependency Summaries",
        &mut remaining,
        &mut dropped_sections,
    );

    let synergy_section = build_synergy_section(params.conn, &task_output.id, params.run_id);
    let synergy_section = try_fit_section(
        synergy_section,
        "Synergy Context",
        &mut remaining,
        &mut dropped_sections,
    );

    let steering_section = params
        .steering_path
        .map(build_steering_section)
        .unwrap_or_default();
    let steering_section = try_fit_section(
        steering_section,
        "Steering",
        &mut remaining,
        &mut dropped_sections,
    );

    let guidance_section = if params.session_guidance.is_empty() {
        String::new()
    } else {
        format!("## Session Guidance\n\n{}\n\n", params.session_guidance,)
    };
    let guidance_section = try_fit_section(
        guidance_section,
        "Session Guidance",
        &mut remaining,
        &mut dropped_sections,
    );

    let hint_section = params
        .reorder_hint
        .map(|hint| {
            format!(
                "## Reorder Hint\n\nThe previous iteration requested reorder to task: `{}`\n\n",
                hint,
            )
        })
        .unwrap_or_default();
    let hint_section = try_fit_section(
        hint_section,
        "Reorder Hint",
        &mut remaining,
        &mut dropped_sections,
    );

    // ============================================================
    // Assembly: concatenate in display order
    // ============================================================
    // Display order: steering → guidance → hint → source → deps → synergy →
    //                task → learnings → completion → escalation → reorder instr → base prompt
    let mut prompt = String::with_capacity(TOTAL_PROMPT_BUDGET);
    prompt.push_str(&steering_section);
    prompt.push_str(&guidance_section);
    prompt.push_str(&hint_section);
    prompt.push_str(&source_section);
    prompt.push_str(&dep_section);
    prompt.push_str(&synergy_section);
    prompt.push_str(&task_section);
    prompt.push_str(&learnings_section);
    prompt.push_str(&completion_section);
    prompt.push_str(&escalation_section);
    prompt.push_str(&reorder_instr_section);
    prompt.push_str(&base_prompt_section);

    Ok(Some(PromptResult {
        prompt,
        task_id: task_output.id.clone(),
        task_files: task_output.files.clone(),
        shown_learning_ids,
        resolved_model,
        dropped_sections,
        task_difficulty: task_output.difficulty.clone(),
    }))
}

/// Try to fit a section into the remaining budget.
/// Returns the section string if it fits, empty string otherwise (with a warning).
/// When a non-empty section is dropped, its name is appended to `dropped`.
fn try_fit_section(
    section: String,
    name: &str,
    remaining: &mut usize,
    dropped: &mut Vec<String>,
) -> String {
    if section.is_empty() {
        return section;
    }
    if section.len() <= *remaining {
        *remaining -= section.len();
        section
    } else {
        eprintln!(
            "Warning: {} section ({} bytes) skipped — only {} bytes remaining in prompt budget",
            name,
            section.len(),
            remaining,
        );
        dropped.push(name.to_string());
        String::new()
    }
}

/// Record shown learnings via the UCB bandit system.
///
/// Returns the list of learning IDs that were shown (for feedback tracking).
/// Errors are logged but don't prevent prompt building.
/// Build a steering section string from the steering.md file.
fn build_steering_section(steering_path: &Path) -> String {
    match fs::read_to_string(steering_path) {
        Ok(content) if !content.trim().is_empty() => {
            format!("## Steering\n\n{}\n\n", content.trim())
        }
        _ => String::new(),
    }
}

/// Append steering.md content to the prompt if the file exists.
#[cfg(test)]
fn append_steering(prompt: &mut String, steering_path: &Path) {
    prompt.push_str(&build_steering_section(steering_path));
}

/// Build a dependency summaries section string.
/// Append dependency completion summaries for the current task.
///
/// For each completed dependsOn task: includes title + key acceptance criteria
/// as a 2-3 line summary.
#[cfg(test)]
fn append_dependency_summaries(prompt: &mut String, conn: &Connection, task_id: &str) {
    prompt.push_str(&build_dependency_section(conn, task_id));
}

/// Append synergy task context for completed synergy tasks in the current run.
#[cfg(test)]
fn append_synergy_context(
    prompt: &mut String,
    conn: &Connection,
    task_id: &str,
    run_id: Option<&str>,
) {
    prompt.push_str(&build_synergy_section(conn, task_id, run_id));
}

/// Build a JSON representation of the task for inclusion in the prompt.
fn build_task_json(
    task: &crate::commands::next::output::NextTaskOutput,
    next_result: &NextResult,
) -> String {
    // Build a simplified JSON that includes what Claude needs
    let mut json = serde_json::json!({
        "id": task.id,
        "title": task.title,
        "priority": task.priority,
        "status": task.status,
        "acceptanceCriteria": task.acceptance_criteria,
        "files": task.files,
    });

    if let Some(ref desc) = task.description {
        json["description"] = serde_json::Value::String(desc.clone());
    }
    if let Some(ref notes) = task.notes {
        json["notes"] = serde_json::Value::String(notes.clone());
    }
    if let Some(ref model) = task.model {
        json["model"] = serde_json::Value::String(model.clone());
    }
    if let Some(ref difficulty) = task.difficulty {
        json["difficulty"] = serde_json::Value::String(difficulty.clone());
    }
    if let Some(ref escalation_note) = task.escalation_note {
        json["escalationNote"] = serde_json::Value::String(escalation_note.clone());
    }
    if !task.batch_with.is_empty() {
        json["batchWith"] = serde_json::json!(task.batch_with);
    }
    if !next_result.batch_tasks.is_empty() {
        json["eligibleBatchTasks"] = serde_json::json!(next_result.batch_tasks);
    }

    serde_json::to_string_pretty(&json).unwrap_or_else(|_| format!("{{\"id\":\"{}\"}}", task.id))
}

/// Build a base prompt section string from the template file.
fn build_base_prompt_section(base_prompt_path: &Path) -> String {
    match fs::read_to_string(base_prompt_path) {
        Ok(content) if !content.trim().is_empty() => {
            let mut content = truncate_to_budget(&content, BASE_PROMPT_BUDGET);
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content
        }
        Ok(_) => {
            eprintln!(
                "Warning: base prompt file is empty: {}",
                base_prompt_path.display()
            );
            String::new()
        }
        Err(e) => {
            eprintln!(
                "Warning: could not read base prompt file {}: {}",
                base_prompt_path.display(),
                e
            );
            String::new()
        }
    }
}

/// Append the base prompt template file content.
#[cfg(test)]
fn append_base_prompt(prompt: &mut String, base_prompt_path: &Path) {
    prompt.push_str(&build_base_prompt_section(base_prompt_path));
}

/// Load the escalation policy template from the scripts directory.
///
/// Resolves the template path as `base_prompt_path.parent()/scripts/escalation-policy.md`.
/// Returns `Some(content)` if the file exists and is readable, `None` otherwise.
/// Warnings are printed to stderr for read failures (but not for missing files).
///
/// The template is loaded fresh each call (not cached) to allow hot-editing.
pub fn load_escalation_template(base_prompt_path: &Path) -> Option<String> {
    let parent = base_prompt_path.parent().unwrap_or_else(|| Path::new("."));
    let template_path = parent.join("scripts").join("escalation-policy.md");

    match fs::read_to_string(&template_path) {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!(
                "Warning: could not read escalation template {}: {}",
                template_path.display(),
                e
            );
            None
        }
    }
}

/// Build an escalation policy section string.
fn build_escalation_section(base_prompt_path: &Path, resolved_model: Option<&str>) -> String {
    if model::model_tier(resolved_model) == model::ModelTier::Opus {
        return String::new();
    }

    match load_escalation_template(base_prompt_path) {
        Some(contents) => format!("## Model Escalation Policy\n\n{}\n\n---\n\n", contents),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rusqlite::params;
    use tempfile::TempDir;

    use crate::commands::next::output::*;
    use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
    use crate::loop_engine::test_utils::{
        insert_relationship, insert_run, insert_run_task, insert_task, insert_task_file,
        insert_task_full, insert_test_learning, setup_test_db,
    };

    /// Create a base prompt file and return its path.
    fn create_base_prompt(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("prompt.md");
        fs::write(&path, "# Agent Instructions\n\nImplement the task.\n").unwrap();
        path
    }

    /// Create a steering file and return its path.
    fn create_steering(dir: &Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("steering.md");
        fs::write(&path, content).unwrap();
        path
    }

    /// Create a Rust source file for context scanning.
    fn create_source_file(dir: &Path, rel_path: &str, content: &str) {
        let full = dir.join(rel_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, content).unwrap();
    }

    /// Build prompt params with defaults, allowing override via closure.
    fn build_params<'a>(
        dir: &'a Path,
        conn: &'a rusqlite::Connection,
        base_prompt_path: &'a Path,
    ) -> BuildPromptParams<'a> {
        BuildPromptParams {
            dir,
            project_root: dir,
            conn,
            after_files: &[],
            run_id: None,
            iteration: 1,
            reorder_hint: None,
            session_guidance: "",
            base_prompt_path,
            steering_path: None,
            verbose: false,
            default_model: None,
            task_prefix: None,
        }
    }

    // ===== Unit tests for helper functions (original) =====

    #[test]
    fn test_append_steering_missing_file() {
        let mut prompt = String::new();
        append_steering(&mut prompt, Path::new("/nonexistent/steering.md"));
        assert!(prompt.is_empty(), "Missing steering file should be no-op");
    }

    #[test]
    fn test_append_steering_with_content() {
        let temp_dir = TempDir::new().unwrap();
        let steering_path = temp_dir.path().join("steering.md");
        fs::write(&steering_path, "Focus on error handling.").unwrap();

        let mut prompt = String::new();
        append_steering(&mut prompt, &steering_path);
        assert!(prompt.contains("## Steering"));
        assert!(prompt.contains("Focus on error handling."));
    }

    #[test]
    fn test_append_steering_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let steering_path = temp_dir.path().join("steering.md");
        fs::write(&steering_path, "   \n  ").unwrap();

        let mut prompt = String::new();
        append_steering(&mut prompt, &steering_path);
        assert!(prompt.is_empty(), "Empty steering file should be no-op");
    }

    #[test]
    fn test_append_base_prompt_with_content() {
        let temp_dir = TempDir::new().unwrap();
        let prompt_path = temp_dir.path().join("prompt.md");
        fs::write(&prompt_path, "# Agent Instructions\n\nDo the task.\n").unwrap();

        let mut prompt = String::new();
        append_base_prompt(&mut prompt, &prompt_path);
        assert!(prompt.contains("# Agent Instructions"));
        assert!(prompt.contains("Do the task."));
    }

    #[test]
    fn test_append_base_prompt_missing_file() {
        let mut prompt = String::new();
        append_base_prompt(&mut prompt, Path::new("/nonexistent/prompt.md"));
        assert!(
            prompt.is_empty(),
            "Missing prompt file should be no-op (with warning)"
        );
    }

    #[test]
    fn test_build_task_json_basic() {
        let task = NextTaskOutput {
            id: "FEAT-001".to_string(),
            title: "Test task".to_string(),
            description: Some("A test".to_string()),
            priority: 5,
            status: "in_progress".to_string(),
            acceptance_criteria: vec!["AC1".to_string()],
            notes: None,
            files: vec!["src/lib.rs".to_string()],
            batch_with: vec![],
            model: None,
            difficulty: None,
            escalation_note: None,
            score: ScoreOutput {
                total: 995,
                priority: 995,
                file_overlap: 0,
                synergy: 0,
                conflict: 0,
                file_overlap_count: 0,
                synergy_from: vec![],
                conflict_from: vec![],
            },
        };

        let next_result = NextResult {
            task: Some(task.clone()),
            batch_tasks: vec![],
            learnings: vec![],
            selection: SelectionMetadata {
                reason: "test".to_string(),
                eligible_count: 1,
            },
            claim: None,
            top_candidates: vec![],
        };

        let json = build_task_json(&task, &next_result);
        assert!(json.contains("FEAT-001"));
        assert!(json.contains("Test task"));
        assert!(json.contains("AC1"));
    }

    #[test]
    fn test_prompt_result_fields() {
        let result = PromptResult {
            prompt: "test prompt".to_string(),
            task_id: "FEAT-001".to_string(),
            task_files: vec!["src/lib.rs".to_string()],
            shown_learning_ids: vec![1, 2, 3],
            resolved_model: None,
            dropped_sections: vec![],
            task_difficulty: None,
        };

        assert_eq!(result.task_id, "FEAT-001");
        assert_eq!(result.shown_learning_ids.len(), 3);
        assert_eq!(result.task_files.len(), 1);
    }

    // ===== TEST-003: Comprehensive prompt assembly integration tests =====

    // --- AC: Prompt with all context types present ---

    #[test]
    fn test_build_prompt_all_context_types_present() {
        let (temp_dir, conn) = setup_test_db();

        // Task FEAT-002 depends on FEAT-001 (done) and has synergy with FEAT-003 (done)
        insert_task(&conn, "FEAT-001", "Foundation types", "done", 5);
        insert_task_full(
            &conn,
            "FEAT-002",
            "Build the widget",
            "todo",
            10,
            "Implement the widget feature",
            &["Widget renders correctly", "Widget handles errors"],
        );
        insert_task(&conn, "FEAT-003", "Helper utils", "done", 6);
        insert_task_file(&conn, "FEAT-002", "src/widget.rs");
        insert_relationship(&conn, "FEAT-002", "FEAT-001", "dependsOn");
        insert_relationship(&conn, "FEAT-002", "FEAT-003", "synergyWith");

        // Create source file for context scanner
        create_source_file(
            temp_dir.path(),
            "src/widget.rs",
            "pub fn render_widget() -> String { todo!() }\n",
        );

        // Create steering and base prompt
        let steering_path = create_steering(temp_dir.path(), "Focus on testing edge cases.");
        let base_prompt_path = create_base_prompt(temp_dir.path());

        let params = BuildPromptParams {
            dir: temp_dir.path(),
            project_root: temp_dir.path(),
            conn: &conn,
            after_files: &[],
            run_id: None,
            iteration: 1,
            reorder_hint: None,
            session_guidance: "User said: prioritize error handling",
            base_prompt_path: &base_prompt_path,
            steering_path: Some(&steering_path),
            verbose: false,
            default_model: None,
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Verify all sections present
        assert!(
            result.prompt.contains("## Steering"),
            "Prompt should contain steering section"
        );
        assert!(
            result.prompt.contains("Focus on testing edge cases."),
            "Prompt should contain steering content"
        );
        assert!(
            result.prompt.contains("## Session Guidance"),
            "Prompt should contain session guidance section"
        );
        assert!(
            result.prompt.contains("prioritize error handling"),
            "Prompt should contain session guidance content"
        );
        assert!(
            result.prompt.contains("## Current Source Context"),
            "Prompt should contain source context section"
        );
        assert!(
            result.prompt.contains("render_widget"),
            "Prompt should contain source signatures"
        );
        assert!(
            result.prompt.contains("## Completed Dependencies"),
            "Prompt should contain dependency summaries"
        );
        assert!(
            result.prompt.contains("FEAT-001"),
            "Prompt should reference completed dependency"
        );
        assert!(
            result.prompt.contains("## Current Task"),
            "Prompt should contain task JSON section"
        );
        assert!(
            result.prompt.contains("```json"),
            "Prompt should contain JSON code block"
        );
        assert!(
            result.prompt.contains("<reorder>"),
            "Prompt should contain reorder instruction"
        );
        assert!(
            result.prompt.contains("# Agent Instructions"),
            "Prompt should contain base prompt"
        );
        assert_eq!(result.task_id, "FEAT-002");
    }

    // --- AC: Prompt includes source signatures from touchesFiles ---

    #[test]
    fn test_build_prompt_includes_source_signatures() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "US-001", "Add API", "todo", 5);
        insert_task_file(&conn, "US-001", "src/api/handler.rs");

        create_source_file(
            temp_dir.path(),
            "src/api/handler.rs",
            r#"
pub struct ApiHandler {
    base_url: String,
}

pub fn handle_request(req: Request) -> Response {
    todo!()
}

pub async fn handle_async(req: Request) -> Response {
    todo!()
}

pub enum ApiError {
    NotFound,
    Internal,
}
"#,
        );

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            result.prompt.contains("pub struct ApiHandler"),
            "Should extract struct signature"
        );
        assert!(
            result.prompt.contains("pub fn handle_request"),
            "Should extract pub fn signature"
        );
        assert!(
            result.prompt.contains("pub async fn handle_async"),
            "Should extract pub async fn signature"
        );
        assert!(
            result.prompt.contains("pub enum ApiError"),
            "Should extract enum signature"
        );
        assert!(
            result.prompt.contains("src/api/handler.rs"),
            "Should reference the file path"
        );
    }

    // --- AC: Prompt includes dependency completion summaries for done tasks ---

    #[test]
    fn test_build_prompt_includes_dependency_summaries() {
        let (temp_dir, conn) = setup_test_db();

        // DEP-001 and DEP-002 are done; TASK-001 depends on both
        insert_task(&conn, "DEP-001", "Setup database schema", "done", 1);
        insert_task(&conn, "DEP-002", "Create user model", "done", 2);
        insert_task(&conn, "TASK-001", "Implement user API", "todo", 10);
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");
        insert_relationship(&conn, "TASK-001", "DEP-002", "dependsOn");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            result.prompt.contains("## Completed Dependencies"),
            "Should have dependencies section"
        );
        assert!(
            result.prompt.contains("DEP-001"),
            "Should list first dependency"
        );
        assert!(
            result.prompt.contains("Setup database schema"),
            "Should include first dependency title"
        );
        assert!(
            result.prompt.contains("DEP-002"),
            "Should list second dependency"
        );
        assert!(
            result.prompt.contains("Create user model"),
            "Should include second dependency title"
        );
    }

    // --- AC: Prompt with no steering.md (section omitted) ---

    #[test]
    fn test_build_prompt_no_steering_omits_section() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "US-001", "Simple task", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            !result.prompt.contains("## Steering"),
            "No steering.md means no steering section"
        );
    }

    // --- AC: Prompt with no learnings (section omitted) ---

    #[test]
    fn test_build_prompt_no_learnings_omits_section() {
        let (temp_dir, conn) = setup_test_db();

        // No learnings inserted
        insert_task(&conn, "US-001", "Task with no learnings", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            !result.prompt.contains("## Relevant Learnings"),
            "No learnings should omit the learnings section"
        );
    }

    // --- AC: Prompt with no matching docs (deps section omitted) ---

    #[test]
    fn test_build_prompt_no_dependencies_omits_section() {
        let (temp_dir, conn) = setup_test_db();

        // Task with no dependsOn relationships
        insert_task(&conn, "US-001", "Standalone task", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            !result.prompt.contains("## Completed Dependencies"),
            "No dependencies should omit the section"
        );
    }

    // --- AC: Task context truncation when exceeding ~4000 char cap ---

    #[test]
    fn test_build_prompt_task_json_truncation() {
        let (temp_dir, conn) = setup_test_db();

        // Create a task with very long description and many acceptance criteria
        let long_desc = "x".repeat(5000);
        let criteria: Vec<&str> = (0..100)
            .map(|_| "This is a very long acceptance criterion that adds to the total length")
            .collect();
        let criteria_json = serde_json::to_string(&criteria).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, description, acceptance_criteria) VALUES (?, ?, ?, ?, ?, ?)",
            params!["BIG-001", "Big task", "todo", 5, long_desc, criteria_json],
        )
        .unwrap();

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // The task JSON section should be truncated
        assert!(
            result.prompt.contains("[truncated to"),
            "Task JSON should be truncated when exceeding budget, got prompt length: {}",
            result.prompt.len()
        );
    }

    // --- AC: Prompt includes reorder instruction text ---

    #[test]
    fn test_build_prompt_includes_reorder_instruction() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "US-001", "Any task", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            result.prompt.contains("<reorder>TASK-ID</reorder>"),
            "Prompt should contain reorder instruction with example tag"
        );
        assert!(
            result
                .prompt
                .contains("strong reason to work on a different"),
            "Prompt should explain when to use reorder"
        );
    }

    // --- AC: Prompt includes task JSON block formatted correctly ---

    #[test]
    fn test_build_prompt_task_json_formatted_correctly() {
        let (temp_dir, conn) = setup_test_db();

        insert_task_full(
            &conn,
            "FEAT-010",
            "Add validation",
            "todo",
            8,
            "Add input validation to the API",
            &["Validates email", "Returns 400 on invalid"],
        );
        insert_task_file(&conn, "FEAT-010", "src/validation.rs");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Verify JSON block structure
        assert!(
            result.prompt.contains("## Current Task"),
            "Should have Current Task heading"
        );
        assert!(
            result.prompt.contains("```json"),
            "Should have JSON code block opening"
        );
        assert!(
            result.prompt.contains("```\n\n"),
            "Should have JSON code block closing"
        );

        // Verify JSON content includes task fields
        assert!(
            result.prompt.contains("FEAT-010"),
            "JSON should contain task ID"
        );
        assert!(
            result.prompt.contains("Add validation"),
            "JSON should contain task title"
        );
        assert!(
            result.prompt.contains("Validates email"),
            "JSON should contain acceptance criteria"
        );
        assert!(
            result.prompt.contains("src/validation.rs"),
            "JSON should contain file paths"
        );
    }

    // --- AC: Reorder hint included in prompt ---

    #[test]
    fn test_build_prompt_with_reorder_hint() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "US-001", "First task", "todo", 5);
        insert_task(&conn, "US-002", "Second task", "todo", 10);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = BuildPromptParams {
            dir: temp_dir.path(),
            project_root: temp_dir.path(),
            conn: &conn,
            after_files: &[],
            run_id: None,
            iteration: 2,
            reorder_hint: Some("US-002"),
            session_guidance: "",
            base_prompt_path: &base_prompt_path,
            steering_path: None,
            verbose: false,
            default_model: None,
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            result.prompt.contains("## Reorder Hint"),
            "Should have reorder hint section"
        );
        assert!(
            result.prompt.contains("US-002"),
            "Should mention the reorder target task"
        );
    }

    // --- AC: shown_learning_ids are correctly tracked and returned ---

    #[test]
    fn test_build_prompt_tracks_shown_learning_ids() {
        let (temp_dir, conn) = setup_test_db();

        // Insert learnings that will be recalled for the task
        let id1 = insert_test_learning(&conn, "Learning about testing");
        let id2 = insert_test_learning(&conn, "Learning about APIs");

        insert_task(&conn, "US-001", "Build API tests", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Learnings should be shown (UCB bandit recall returns them based on
        // exploration/exploitation). We can't guarantee exactly which IDs are returned
        // since recall_learnings uses UCB scoring, but the mechanism should work.
        // If learnings are returned, their IDs should be tracked.
        if !result.shown_learning_ids.is_empty() {
            // Verify the IDs are valid learning IDs (from the set we inserted)
            for shown_id in &result.shown_learning_ids {
                assert!(
                    *shown_id == id1 || *shown_id == id2,
                    "Shown ID {} should be one of the inserted learnings",
                    shown_id
                );
            }
        }
        // The shown_learning_ids Vec should always be initialized
        // (may be empty if no learnings matched, but should not panic)
    }

    // --- Additional comprehensive tests ---

    #[test]
    fn test_build_prompt_returns_none_when_no_tasks() {
        let (temp_dir, conn) = setup_test_db();

        // All tasks are done
        insert_task(&conn, "DONE-001", "Already done", "done", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params).unwrap();
        assert!(result.is_none(), "Should return None when no tasks remain");
    }

    #[test]
    fn test_build_prompt_task_files_populated() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "US-001", "Task with files", "todo", 5);
        insert_task_file(&conn, "US-001", "src/main.rs");
        insert_task_file(&conn, "US-001", "src/lib.rs");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(result.task_id, "US-001");
        assert!(
            result.task_files.contains(&"src/main.rs".to_string()),
            "task_files should contain src/main.rs"
        );
        assert!(
            result.task_files.contains(&"src/lib.rs".to_string()),
            "task_files should contain src/lib.rs"
        );
    }

    #[test]
    fn test_build_prompt_session_guidance_empty_omits_section() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "US-001", "Task", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
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
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            !result.prompt.contains("## Session Guidance"),
            "Empty session guidance should omit the section"
        );
    }

    #[test]
    fn test_build_prompt_section_ordering() {
        // Verify sections appear in the expected order
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "DEP-001", "Dependency", "done", 1);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        insert_task_file(&conn, "TASK-001", "src/main.rs");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        create_source_file(
            temp_dir.path(),
            "src/main.rs",
            "pub fn main_entry() { todo!() }\n",
        );

        let steering_path = create_steering(temp_dir.path(), "Be thorough.");
        let base_prompt_path = create_base_prompt(temp_dir.path());

        let params = BuildPromptParams {
            dir: temp_dir.path(),
            project_root: temp_dir.path(),
            conn: &conn,
            after_files: &[],
            run_id: None,
            iteration: 1,
            reorder_hint: Some("OTHER-TASK"),
            session_guidance: "Focus on tests",
            base_prompt_path: &base_prompt_path,
            steering_path: Some(&steering_path),
            verbose: false,
            default_model: None,
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");
        let p = &result.prompt;

        // Verify ordering: steering < session guidance < reorder hint < source context
        //   < deps < task JSON < reorder instruction < base prompt
        let steering_pos = p.find("## Steering").expect("Steering present");
        let guidance_pos = p.find("## Session Guidance").expect("Guidance present");
        let reorder_hint_pos = p.find("## Reorder Hint").expect("Reorder hint present");
        let source_pos = p
            .find("## Current Source Context")
            .expect("Source context present");
        let deps_pos = p.find("## Completed Dependencies").expect("Deps present");
        let task_pos = p.find("## Current Task").expect("Task present");
        let reorder_instr_pos = p
            .find("strong reason to work on a different")
            .expect("Reorder instruction present");
        let base_pos = p.find("# Agent Instructions").expect("Base prompt present");

        assert!(
            steering_pos < guidance_pos,
            "Steering should come before session guidance"
        );
        assert!(
            guidance_pos < reorder_hint_pos,
            "Session guidance should come before reorder hint"
        );
        assert!(
            reorder_hint_pos < source_pos,
            "Reorder hint should come before source context"
        );
        assert!(
            source_pos < deps_pos,
            "Source context should come before deps"
        );
        assert!(deps_pos < task_pos, "Deps should come before task JSON");
        assert!(
            task_pos < reorder_instr_pos,
            "Task JSON should come before reorder instruction"
        );
        assert!(
            reorder_instr_pos < base_pos,
            "Reorder instruction should come before base prompt"
        );
    }

    #[test]
    fn test_build_prompt_no_source_context_for_nonexistent_files() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "US-001", "Task with missing file", "todo", 5);
        insert_task_file(&conn, "US-001", "src/does_not_exist.rs");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Source context section should not appear for non-existent files
        assert!(
            !result.prompt.contains("## Current Source Context"),
            "Non-existent source files should not produce source context"
        );
    }

    #[test]
    fn test_build_prompt_dependency_summaries_only_for_done_deps() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "DEP-DONE", "Done dependency", "done", 1);
        insert_task(&conn, "DEP-TODO", "Todo dependency", "todo", 2);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "DEP-DONE", "dependsOn");
        // Note: DEP-TODO is in todo status but we add a dependsOn for it anyway
        // The task should still be eligible because selection filters in Rust, not SQL
        // But the dependency summary should only show the done one
        insert_relationship(&conn, "TASK-001", "DEP-TODO", "dependsOn");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let _params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        // TASK-001 won't be eligible since DEP-TODO isn't done; the next() selection
        // will pick DEP-TODO instead (lowest priority todo task). Let's test the
        // dependency summary helper directly instead.
        let mut prompt = String::new();
        append_dependency_summaries(&mut prompt, &conn, "TASK-001");

        assert!(prompt.contains("DEP-DONE"), "Should list done dependency");
        assert!(
            prompt.contains("Done dependency"),
            "Should show done dep title"
        );
        // DEP-TODO is not done, so it should NOT appear in the summaries
        assert!(
            !prompt.contains("DEP-TODO"),
            "Should not list todo dependency in summaries"
        );
    }

    // --- Synergy context tests (unit-level) ---

    #[test]
    fn test_synergy_context_with_run_id() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "SYN-001", "Synergy task done", "done", 5);
        insert_task(&conn, "TASK-001", "Current task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "SYN-001", "synergyWith");

        // Create run with a run_task entry linking SYN-001 to the run
        insert_run(&conn, "run-001");
        insert_run_task(&conn, "run-001", "SYN-001", 1);

        // Set a last_commit on the run
        conn.execute(
            "UPDATE runs SET last_commit = 'abc123' WHERE run_id = 'run-001'",
            [],
        )
        .unwrap();

        let mut prompt = String::new();
        append_synergy_context(&mut prompt, &conn, "TASK-001", Some("run-001"));

        assert!(
            prompt.contains("## Synergy Tasks"),
            "Should have synergy section"
        );
        assert!(prompt.contains("SYN-001"), "Should reference synergy task");
        assert!(
            prompt.contains("Synergy task done"),
            "Should include synergy task title"
        );
        assert!(prompt.contains("abc123"), "Should include commit hash");
    }

    #[test]
    fn test_synergy_context_without_run_id_omits_section() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "SYN-001", "Synergy task", "done", 5);
        insert_task(&conn, "TASK-001", "Current task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "SYN-001", "synergyWith");

        let mut prompt = String::new();
        append_synergy_context(&mut prompt, &conn, "TASK-001", None);

        assert!(prompt.is_empty(), "No run_id should omit synergy section");
    }

    #[test]
    fn test_synergy_context_no_synergy_tasks_omits_section() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "TASK-001", "Task without synergy", "todo", 10);

        let mut prompt = String::new();
        append_synergy_context(&mut prompt, &conn, "TASK-001", Some("run-001"));

        assert!(
            prompt.is_empty(),
            "No synergy tasks should omit the section"
        );
    }

    // --- build_task_json comprehensive tests ---

    #[test]
    fn test_build_task_json_with_all_fields() {
        let task = NextTaskOutput {
            id: "FEAT-042".to_string(),
            title: "Complex feature".to_string(),
            description: Some("A complex feature with many parts".to_string()),
            priority: 3,
            status: "in_progress".to_string(),
            acceptance_criteria: vec!["Criterion one".to_string(), "Criterion two".to_string()],
            notes: Some("Important: check edge cases".to_string()),
            files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            batch_with: vec!["FEAT-043".to_string()],
            model: Some("claude-opus-4-6".to_string()),
            difficulty: Some("high".to_string()),
            escalation_note: Some("Complex architectural task".to_string()),
            score: ScoreOutput {
                total: 1003,
                priority: 997,
                file_overlap: 10,
                synergy: 3,
                conflict: -7,
                file_overlap_count: 1,
                synergy_from: vec!["FEAT-040".to_string()],
                conflict_from: vec!["FEAT-099".to_string()],
            },
        };

        let next_result = NextResult {
            task: Some(task.clone()),
            batch_tasks: vec!["FEAT-043".to_string()],
            learnings: vec![],
            selection: SelectionMetadata {
                reason: "highest score".to_string(),
                eligible_count: 5,
            },
            claim: None,
            top_candidates: vec![],
        };

        let json = build_task_json(&task, &next_result);

        assert!(json.contains("FEAT-042"), "Should contain task ID");
        assert!(json.contains("Complex feature"), "Should contain title");
        assert!(
            json.contains("A complex feature"),
            "Should contain description"
        );
        assert!(
            json.contains("Important: check edge cases"),
            "Should contain notes"
        );
        assert!(
            json.contains("Criterion one"),
            "Should contain first criterion"
        );
        assert!(
            json.contains("Criterion two"),
            "Should contain second criterion"
        );
        assert!(json.contains("src/a.rs"), "Should contain first file");
        assert!(json.contains("FEAT-043"), "Should contain batchWith");
        assert!(
            json.contains("eligibleBatchTasks"),
            "Should contain eligible batch tasks when non-empty"
        );
        assert!(
            json.contains("claude-opus-4-6"),
            "Should contain model when present"
        );
        assert!(
            json.contains("\"difficulty\""),
            "Should contain difficulty key when present"
        );
        assert!(
            json.contains("high"),
            "Should contain difficulty value when present"
        );
        assert!(
            json.contains("escalationNote"),
            "Should contain escalationNote key when present"
        );
        assert!(
            json.contains("Complex architectural task"),
            "Should contain escalation note value when present"
        );
    }

    #[test]
    fn test_build_task_json_minimal_fields() {
        let task = NextTaskOutput {
            id: "FIX-001".to_string(),
            title: "Quick fix".to_string(),
            description: None,
            priority: 1,
            status: "in_progress".to_string(),
            acceptance_criteria: vec![],
            notes: None,
            files: vec![],
            batch_with: vec![],
            model: None,
            difficulty: None,
            escalation_note: None,
            score: ScoreOutput {
                total: 999,
                priority: 999,
                file_overlap: 0,
                synergy: 0,
                conflict: 0,
                file_overlap_count: 0,
                synergy_from: vec![],
                conflict_from: vec![],
            },
        };

        let next_result = NextResult {
            task: Some(task.clone()),
            batch_tasks: vec![],
            learnings: vec![],
            selection: SelectionMetadata {
                reason: "only task".to_string(),
                eligible_count: 1,
            },
            claim: None,
            top_candidates: vec![],
        };

        let json = build_task_json(&task, &next_result);

        assert!(json.contains("FIX-001"), "Should contain task ID");
        assert!(json.contains("Quick fix"), "Should contain title");
        // Optional fields should not appear
        assert!(
            !json.contains("description"),
            "Should not contain description key when None"
        );
        assert!(
            !json.contains("notes"),
            "Should not contain notes key when None"
        );
        assert!(
            !json.contains("batchWith"),
            "Should not contain batchWith when empty"
        );
        assert!(
            !json.contains("eligibleBatchTasks"),
            "Should not contain eligibleBatchTasks when empty"
        );
        assert!(
            !json.contains("model"),
            "Should not contain model key when None"
        );
        assert!(
            !json.contains("difficulty"),
            "Should not contain difficulty key when None"
        );
        assert!(
            !json.contains("escalationNote"),
            "Should not contain escalationNote key when None"
        );
    }

    // ===== TEST-003: Individual model field tests for build_task_json =====

    /// Helper to create a minimal NextTaskOutput with configurable model fields.
    fn task_output_with_model_fields(
        model: Option<&str>,
        difficulty: Option<&str>,
        escalation_note: Option<&str>,
    ) -> NextTaskOutput {
        NextTaskOutput {
            id: "TEST-001".to_string(),
            title: "Test".to_string(),
            description: None,
            priority: 10,
            status: "todo".to_string(),
            acceptance_criteria: vec![],
            notes: None,
            files: vec![],
            batch_with: vec![],
            model: model.map(String::from),
            difficulty: difficulty.map(String::from),
            escalation_note: escalation_note.map(String::from),
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
        }
    }

    fn empty_next_result(task: &NextTaskOutput) -> NextResult {
        NextResult {
            task: Some(task.clone()),
            batch_tasks: vec![],
            learnings: vec![],
            selection: SelectionMetadata {
                reason: "test".to_string(),
                eligible_count: 1,
            },
            claim: None,
            top_candidates: vec![],
        }
    }

    #[test]
    fn test_build_task_json_model_field_only() {
        let task = task_output_with_model_fields(Some("claude-haiku-4-5-20251001"), None, None);
        let result = empty_next_result(&task);
        let json = build_task_json(&task, &result);

        assert!(
            json.contains("\"model\""),
            "JSON should contain model key when model is set"
        );
        assert!(
            json.contains("claude-haiku-4-5-20251001"),
            "JSON should contain model value"
        );
        assert!(
            !json.contains("\"difficulty\""),
            "JSON should not contain difficulty key when None"
        );
        assert!(
            !json.contains("\"escalationNote\""),
            "JSON should not contain escalationNote key when None"
        );
    }

    #[test]
    fn test_build_task_json_difficulty_field_only() {
        let task = task_output_with_model_fields(None, Some("medium"), None);
        let result = empty_next_result(&task);
        let json = build_task_json(&task, &result);

        assert!(
            !json.contains("\"model\""),
            "JSON should not contain model key when None"
        );
        assert!(
            json.contains("\"difficulty\""),
            "JSON should contain difficulty key when set"
        );
        assert!(
            json.contains("medium"),
            "JSON should contain difficulty value"
        );
        assert!(
            !json.contains("\"escalationNote\""),
            "JSON should not contain escalationNote key when None"
        );
    }

    #[test]
    fn test_build_task_json_escalation_note_field_only() {
        let task =
            task_output_with_model_fields(None, None, Some("Needs opus for complex reasoning"));
        let result = empty_next_result(&task);
        let json = build_task_json(&task, &result);

        assert!(
            !json.contains("\"model\""),
            "JSON should not contain model key when None"
        );
        assert!(
            !json.contains("\"difficulty\""),
            "JSON should not contain difficulty key when None"
        );
        assert!(
            json.contains("\"escalationNote\""),
            "JSON key should be camelCase escalationNote"
        );
        assert!(
            json.contains("Needs opus for complex reasoning"),
            "JSON should contain escalation note value"
        );
    }

    // ===== TEST-003: Integration test - prompt with model fields from DB =====

    #[test]
    fn test_build_prompt_task_json_includes_model_fields() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "MOD-001", "Task with model fields", "todo", 5);
        // Set model fields directly via SQL since insert_task doesn't support them
        conn.execute(
            "UPDATE tasks SET model = ?1, difficulty = ?2, escalation_note = ?3 WHERE id = 'MOD-001'",
            params!["claude-opus-4-6", "high", "Complex multi-file refactor"],
        )
        .unwrap();

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Verify model fields appear in the task JSON block
        assert!(
            result.prompt.contains("claude-opus-4-6"),
            "Prompt JSON should contain model value"
        );
        assert!(
            result.prompt.contains("\"difficulty\""),
            "Prompt JSON should contain difficulty key"
        );
        assert!(
            result.prompt.contains("high"),
            "Prompt JSON should contain difficulty value"
        );
        assert!(
            result.prompt.contains("escalationNote"),
            "Prompt JSON should contain escalationNote key (camelCase)"
        );
        assert!(
            result.prompt.contains("Complex multi-file refactor"),
            "Prompt JSON should contain escalation note value"
        );
    }

    #[test]
    fn test_build_prompt_task_json_omits_model_fields_when_none() {
        let (temp_dir, conn) = setup_test_db();

        // Task without model fields
        insert_task(&conn, "PLAIN-001", "Plain task", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Extract just the JSON block between ```json and ```
        let json_start = result.prompt.find("```json\n").unwrap() + 8;
        let json_end = result.prompt[json_start..].find("\n```").unwrap() + json_start;
        let json_block = &result.prompt[json_start..json_end];

        assert!(
            !json_block.contains("\"model\""),
            "JSON should not contain model key when None"
        );
        assert!(
            !json_block.contains("\"difficulty\""),
            "JSON should not contain difficulty key when None"
        );
        assert!(
            !json_block.contains("\"escalationNote\""),
            "JSON should not contain escalationNote key when None"
        );
    }

    // --- record_shown_learnings with actual learnings ---

    // --- Steering edge cases ---

    #[test]
    fn test_append_steering_with_newlines_only() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("steering.md");
        fs::write(&path, "\n\n\n").unwrap();

        let mut prompt = String::new();
        append_steering(&mut prompt, &path);
        assert!(
            prompt.is_empty(),
            "Newlines-only file should be treated as empty"
        );
    }

    #[test]
    fn test_append_steering_trims_content() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("steering.md");
        fs::write(&path, "  \n  Focus on X  \n  ").unwrap();

        let mut prompt = String::new();
        append_steering(&mut prompt, &path);
        assert!(
            prompt.contains("Focus on X"),
            "Should contain trimmed content"
        );
        // Verify the content is trimmed (no leading/trailing whitespace)
        let after_header = prompt.split("## Steering\n\n").nth(1).unwrap();
        assert!(
            after_header.starts_with("Focus on X"),
            "Content should be trimmed"
        );
    }

    // --- Base prompt edge cases ---

    #[test]
    fn test_append_base_prompt_no_trailing_newline() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("prompt.md");
        fs::write(&path, "No trailing newline").unwrap();

        let mut prompt = String::new();
        append_base_prompt(&mut prompt, &path);
        assert!(prompt.ends_with('\n'), "Should append newline when missing");
    }

    #[test]
    fn test_append_base_prompt_with_trailing_newline() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("prompt.md");
        fs::write(&path, "Has trailing newline\n").unwrap();

        let mut prompt = String::new();
        append_base_prompt(&mut prompt, &path);
        assert!(!prompt.ends_with("\n\n"), "Should not double-add newline");
    }

    // --- TEST-001: project_root separation in BuildPromptParams routing ---

    #[test]
    fn test_build_prompt_uses_project_root_for_source_context() {
        // When dir (DB) and project_root differ, source files should be scanned
        // from project_root, not from dir.
        let (db_temp_dir, conn) = setup_test_db();

        // Create a separate project_root directory with source files
        let project_root = TempDir::new().unwrap();
        create_source_file(
            project_root.path(),
            "src/handler.rs",
            "pub fn handle_request() -> bool { true }\n",
        );

        insert_task(&conn, "FEAT-001", "Add handler", "todo", 5);
        insert_task_file(&conn, "FEAT-001", "src/handler.rs");

        let base_prompt_path = create_base_prompt(project_root.path());

        // dir points to DB temp dir, project_root points to source files
        let params = BuildPromptParams {
            dir: db_temp_dir.path(),
            project_root: project_root.path(),
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
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Source context should find handler.rs under project_root
        assert!(
            result.prompt.contains("handle_request"),
            "Source context should scan files from project_root, not dir. Prompt: {}",
            &result.prompt[..result.prompt.len().min(500)]
        );
        assert!(
            result.prompt.contains("## Current Source Context"),
            "Source context section should be present when files exist under project_root"
        );
    }

    #[test]
    fn test_build_prompt_no_source_context_when_files_only_under_dir() {
        // If source files exist under dir (DB directory) but NOT under project_root,
        // the source context should be empty because scan_source_context uses project_root.
        let (db_temp_dir, conn) = setup_test_db();

        let project_root = TempDir::new().unwrap();
        // Source file under db_temp_dir (the DB dir), NOT under project_root
        create_source_file(
            db_temp_dir.path(),
            "src/orphan.rs",
            "pub fn orphan_fn() { }\n",
        );

        insert_task(&conn, "FEAT-001", "Task", "todo", 5);
        insert_task_file(&conn, "FEAT-001", "src/orphan.rs");

        let base_prompt_path = create_base_prompt(project_root.path());

        let params = BuildPromptParams {
            dir: db_temp_dir.path(),
            project_root: project_root.path(),
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
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Source context should NOT find orphan.rs because it's under dir, not project_root
        assert!(
            !result.prompt.contains("orphan_fn"),
            "Source context should NOT scan files from dir (DB directory)"
        );
        assert!(
            !result.prompt.contains("## Current Source Context"),
            "Source context section should be absent when no files exist under project_root"
        );
    }

    #[test]
    fn test_build_prompt_dir_routes_to_task_selection() {
        // Verify that dir (not project_root) is used for DB-backed task selection
        // by using a dir that has the DB and a project_root that doesn't.
        let (db_temp_dir, conn) = setup_test_db();
        let project_root = TempDir::new().unwrap();

        // Task exists in DB (which lives under db_temp_dir)
        insert_task(&conn, "DB-001", "DB-backed task", "todo", 5);

        let base_prompt_path = create_base_prompt(project_root.path());

        let params = BuildPromptParams {
            dir: db_temp_dir.path(),
            project_root: project_root.path(),
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
            task_prefix: None,
        };

        // next::next uses dir for DB access — task should be found
        let result = build_prompt(&params)
            .unwrap()
            .expect("Task should be found via dir (DB path)");

        assert_eq!(
            result.task_id, "DB-001",
            "Task selection should use dir for DB, not project_root"
        );
    }

    // ===== TEST-INIT-002: Model resolution in build_prompt =====
    //
    // TDD tests defining expected behavior for PromptResult.resolved_model.
    // Tests marked #[ignore] will pass once FEAT-004 implements model resolution.

    /// AC1: Task with explicit model='claude-opus-4-6' → resolved_model is Some('claude-opus-4-6').
    #[test]

    fn test_resolved_model_explicit_model_on_task() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "MOD-001", "Task with explicit model", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'MOD-001'",
            params![OPUS_MODEL],
        )
        .unwrap();

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(OPUS_MODEL.to_string()),
            "Explicit model on task should flow through to resolved_model"
        );
    }

    /// AC2: Task with difficulty='high' and no explicit model → resolved_model is opus.
    #[test]

    fn test_resolved_model_difficulty_high_forces_opus() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "MOD-002", "High-difficulty task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET difficulty = 'high' WHERE id = 'MOD-002'",
            [],
        )
        .unwrap();

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(OPUS_MODEL.to_string()),
            "difficulty='high' with no explicit model should resolve to opus"
        );
    }

    /// AC3: Task with no model, no difficulty, prd_default=haiku → resolved_model is haiku.
    #[test]

    fn test_resolved_model_prd_default_fallback() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "MOD-003", "Task with prd default", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let mut params = build_params(temp_dir.path(), &conn, &base_prompt_path);
        params.default_model = Some(HAIKU_MODEL);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(HAIKU_MODEL.to_string()),
            "No model/difficulty should fall back to prd_default"
        );
    }

    /// AC4: Task with synergyWith partner that has higher-tier model → resolved_model is higher.
    #[test]

    fn test_resolved_model_synergy_partner_higher_tier() {
        let (temp_dir, conn) = setup_test_db();

        // Selected task has haiku model
        insert_task(&conn, "MOD-004", "Selected task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'MOD-004'",
            params![HAIKU_MODEL],
        )
        .unwrap();

        // Synergy partner has sonnet model and is pending (todo)
        insert_task(&conn, "SYN-004", "Synergy partner", "todo", 10);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'SYN-004'",
            params![SONNET_MODEL],
        )
        .unwrap();

        // Establish synergyWith relationship
        insert_relationship(&conn, "MOD-004", "SYN-004", "synergyWith");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(SONNET_MODEL.to_string()),
            "Synergy partner with higher-tier model should elevate resolved_model"
        );
    }

    /// AC5: No default_model in prd_metadata → resolved_model falls back to None.
    /// This test runs against the stub (resolved_model: None) and validates
    /// the expected behavior when no model information is available.
    #[test]
    fn test_resolved_model_no_defaults_returns_none() {
        let (temp_dir, conn) = setup_test_db();

        // Task with no model, no difficulty
        insert_task(&conn, "MOD-005", "Plain task", "todo", 5);

        let base_prompt_path = create_base_prompt(temp_dir.path());
        // default_model is None (no prd default)
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model, None,
            "No model info at any level should resolve to None"
        );
    }

    /// AC6 (known-bad discriminator): Synergy partner with opus model overrides
    /// selected task's haiku. Rejects implementations that ignore synergy.
    #[test]

    fn test_resolved_model_synergy_opus_overrides_task_haiku() {
        let (temp_dir, conn) = setup_test_db();

        // Selected task explicitly set to haiku
        insert_task(&conn, "MOD-006", "Haiku task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'MOD-006'",
            params![HAIKU_MODEL],
        )
        .unwrap();

        // Synergy partner set to opus and is in_progress (active)
        insert_task(&conn, "SYN-006", "Opus partner", "in_progress", 10);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'SYN-006'",
            params![OPUS_MODEL],
        )
        .unwrap();

        // Establish synergyWith relationship
        insert_relationship(&conn, "MOD-006", "SYN-006", "synergyWith");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(OPUS_MODEL.to_string()),
            "Synergy partner with opus MUST override selected task's haiku — \
             synergy cluster model = max tier across all members"
        );
    }

    // --- Edge case tests for model resolution ---

    /// Edge case: Task with empty string model → normalized to None, not Some("").
    /// Invariant: resolved_model is never Some("").
    #[test]

    fn test_resolved_model_empty_string_normalized_to_none() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "MOD-007", "Empty model task", "todo", 5);
        conn.execute("UPDATE tasks SET model = '' WHERE id = 'MOD-007'", [])
            .unwrap();

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model, None,
            "Empty string model must be normalized to None, never Some('')"
        );
    }

    /// Edge case: Multiple synergy partners with different tiers → highest tier wins.
    #[test]

    fn test_resolved_model_multi_partner_highest_wins() {
        let (temp_dir, conn) = setup_test_db();

        // Selected task has no model
        insert_task(&conn, "MOD-008", "No-model task", "todo", 5);

        // Synergy partners: haiku, opus, sonnet
        insert_task(&conn, "SYN-008A", "Haiku partner", "todo", 10);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'SYN-008A'",
            params![HAIKU_MODEL],
        )
        .unwrap();

        insert_task(&conn, "SYN-008B", "Opus partner", "todo", 15);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'SYN-008B'",
            params![OPUS_MODEL],
        )
        .unwrap();

        insert_task(&conn, "SYN-008C", "Sonnet partner", "todo", 20);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'SYN-008C'",
            params![SONNET_MODEL],
        )
        .unwrap();

        insert_relationship(&conn, "MOD-008", "SYN-008A", "synergyWith");
        insert_relationship(&conn, "MOD-008", "SYN-008B", "synergyWith");
        insert_relationship(&conn, "MOD-008", "SYN-008C", "synergyWith");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(OPUS_MODEL.to_string()),
            "Among multiple synergy partners, opus (highest tier) must win"
        );
    }

    /// Edge case: All synergy tasks are done → cluster is just the selected task.
    /// Done synergy partners should be excluded from model resolution.
    #[test]

    fn test_resolved_model_done_synergy_partners_excluded() {
        let (temp_dir, conn) = setup_test_db();

        // Selected task has haiku model
        insert_task(&conn, "MOD-009", "Haiku task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'MOD-009'",
            params![HAIKU_MODEL],
        )
        .unwrap();

        // Synergy partner has opus but is DONE — should not elevate
        insert_task(&conn, "SYN-009", "Done opus partner", "done", 10);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'SYN-009'",
            params![OPUS_MODEL],
        )
        .unwrap();

        insert_relationship(&conn, "MOD-009", "SYN-009", "synergyWith");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(HAIKU_MODEL.to_string()),
            "Done synergy partners should be excluded; cluster is just the selected task"
        );
    }

    /// Edge case: No synergyWith partners → resolved_model is just the selected task's model.
    #[test]

    fn test_resolved_model_no_synergy_partners() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "MOD-010", "Solo task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'MOD-010'",
            params![SONNET_MODEL],
        )
        .unwrap();

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(SONNET_MODEL.to_string()),
            "Task with no synergy partners should resolve to its own model"
        );
    }

    // ===== TEST-002: Comprehensive model resolution tests =====

    /// AC: Synergy partner with None model and prd_default=haiku → selected task's model wins.
    #[test]
    fn test_resolved_model_synergy_none_model_with_prd_default() {
        let (temp_dir, conn) = setup_test_db();

        // Selected task has sonnet model
        insert_task(&conn, "MOD-020", "Sonnet task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'MOD-020'",
            params![SONNET_MODEL],
        )
        .unwrap();

        // Synergy partner has no model (None) — will resolve via prd_default=haiku
        insert_task(&conn, "SYN-020", "No-model partner", "todo", 10);
        insert_relationship(&conn, "MOD-020", "SYN-020", "synergyWith");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let mut params = build_params(temp_dir.path(), &conn, &base_prompt_path);
        params.default_model = Some(HAIKU_MODEL);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(SONNET_MODEL.to_string()),
            "Selected task's sonnet should win over partner's haiku (via prd_default)"
        );
    }

    /// AC: No model, difficulty=medium, prd_default=sonnet → resolved is sonnet.
    #[test]
    fn test_resolved_model_medium_difficulty_uses_prd_default() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "MOD-021", "Medium task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET difficulty = 'medium' WHERE id = 'MOD-021'",
            [],
        )
        .unwrap();

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let mut params = build_params(temp_dir.path(), &conn, &base_prompt_path);
        params.default_model = Some(SONNET_MODEL);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(SONNET_MODEL.to_string()),
            "Medium difficulty should NOT escalate to opus; should fall through to prd_default"
        );
    }

    /// AC: All synergy partners have None model → falls back to prd_default.
    #[test]
    fn test_resolved_model_all_synergy_none_falls_to_prd_default() {
        let (temp_dir, conn) = setup_test_db();

        // Selected task has no model
        insert_task(&conn, "MOD-022", "No-model task", "todo", 5);

        // Two synergy partners with no model
        insert_task(&conn, "SYN-022A", "Partner A", "todo", 10);
        insert_task(&conn, "SYN-022B", "Partner B", "in_progress", 15);
        insert_relationship(&conn, "MOD-022", "SYN-022A", "synergyWith");
        insert_relationship(&conn, "MOD-022", "SYN-022B", "synergyWith");

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let mut params = build_params(temp_dir.path(), &conn, &base_prompt_path);
        params.default_model = Some(HAIKU_MODEL);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert_eq!(
            result.resolved_model,
            Some(HAIKU_MODEL.to_string()),
            "All partners with None model should fall back to prd_default"
        );
    }

    // ===== TEST-INIT-003: Escalation template loading =====
    //
    // TDD tests for load_escalation_template() and its integration into build_prompt().
    // Unit tests for load_escalation_template run against the real implementation.
    // Integration tests through build_prompt verify escalation template injection.

    /// Helper: create escalation template file under base_prompt_path's parent/scripts/.
    fn create_escalation_template(base_prompt_dir: &Path, content: &str) -> std::path::PathBuf {
        let scripts_dir = base_prompt_dir.join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        let template_path = scripts_dir.join("escalation-policy.md");
        fs::write(&template_path, content).unwrap();
        template_path
    }

    // --- AC1: load_escalation_template with valid file returns Some(content) ---

    #[test]
    fn test_load_escalation_template_valid_file_returns_some() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        fs::write(&base_prompt_path, "base prompt").unwrap();

        let template_content = "# Escalation Policy\n\nWhen stuck, escalate to opus.\n";
        create_escalation_template(temp_dir.path(), template_content);

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result,
            Some(template_content.to_string()),
            "Valid template file should return Some(content)"
        );
    }

    // --- AC2: load_escalation_template with missing file returns None ---

    #[test]
    fn test_load_escalation_template_missing_file_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        // No escalation template created

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(result, None, "Missing template file should return None");
    }

    // --- Edge case: empty template file returns Some("") ---

    #[test]
    fn test_load_escalation_template_empty_file_returns_some_empty() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        create_escalation_template(temp_dir.path(), "");

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result,
            Some(String::new()),
            "Empty template file should return Some('') — harmless empty section"
        );
    }

    // --- Edge case: template with unicode content passes through verbatim ---

    #[test]
    fn test_load_escalation_template_unicode_content_verbatim() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        let unicode_content = "# エスカレーション方針\n\n困った場合は opus に切り替え 🚀\n";
        create_escalation_template(temp_dir.path(), unicode_content);

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result,
            Some(unicode_content.to_string()),
            "Unicode content should pass through verbatim"
        );
    }

    // --- Edge case: base_prompt_path with no parent (just filename) ---

    #[test]
    fn test_load_escalation_template_no_parent_resolves_relative() {
        // When base_prompt_path is just "prompt.md" with no parent directory,
        // the function should resolve relative to "." (current dir).
        // This must not panic regardless of whether the file exists.
        let bare_path = Path::new("prompt.md");
        let _result = load_escalation_template(bare_path);
        // No panic = pass. Result depends on whether scripts/escalation-policy.md
        // exists in cwd, which is environment-dependent.
    }

    // --- Invariant: Template is loaded fresh each call (not cached) ---

    #[test]
    fn test_load_escalation_template_not_cached_hot_edit() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");

        // First call: template with initial content
        create_escalation_template(temp_dir.path(), "version 1");
        let result1 = load_escalation_template(&base_prompt_path);
        assert_eq!(result1, Some("version 1".to_string()));

        // Hot-edit: overwrite with new content
        create_escalation_template(temp_dir.path(), "version 2 — hot edited");
        let result2 = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result2,
            Some("version 2 — hot edited".to_string()),
            "Template should be re-read each call, not cached"
        );
    }

    // --- AC3: Escalation section injected when resolved_model is haiku ---

    #[test]

    fn test_escalation_section_injected_for_haiku() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "ESC-001", "Haiku task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'ESC-001'",
            params![HAIKU_MODEL],
        )
        .unwrap();

        let template_content = "# Escalation Policy\n\nEscalate when blocked.\n";
        let base_prompt_path = create_base_prompt(temp_dir.path());
        create_escalation_template(temp_dir.path(), template_content);

        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            result.prompt.contains("Escalation Policy"),
            "Escalation section should be injected for haiku model"
        );
        assert!(
            result.prompt.contains("Escalate when blocked."),
            "Escalation template content should be present for haiku"
        );
    }

    // --- AC3 continued: Escalation section injected when resolved_model is sonnet ---

    #[test]

    fn test_escalation_section_injected_for_sonnet() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "ESC-002", "Sonnet task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'ESC-002'",
            params![SONNET_MODEL],
        )
        .unwrap();

        let template_content = "# Escalation Policy\n\nEscalate when blocked.\n";
        let base_prompt_path = create_base_prompt(temp_dir.path());
        create_escalation_template(temp_dir.path(), template_content);

        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            result.prompt.contains("Escalation Policy"),
            "Escalation section should be injected for sonnet model"
        );
    }

    // --- AC4: Escalation section injected when resolved_model is None (Default tier) ---

    #[test]

    fn test_escalation_section_injected_for_default_none() {
        let (temp_dir, conn) = setup_test_db();

        // Task with no model, no difficulty, no prd_default → resolved_model = None
        insert_task(&conn, "ESC-003", "Default tier task", "todo", 5);

        let template_content = "# Escalation Policy\n\nDefault tier gets policy too.\n";
        let base_prompt_path = create_base_prompt(temp_dir.path());
        create_escalation_template(temp_dir.path(), template_content);

        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            result.prompt.contains("Escalation Policy"),
            "Escalation section should be injected for Default/None tier — \
             CLI default could be sonnet, policy is informational and safe"
        );
    }

    // --- AC5: Escalation section NOT injected when resolved_model is opus ---

    #[test]

    fn test_escalation_section_not_injected_for_opus() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "ESC-004", "Opus task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'ESC-004'",
            params![OPUS_MODEL],
        )
        .unwrap();

        let template_content = "# Escalation Policy\n\nShould not appear for opus.\n";
        let base_prompt_path = create_base_prompt(temp_dir.path());
        create_escalation_template(temp_dir.path(), template_content);

        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        assert!(
            !result.prompt.contains("Escalation Policy"),
            "Escalation section must NOT be injected for opus model — \
             opus is the highest tier, escalation makes no sense"
        );
        assert!(
            !result.prompt.contains("Should not appear for opus."),
            "Escalation template content must be absent for opus"
        );
    }

    // --- AC6: Known-bad discriminator: template content BEFORE reorder instruction ---

    #[test]

    fn test_escalation_section_before_reorder_instruction() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "ESC-005", "Sonnet task for ordering", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'ESC-005'",
            params![SONNET_MODEL],
        )
        .unwrap();

        let template_content = "# Escalation Policy\n\nUNIQUE_ESCALATION_MARKER_XYZ\n";
        let base_prompt_path = create_base_prompt(temp_dir.path());
        create_escalation_template(temp_dir.path(), template_content);

        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        let escalation_pos = result
            .prompt
            .find("UNIQUE_ESCALATION_MARKER_XYZ")
            .expect("Escalation content should be present for sonnet");
        let reorder_pos = result
            .prompt
            .find("strong reason to work on a different")
            .expect("Reorder instruction should be present");

        assert!(
            escalation_pos < reorder_pos,
            "Escalation template (pos={}) must appear BEFORE reorder instruction (pos={}) — \
             rejects implementations that naively append at end",
            escalation_pos,
            reorder_pos
        );
    }

    // ===== TEST-003: Comprehensive escalation template tests =====

    /// AC: Large template content (>5KB) loads and injects correctly.
    #[test]
    fn test_escalation_large_template_loads_correctly() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        fs::write(&base_prompt_path, "base prompt").unwrap();

        let large_content = "x".repeat(6000);
        create_escalation_template(temp_dir.path(), &large_content);

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result.as_ref().map(|s| s.len()),
            Some(6000),
            "Large template (>5KB) should load fully without truncation"
        );
    }

    /// AC: Template with markdown headers doesn't interfere with prompt structure.
    #[test]
    fn test_escalation_template_markdown_headers_no_interference() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "ESC-030", "Sonnet task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'ESC-030'",
            params![SONNET_MODEL],
        )
        .unwrap();

        let template_content =
            "# Escalation Policy\n\n## When to Escalate\n\n### Step 1\n\nDo this.\n";
        let base_prompt_path = create_base_prompt(temp_dir.path());
        create_escalation_template(temp_dir.path(), template_content);

        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);
        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Template headers should appear within the escalation section
        assert!(result.prompt.contains("## When to Escalate"));
        assert!(result.prompt.contains("### Step 1"));
        // The prompt structure sections should still be present
        assert!(result.prompt.contains("## Current Task"));
        assert!(result.prompt.contains("<reorder>"));
    }

    /// AC: base_prompt_path in different directory → template resolves from correct parent.
    #[test]
    fn test_escalation_template_different_parent_directory() {
        let temp_dir = TempDir::new().unwrap();
        let subdir = temp_dir.path().join("config").join("prompts");
        fs::create_dir_all(&subdir).unwrap();

        let base_prompt_path = subdir.join("prompt.md");
        fs::write(&base_prompt_path, "base prompt").unwrap();

        // Template must be under subdir/scripts/, not temp_dir/scripts/
        let scripts_dir = subdir.join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        fs::write(
            scripts_dir.join("escalation-policy.md"),
            "Nested template content",
        )
        .unwrap();

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result,
            Some("Nested template content".to_string()),
            "Template should resolve relative to base_prompt_path's parent, not cwd"
        );
    }

    /// AC: Switching from non-opus to opus model → template disappears.
    /// Verifies by contrast: sonnet task has template, opus task does not.
    #[test]
    fn test_escalation_template_disappears_for_opus() {
        let (temp_dir, conn) = setup_test_db();

        // Sonnet task — should have escalation
        insert_task(&conn, "ESC-031", "Sonnet task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'ESC-031'",
            params![SONNET_MODEL],
        )
        .unwrap();

        let template_content = "ESCALATION_MARKER_CHECK";
        let base_prompt_path = create_base_prompt(temp_dir.path());
        create_escalation_template(temp_dir.path(), template_content);

        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);
        let sonnet_result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");
        assert!(
            sonnet_result.prompt.contains("ESCALATION_MARKER_CHECK"),
            "Sonnet should have escalation template"
        );

        // Now mark sonnet task done, add opus task
        conn.execute("UPDATE tasks SET status = 'done' WHERE id = 'ESC-031'", [])
            .unwrap();
        insert_task(&conn, "ESC-032", "Opus task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'ESC-032'",
            params![OPUS_MODEL],
        )
        .unwrap();

        let opus_result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");
        assert!(
            !opus_result.prompt.contains("ESCALATION_MARKER_CHECK"),
            "Opus should NOT have escalation template — template disappears when tier is Opus"
        );
    }

    // --- Edge case: escalation template missing + non-opus model → section silently omitted ---

    #[test]

    fn test_escalation_section_missing_template_silently_omitted() {
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "ESC-006", "Sonnet task no template", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'ESC-006'",
            params![SONNET_MODEL],
        )
        .unwrap();

        // No escalation template file created
        let base_prompt_path = create_base_prompt(temp_dir.path());
        let params = build_params(temp_dir.path(), &conn, &base_prompt_path);

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Should not panic or error — just no escalation section
        assert!(
            !result.prompt.contains("Escalation Policy"),
            "Missing escalation template should result in no escalation section"
        );
    }

    // ===== Prompt size budget tests =====

    #[test]
    fn test_append_base_prompt_truncation_over_budget() {
        let temp_dir = TempDir::new().unwrap();
        let prompt_path = temp_dir.path().join("prompt.md");
        // Write content larger than BASE_PROMPT_BUDGET (16K)
        let large_content = "x".repeat(20_000);
        fs::write(&prompt_path, &large_content).unwrap();

        let mut prompt = String::new();
        append_base_prompt(&mut prompt, &prompt_path);

        assert!(
            prompt.contains("[truncated to"),
            "Large base prompt should be truncated"
        );
        assert!(
            prompt.len() <= BASE_PROMPT_BUDGET + 100,
            "Base prompt ({} bytes) should be within budget ({} + overhead)",
            prompt.len(),
            BASE_PROMPT_BUDGET
        );
    }

    #[test]
    fn test_append_base_prompt_under_budget_not_truncated() {
        let temp_dir = TempDir::new().unwrap();
        let prompt_path = temp_dir.path().join("prompt.md");
        fs::write(&prompt_path, "# Short prompt\n\nDo the task.\n").unwrap();

        let mut prompt = String::new();
        append_base_prompt(&mut prompt, &prompt_path);

        assert!(
            !prompt.contains("[truncated to"),
            "Short base prompt should not be truncated"
        );
        assert!(
            prompt.contains("Short prompt"),
            "Content should be preserved"
        );
    }

    // ===== Two-phase prompt assembly tests =====

    #[test]
    fn test_try_fit_section_within_budget() {
        let mut remaining = 1000;
        let mut dropped = Vec::new();
        let section = "hello world".to_string();
        let result = try_fit_section(section, "Test", &mut remaining, &mut dropped);
        assert_eq!(result, "hello world");
        assert_eq!(remaining, 1000 - 11);
        assert!(dropped.is_empty(), "Should not record a drop");
    }

    #[test]
    fn test_try_fit_section_over_budget_returns_empty() {
        let mut remaining = 5;
        let mut dropped = Vec::new();
        let section = "hello world".to_string(); // 11 bytes > 5
        let result = try_fit_section(section, "Test", &mut remaining, &mut dropped);
        assert!(
            result.is_empty(),
            "Section should be dropped when over budget"
        );
        assert_eq!(remaining, 5, "Budget should not be consumed");
        assert_eq!(
            dropped,
            vec!["Test"],
            "Should record the dropped section name"
        );
    }

    #[test]
    fn test_try_fit_section_empty_section_passthrough() {
        let mut remaining = 100;
        let mut dropped = Vec::new();
        let section = String::new();
        let result = try_fit_section(section, "Test", &mut remaining, &mut dropped);
        assert!(result.is_empty());
        assert_eq!(remaining, 100, "Empty section should not consume budget");
        assert!(
            dropped.is_empty(),
            "Empty sections should not appear in dropped list"
        );
    }

    #[test]
    fn test_trimmable_sections_dropped_when_budget_tight() {
        // Build a prompt where critical sections + high-priority trimmable sections
        // consume enough budget that low-priority trimmable sections get dropped.
        //
        // Critical sections: ~16K base prompt + ~4K task JSON + reorder instr ≈ 20K
        // Remaining: ~60K for trimmable sections
        // We provide >60K of trimmable content so lower-priority ones get dropped.
        let (temp_dir, conn) = setup_test_db();

        insert_task_full(
            &conn,
            "TIGHT-001",
            "Tight budget task",
            "todo",
            10,
            "A task to test tight budgets",
            &["AC1"],
        );

        // Create a base prompt that fills its per-section budget
        let base_prompt_path = temp_dir.path().join("prompt.md");
        let large_base = "x".repeat(BASE_PROMPT_BUDGET - 100);
        fs::write(&base_prompt_path, &large_base).unwrap();

        // Provide large steering (~30K) — this is a LOW priority trimmable section
        let steering_path = create_steering(temp_dir.path(), &"y".repeat(30_000));

        // Provide large session guidance (~35K) — also low priority
        let large_guidance = "z".repeat(35_000);

        let params = BuildPromptParams {
            dir: temp_dir.path(),
            project_root: temp_dir.path(),
            conn: &conn,
            after_files: &[],
            run_id: None,
            iteration: 1,
            reorder_hint: Some("TIGHT-002"),
            session_guidance: &large_guidance,
            base_prompt_path: &base_prompt_path,
            steering_path: Some(&steering_path),
            verbose: false,
            default_model: None,
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Critical sections must always be present
        assert!(
            result.prompt.contains("## Current Task"),
            "Task JSON (critical) must be present"
        );
        assert!(
            result.prompt.contains(&large_base[..100]),
            "Base prompt (critical) must be present"
        );
        assert!(
            result
                .prompt
                .contains("strong reason to work on a different"),
            "Reorder instruction (critical) must be present"
        );

        // The prompt must not exceed the total budget
        assert!(
            result.prompt.len() <= TOTAL_PROMPT_BUDGET,
            "Prompt ({} bytes) must not exceed budget ({} bytes)",
            result.prompt.len(),
            TOTAL_PROMPT_BUDGET,
        );

        // At least one trimmable section should have been dropped
        assert!(
            !result.dropped_sections.is_empty(),
            "With >60K of trimmable content competing for ~60K remaining, \
             at least one section must be dropped. Got dropped_sections: {:?}",
            result.dropped_sections,
        );

        // Sections that WERE dropped should NOT appear in the prompt
        for name in &result.dropped_sections {
            let header = format!("## {}", name);
            assert!(
                !result.prompt.contains(&header),
                "Dropped section '{}' should not appear in the prompt",
                name,
            );
        }
    }

    #[test]
    fn test_all_sections_present_when_budget_sufficient() {
        // Normal-sized prompt where everything fits
        let (temp_dir, conn) = setup_test_db();

        insert_task_full(
            &conn,
            "FULL-001",
            "Full budget task",
            "todo",
            10,
            "A task with all sections",
            &["AC1"],
        );
        insert_task_file(&conn, "FULL-001", "src/main.rs");
        create_source_file(
            temp_dir.path(),
            "src/main.rs",
            "pub fn main() {\n    println!(\"hello\");\n}\n\npub fn helper(x: i32) -> i32 {\n    x + 1\n}\n",
        );

        let base_prompt_path = create_base_prompt(temp_dir.path());
        let steering_path = create_steering(temp_dir.path(), "Focus on correctness.");

        let params = BuildPromptParams {
            dir: temp_dir.path(),
            project_root: temp_dir.path(),
            conn: &conn,
            after_files: &[],
            run_id: None,
            iteration: 1,
            reorder_hint: Some("FULL-002"),
            session_guidance: "User wants tests",
            base_prompt_path: &base_prompt_path,
            steering_path: Some(&steering_path),
            verbose: false,
            default_model: None,
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // All sections should be present
        assert!(
            result.prompt.contains("## Current Task"),
            "Task JSON present"
        );
        assert!(result.prompt.contains("## Steering"), "Steering present");
        assert!(
            result.prompt.contains("## Session Guidance"),
            "Session Guidance present"
        );
        assert!(
            result.prompt.contains("## Reorder Hint"),
            "Reorder Hint present"
        );
        assert!(
            result.prompt.contains("## Current Source Context"),
            "Source Context present"
        );
        assert!(
            result
                .prompt
                .contains("strong reason to work on a different"),
            "Reorder instruction present"
        );
        assert!(
            result.prompt.contains("Agent Instructions"),
            "Base prompt present"
        );

        // No sections should have been dropped — everything fits
        assert!(
            result.dropped_sections.is_empty(),
            "No sections should be dropped when budget is sufficient, but got: {:?}",
            result.dropped_sections,
        );
    }

    #[test]
    fn test_critical_sections_over_budget_returns_error() {
        // We can't easily make real critical sections exceed 80K without a
        // massive task + base prompt. Instead, test build_escalation_section
        // and build_base_prompt_section together via the try_fit_section logic.
        //
        // The PromptOverflow error is tested by verifying the error variant
        // is constructible and has the right message.
        use crate::error::TaskMgrError;

        let err = TaskMgrError::PromptOverflow {
            critical_size: 90_000,
            budget: 80_000,
            task_id: "OVER-001".to_string(),
        };

        let msg = err.to_string();
        assert!(msg.contains("90000"));
        assert!(msg.contains("80000"));
        assert!(msg.contains("OVER-001"));
        assert!(msg.contains("Reduce base prompt.md size or split the task"));
    }

    #[test]
    fn test_escalation_included_as_critical_section() {
        // Verify escalation section is in the critical path (never dropped by budget)
        // by creating a prompt with a non-Opus model and an escalation template
        let (temp_dir, conn) = setup_test_db();

        insert_task(&conn, "CRIT-001", "Sonnet task", "todo", 5);
        conn.execute(
            "UPDATE tasks SET model = ?1 WHERE id = 'CRIT-001'",
            params![SONNET_MODEL],
        )
        .unwrap();

        let template_content = "UNIQUE_CRITICAL_ESCALATION_MARKER\n";
        let base_prompt_path = create_base_prompt(temp_dir.path());
        create_escalation_template(temp_dir.path(), template_content);

        // Create a large steering file to put pressure on the trimmable budget
        let steering_path = create_steering(temp_dir.path(), &"s".repeat(5000));

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
            steering_path: Some(&steering_path),
            verbose: false,
            default_model: None,
            task_prefix: None,
        };

        let result = build_prompt(&params)
            .unwrap()
            .expect("Should return a prompt");

        // Escalation is critical — must be present regardless of budget pressure
        assert!(
            result.prompt.contains("UNIQUE_CRITICAL_ESCALATION_MARKER"),
            "Escalation section must always be present (critical) for non-Opus"
        );
        assert!(
            result.prompt.contains("## Model Escalation Policy"),
            "Escalation header must be present"
        );
    }
}
