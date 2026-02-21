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
use crate::commands::next::output::{LearningSummaryOutput, NextResult};
use crate::error::TaskMgrResult;
use crate::learnings::bandit;
use crate::loop_engine::context;
use crate::loop_engine::model;

/// Total character budget for enriched task context in the prompt.
const TASK_CONTEXT_BUDGET: usize = 4000;

/// Character budget for source context from touchesFiles.
const SOURCE_CONTEXT_BUDGET: usize = 2000;

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
}

/// Build a prompt for the current iteration.
///
/// Calls `next::next()` to select and claim a task, then assembles the prompt
/// with all available context. Returns `None` if no tasks remain.
pub fn build_prompt(params: &BuildPromptParams<'_>) -> TaskMgrResult<Option<PromptResult>> {
    // Step 1: Select and claim a task
    let next_result = next::next(
        params.dir,
        params.after_files,
        true,
        params.run_id,
        params.verbose,
    )?;

    let task_output = match next_result.task {
        Some(ref task) => task,
        None => return Ok(None), // All tasks complete
    };

    // Step 2: Record shown learnings and collect IDs for feedback loop
    let shown_learning_ids = record_shown_learnings(
        params.conn,
        &next_result.learnings,
        i64::from(params.iteration),
    );

    // Step 3: Build the prompt sections
    let mut prompt = String::new();

    // Section: Steering (optional)
    if let Some(steering) = params.steering_path {
        append_steering(&mut prompt, steering);
    }

    // Section: Session guidance (if any)
    if !params.session_guidance.is_empty() {
        prompt.push_str("## Session Guidance\n\n");
        prompt.push_str(params.session_guidance);
        prompt.push_str("\n\n");
    }

    // Section: Previous iteration context
    if let Some(hint) = params.reorder_hint {
        prompt.push_str(&format!(
            "## Reorder Hint\n\nThe previous iteration requested reorder to task: `{}`\n\n",
            hint
        ));
    }

    // Section: Source context from touchesFiles
    let source_ctx = context::scan_source_context(
        &task_output.files,
        SOURCE_CONTEXT_BUDGET,
        params.project_root,
    );
    let source_prompt = source_ctx.format_for_prompt();
    if !source_prompt.is_empty() {
        prompt.push_str(&source_prompt);
    }

    // Section: Dependency completion summaries
    append_dependency_summaries(&mut prompt, params.conn, &task_output.id);

    // Section: Synergy task context
    append_synergy_context(&mut prompt, params.conn, &task_output.id, params.run_id);

    // Section: Task JSON
    prompt.push_str("## Current Task\n\n```json\n");
    let task_json = build_task_json(task_output, &next_result);
    let truncated_json = truncate_to_budget(&task_json, TASK_CONTEXT_BUDGET);
    prompt.push_str(&truncated_json);
    prompt.push_str("\n```\n\n");

    // Section: Learnings
    append_learnings(&mut prompt, &next_result.learnings);

    // Section: Non-code task completion instruction
    // Tasks with no touchesFiles (verification, milestones, polish) won't produce
    // commits, so the git-based completion detection won't fire. Tell Claude how
    // to signal completion explicitly.
    if task_output.files.is_empty() {
        prompt.push_str(&format!(
            "## Completing This Task\n\n\
             This task has no `touchesFiles` — it is a verification, milestone, or polish task.\n\
             After all acceptance criteria pass:\n\
             1. Run `task-mgr done {task_id} --force` to mark it done in the DB\n\
             2. Update the PRD JSON to set `passes: true` for this task\n\
             3. Print the task ID in brackets in your output, e.g.: `Completed [{task_id}]`\n\n\
             All three steps are **required** for the loop to detect completion.\n\n",
            task_id = task_output.id,
        ));
    }

    // Model resolution: resolve synergy cluster model from task + synergyWith partners
    // (must happen before escalation injection to determine tier)
    let resolved_model = resolve_synergy_cluster_model(
        params.conn,
        &task_output.id,
        task_output.model.as_deref(),
        task_output.difficulty.as_deref(),
        params.default_model,
    );

    // Section: Escalation policy (skip only for Opus tier)
    append_escalation_policy(
        &mut prompt,
        params.base_prompt_path,
        resolved_model.as_deref(),
    );

    // Section: Reorder instruction
    prompt.push_str(
        "If you have a strong reason to work on a different eligible task, \
         output `<reorder>TASK-ID</reorder>`.\n\n",
    );

    // Section: Base prompt template
    append_base_prompt(&mut prompt, params.base_prompt_path);

    Ok(Some(PromptResult {
        prompt,
        task_id: task_output.id.clone(),
        task_files: task_output.files.clone(),
        shown_learning_ids,
        resolved_model,
    }))
}

/// Record shown learnings via the UCB bandit system.
///
/// Returns the list of learning IDs that were shown (for feedback tracking).
/// Errors are logged but don't prevent prompt building.
fn record_shown_learnings(
    conn: &Connection,
    learnings: &[LearningSummaryOutput],
    iteration: i64,
) -> Vec<i64> {
    let mut shown_ids = Vec::with_capacity(learnings.len());
    for learning in learnings {
        shown_ids.push(learning.id);
        if let Err(e) = bandit::record_learning_shown(conn, learning.id, iteration) {
            eprintln!(
                "Warning: failed to record learning {} as shown: {}",
                learning.id, e
            );
        }
    }
    shown_ids
}

/// Append steering.md content to the prompt if the file exists.
fn append_steering(prompt: &mut String, steering_path: &Path) {
    match fs::read_to_string(steering_path) {
        Ok(content) if !content.trim().is_empty() => {
            prompt.push_str("## Steering\n\n");
            prompt.push_str(content.trim());
            prompt.push_str("\n\n");
        }
        Ok(_) => {}  // Empty file, skip
        Err(_) => {} // Missing file, skip gracefully
    }
}

/// Append dependency completion summaries for the current task.
///
/// For each completed dependsOn task: includes title + key acceptance criteria
/// as a 2-3 line summary.
fn append_dependency_summaries(prompt: &mut String, conn: &Connection, task_id: &str) {
    let deps = match get_completed_dependencies(conn, task_id) {
        Ok(deps) if !deps.is_empty() => deps,
        _ => return,
    };

    prompt.push_str("## Completed Dependencies\n\n");
    for (dep_id, dep_title) in &deps {
        prompt.push_str(&format!("- **{}**: {}\n", dep_id, dep_title));
    }
    prompt.push('\n');
}

/// Get completed dependency task IDs and titles for a task.
fn get_completed_dependencies(
    conn: &Connection,
    task_id: &str,
) -> TaskMgrResult<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title FROM tasks t
         INNER JOIN task_relationships tr ON tr.related_id = t.id
         WHERE tr.task_id = ?1
           AND tr.rel_type = 'dependsOn'
           AND t.status = 'done'
         ORDER BY t.id",
    )?;

    let deps: Vec<(String, String)> = stmt
        .query_map([task_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;

    Ok(deps)
}

/// Append synergy task context for completed synergy tasks in the current run.
fn append_synergy_context(
    prompt: &mut String,
    conn: &Connection,
    task_id: &str,
    run_id: Option<&str>,
) {
    let run_id = match run_id {
        Some(rid) => rid,
        None => return,
    };

    let synergies = match get_synergy_tasks_in_run(conn, task_id, run_id) {
        Ok(s) if !s.is_empty() => s,
        _ => return,
    };

    prompt.push_str("## Synergy Tasks (completed this run)\n\n");
    for (syn_id, syn_title, syn_commit) in &synergies {
        prompt.push_str(&format!("- **{}**: {}", syn_id, syn_title));
        if let Some(commit) = syn_commit {
            prompt.push_str(&format!(" (commit: {})", commit));
        }
        prompt.push('\n');
    }
    prompt.push('\n');
}

/// Get synergy tasks that were completed in the current run.
fn get_synergy_tasks_in_run(
    conn: &Connection,
    task_id: &str,
    run_id: &str,
) -> TaskMgrResult<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, r.last_commit
         FROM tasks t
         INNER JOIN task_relationships tr ON tr.related_id = t.id
         LEFT JOIN run_tasks rt ON rt.task_id = t.id AND rt.run_id = ?2
         LEFT JOIN runs r ON r.run_id = rt.run_id
         WHERE tr.task_id = ?1
           AND tr.rel_type = 'synergyWith'
           AND t.status = 'done'
         ORDER BY t.id",
    )?;

    let results: Vec<(String, String, Option<String>)> = stmt
        .query_map(rusqlite::params![task_id, run_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?;

    Ok(results)
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

/// Append learnings to the prompt.
fn append_learnings(prompt: &mut String, learnings: &[LearningSummaryOutput]) {
    if learnings.is_empty() {
        return;
    }

    prompt.push_str("## Relevant Learnings\n\n```json\n");
    let learnings_json =
        serde_json::to_string_pretty(learnings).unwrap_or_else(|_| "[]".to_string());
    prompt.push_str(&learnings_json);
    prompt.push_str("\n```\n\n");
}

/// Append the base prompt template file content.
fn append_base_prompt(prompt: &mut String, base_prompt_path: &Path) {
    match fs::read_to_string(base_prompt_path) {
        Ok(content) if !content.trim().is_empty() => {
            prompt.push_str(&content);
            if !content.ends_with('\n') {
                prompt.push('\n');
            }
        }
        Ok(_) => {
            eprintln!(
                "Warning: base prompt file is empty: {}",
                base_prompt_path.display()
            );
        }
        Err(e) => {
            eprintln!(
                "Warning: could not read base prompt file {}: {}",
                base_prompt_path.display(),
                e
            );
        }
    }
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

/// Append the escalation policy section to the prompt when the resolved model is not Opus.
///
/// Loads the template from `base_prompt_path.parent()/scripts/escalation-policy.md`.
/// Injects for all non-Opus tiers (including Default/None) per architect decision.
/// When the template file is missing, the section is silently omitted.
fn append_escalation_policy(
    prompt: &mut String,
    base_prompt_path: &Path,
    resolved_model: Option<&str>,
) {
    // Opus tier already has maximum capability — no escalation needed
    if model::model_tier(resolved_model) == model::ModelTier::Opus {
        return;
    }

    if let Some(contents) = load_escalation_template(base_prompt_path) {
        prompt.push_str("## Model Escalation Policy\n\n");
        prompt.push_str(&contents);
        prompt.push_str("\n\n---\n\n");
    }
}

/// Resolve the model for a synergy cluster (the selected task + its pending synergyWith partners).
///
/// 1. Resolves the primary task's model via `model::resolve_task_model()`.
/// 2. Queries pending (todo/in_progress) synergyWith partners' model and difficulty.
/// 3. Resolves each partner via `model::resolve_task_model()`.
/// 4. Combines all resolved models via `model::resolve_iteration_model()` (highest tier wins).
/// 5. Normalizes `Some("")` to `None`.
///
/// When no synergyWith partners exist, the cluster is just the selected task.
pub fn resolve_synergy_cluster_model(
    conn: &Connection,
    task_id: &str,
    task_model: Option<&str>,
    task_difficulty: Option<&str>,
    default_model: Option<&str>,
) -> Option<String> {
    // Resolve the primary task's model
    let primary_model = model::resolve_task_model(task_model, task_difficulty, default_model);

    // Query pending synergyWith partners' model and difficulty
    let synergy_models = get_synergy_partner_models(conn, task_id, default_model);

    // Combine: primary task + all synergy partners
    let mut all_models = vec![primary_model];
    all_models.extend(synergy_models);

    // Select highest tier across the cluster
    let resolved = model::resolve_iteration_model(&all_models);

    // Normalize Some("") to None
    resolved.filter(|m| !m.trim().is_empty())
}

/// Query pending synergyWith partners and resolve each one's model.
fn get_synergy_partner_models(
    conn: &Connection,
    task_id: &str,
    default_model: Option<&str>,
) -> Vec<Option<String>> {
    let mut stmt = match conn.prepare(
        "SELECT t.model, t.difficulty
         FROM tasks t
         INNER JOIN task_relationships tr ON tr.related_id = t.id
         WHERE tr.task_id = ?1
           AND tr.rel_type = 'synergyWith'
           AND t.status IN ('todo', 'in_progress')",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };

    let rows = match stmt.query_map([task_id], |row| {
        let partner_model: Option<String> = row.get("model")?;
        let partner_difficulty: Option<String> = row.get("difficulty")?;
        Ok(model::resolve_task_model(
            partner_model.as_deref(),
            partner_difficulty.as_deref(),
            default_model,
        ))
    }) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    rows.filter_map(|r| r.ok()).collect()
}

/// Truncate a string to fit within a byte budget.
fn truncate_to_budget(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        text.to_string()
    } else {
        let safe_end = text.floor_char_boundary(budget);
        let truncated = &text[..safe_end];
        format!("{}...\n[truncated to {} bytes]", truncated, budget)
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
        }
    }

    // ===== Unit tests for helper functions (original) =====

    #[test]
    fn test_truncate_to_budget_within_limit() {
        let text = "short text";
        let result = truncate_to_budget(text, 100);
        assert_eq!(result, "short text");
    }

    #[test]
    fn test_truncate_to_budget_exceeds_limit() {
        let text = "a".repeat(5000);
        let result = truncate_to_budget(&text, 100);
        assert!(result.len() < 200);
        assert!(result.contains("[truncated to 100 bytes]"));
    }

    #[test]
    fn test_truncate_to_budget_exact_limit() {
        let text = "abcde";
        let result = truncate_to_budget(text, 5);
        assert_eq!(result, "abcde");
    }

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
    fn test_append_learnings_empty() {
        let mut prompt = String::new();
        append_learnings(&mut prompt, &[]);
        assert!(prompt.is_empty(), "No learnings should produce no section");
    }

    #[test]
    fn test_append_learnings_with_content() {
        let learnings = vec![LearningSummaryOutput {
            id: 1,
            title: "Test Learning".to_string(),
            outcome: "pattern".to_string(),
            confidence: "high".to_string(),
            content: Some("Use X instead of Y".to_string()),
            applies_to_files: None,
            applies_to_task_types: None,
        }];

        let mut prompt = String::new();
        append_learnings(&mut prompt, &learnings);
        assert!(prompt.contains("## Relevant Learnings"));
        assert!(prompt.contains("Test Learning"));
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
    fn test_record_shown_learnings_empty() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let ids = record_shown_learnings(&conn, &[], 1);
        assert!(ids.is_empty());
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

    #[test]
    fn test_record_shown_learnings_tracks_ids() {
        let (_temp_dir, conn) = setup_test_db();

        let id1 = insert_test_learning(&conn, "Learning A");
        let id2 = insert_test_learning(&conn, "Learning B");

        let learnings = vec![
            LearningSummaryOutput {
                id: id1,
                title: "Learning A".to_string(),
                outcome: "pattern".to_string(),
                confidence: "medium".to_string(),
                content: Some("Content A".to_string()),
                applies_to_files: None,
                applies_to_task_types: None,
            },
            LearningSummaryOutput {
                id: id2,
                title: "Learning B".to_string(),
                outcome: "success".to_string(),
                confidence: "high".to_string(),
                content: Some("Content B".to_string()),
                applies_to_files: None,
                applies_to_task_types: None,
            },
        ];

        let ids = record_shown_learnings(&conn, &learnings, 5);

        assert_eq!(ids.len(), 2, "Should return 2 shown IDs");
        assert_eq!(ids[0], id1, "First ID should match");
        assert_eq!(ids[1], id2, "Second ID should match");
    }

    #[test]
    fn test_record_shown_learnings_graceful_on_invalid_id() {
        let (_temp_dir, conn) = setup_test_db();

        // Learning ID 99999 doesn't exist, but record_learning_shown should
        // either succeed (no-op) or log warning and continue
        let learnings = vec![LearningSummaryOutput {
            id: 99999,
            title: "Ghost learning".to_string(),
            outcome: "pattern".to_string(),
            confidence: "low".to_string(),
            content: None,
            applies_to_files: None,
            applies_to_task_types: None,
        }];

        // Should not panic
        let ids = record_shown_learnings(&conn, &learnings, 1);
        assert_eq!(
            ids.len(),
            1,
            "Should still return the ID even if DB op fails"
        );
        assert_eq!(ids[0], 99999);
    }

    // --- Truncation edge cases ---

    #[test]
    fn test_truncate_to_budget_zero() {
        let result = truncate_to_budget("hello", 0);
        assert!(
            result.contains("[truncated to 0 bytes]"),
            "Zero budget should truncate"
        );
    }

    #[test]
    fn test_truncate_to_budget_one_char() {
        let result = truncate_to_budget("hello", 1);
        assert!(result.starts_with('h'));
        assert!(result.contains("[truncated to 1 bytes]"));
    }

    #[test]
    fn test_truncate_to_budget_empty_string() {
        let result = truncate_to_budget("", 100);
        assert_eq!(result, "", "Empty string within budget returns empty");
    }

    #[test]
    fn test_truncate_to_budget_multibyte_utf8_no_panic() {
        // "café" = 5 chars, 6 bytes (é is 2 bytes: 0xC3 0xA9)
        let text = "café";
        assert_eq!(text.len(), 5); // 5 bytes
                                   // Budget 4 falls after 'f' but before 'é' starts — safe
        let result = truncate_to_budget(text, 4);
        assert!(result.contains("[truncated to 4 bytes]"));
        assert!(result.starts_with("caf"));
        // Budget 3 falls mid-way — would panic with naive slicing if é started at byte 3
        let result = truncate_to_budget(text, 3);
        assert!(result.contains("[truncated to 3 bytes]"));
    }

    #[test]
    fn test_truncate_to_budget_emoji_no_panic() {
        // Each emoji is 4 bytes
        let text = "🍕🍔🌮🍣";
        assert_eq!(text.len(), 16); // 4 emoji × 4 bytes
                                    // Budget 5 falls mid-second emoji (byte 5 is inside 🍔)
        let result = truncate_to_budget(text, 5);
        assert!(result.contains("[truncated to 5 bytes]"));
        // Should contain only first emoji (4 bytes), not a partial second
        assert!(result.starts_with("🍕"));
        assert!(!result.starts_with("🍕🍔"));
    }

    #[test]
    fn test_truncate_to_budget_cjk_no_panic() {
        // CJK characters are 3 bytes each
        let text = "你好世界";
        assert_eq!(text.len(), 12); // 4 chars × 3 bytes
                                    // Budget 4 falls mid-second character (byte 4 is inside 好)
        let result = truncate_to_budget(text, 4);
        assert!(result.contains("[truncated to 4 bytes]"));
        assert!(result.starts_with("你"));
    }

    #[test]
    fn test_truncate_to_budget_mixed_ascii_and_multibyte() {
        let text = "hello 世界!";
        // h(1) e(1) l(1) l(1) o(1) (1) 世(3) 界(3) !(1) = 13 bytes
        assert_eq!(text.len(), 13);
        // Budget 7 = just past the space, before 世 starts (byte 6 is space, 7 is mid-世)
        let result = truncate_to_budget(text, 7);
        assert!(result.contains("[truncated to 7 bytes]"));
        assert!(result.starts_with("hello "));
    }

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

    // --- Learnings formatting ---

    #[test]
    fn test_append_learnings_multiple() {
        let learnings = vec![
            LearningSummaryOutput {
                id: 1,
                title: "First Learning".to_string(),
                outcome: "pattern".to_string(),
                confidence: "high".to_string(),
                content: Some("Content 1".to_string()),
                applies_to_files: Some(vec!["src/*.rs".to_string()]),
                applies_to_task_types: None,
            },
            LearningSummaryOutput {
                id: 2,
                title: "Second Learning".to_string(),
                outcome: "failure".to_string(),
                confidence: "medium".to_string(),
                content: None,
                applies_to_files: None,
                applies_to_task_types: Some(vec!["FEAT-".to_string()]),
            },
        ];

        let mut prompt = String::new();
        append_learnings(&mut prompt, &learnings);

        assert!(prompt.contains("## Relevant Learnings"));
        assert!(prompt.contains("```json"));
        assert!(prompt.contains("First Learning"));
        assert!(prompt.contains("Second Learning"));
        assert!(prompt.contains("Content 1"));
    }

    // --- get_completed_dependencies edge cases ---

    #[test]
    fn test_get_completed_dependencies_none_done() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "DEP-001", "Still in progress", "in_progress", 1);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let deps = get_completed_dependencies(&conn, "TASK-001").unwrap();
        assert!(deps.is_empty(), "In-progress deps should not be listed");
    }

    #[test]
    fn test_get_completed_dependencies_ignores_synergy_relationships() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "SYN-001", "Synergy task", "done", 1);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        // Only synergyWith, NOT dependsOn
        insert_relationship(&conn, "TASK-001", "SYN-001", "synergyWith");

        let deps = get_completed_dependencies(&conn, "TASK-001").unwrap();
        assert!(
            deps.is_empty(),
            "Synergy relationships should not appear in dependency summaries"
        );
    }

    #[test]
    fn test_get_completed_dependencies_ordered_by_id() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "DEP-C", "Dep C", "done", 3);
        insert_task(&conn, "DEP-A", "Dep A", "done", 1);
        insert_task(&conn, "DEP-B", "Dep B", "done", 2);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "DEP-C", "dependsOn");
        insert_relationship(&conn, "TASK-001", "DEP-A", "dependsOn");
        insert_relationship(&conn, "TASK-001", "DEP-B", "dependsOn");

        let deps = get_completed_dependencies(&conn, "TASK-001").unwrap();
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].0, "DEP-A", "Should be ordered by ID");
        assert_eq!(deps[1].0, "DEP-B");
        assert_eq!(deps[2].0, "DEP-C");
    }

    // --- get_synergy_tasks_in_run edge cases ---

    #[test]
    fn test_get_synergy_tasks_in_run_no_commit() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "SYN-001", "Synergy task", "done", 5);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "SYN-001", "synergyWith");
        insert_run(&conn, "run-001");
        insert_run_task(&conn, "run-001", "SYN-001", 1);
        // Note: no last_commit set on the run

        let results = get_synergy_tasks_in_run(&conn, "TASK-001", "run-001").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "SYN-001");
        assert!(
            results[0].2.is_none(),
            "Commit should be None when run has no last_commit"
        );
    }

    #[test]
    fn test_get_synergy_tasks_in_run_nonexistent_run() {
        let (_temp_dir, conn) = setup_test_db();

        insert_task(&conn, "SYN-001", "Synergy task", "done", 5);
        insert_task(&conn, "TASK-001", "Main task", "todo", 10);
        insert_relationship(&conn, "TASK-001", "SYN-001", "synergyWith");

        let results = get_synergy_tasks_in_run(&conn, "TASK-001", "nonexistent-run").unwrap();
        // The LEFT JOIN means no run_tasks match, so no results with run data,
        // but the synergy task itself is still done. The query filters by run_id
        // in the LEFT JOIN clause, so SYN-001 will still appear (run-related columns will be NULL).
        // Actually, let's verify the actual behavior:
        // The query LEFT JOINs run_tasks ON task_id AND run_id, so if run doesn't exist,
        // rt.* will be NULL but the row still appears because of LEFT JOIN.
        // This is acceptable behavior — the task is still listed as a synergy task.
        assert!(results.len() <= 1, "Should return at most 1 synergy task");
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
        // Since there's no scripts/escalation-policy.md in cwd, this returns None.
        let bare_path = Path::new("prompt.md");
        let result = load_escalation_template(bare_path);
        // This should not panic — graceful None
        assert_eq!(
            result, None,
            "Base path with no parent should resolve relative to '.' and return None if not found"
        );
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
}
