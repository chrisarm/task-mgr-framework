//! Bedrock prompt section helpers shared by sequential and slot prompt builders.
//!
//! Each helper renders a single, self-contained section of the agent prompt.
//! They are deliberately small and free of cross-section coupling so the
//! sequential builder ([`super::sequential`]) and the slot builder
//! ([`super::slot`]) can compose them without diverging.
//!
//! Invariants honored here (and validated by `tests/prompt_core.rs`):
//! - [`format_task_json`] produces JSON that round-trips via
//!   `serde_json::from_str` and includes `id`, `title`, and `files`.
//! - [`completion_instruction`] mentions both the task ID and the title.
//! - [`build_learnings_block`] returns `("", vec![])` on retrieval failure
//!   (e.g. missing FTS5 table on a partially migrated DB) — no panics.
//! - [`build_source_context_block`] returns `""` when `project_root` does not
//!   exist (graceful degradation, not an error).
//! - [`build_tool_awareness_block`] and [`build_key_decisions_block`] produce
//!   non-empty content for valid inputs.

use std::path::Path;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::commands::next::output::{LearningSummaryOutput, NextTaskOutput};
use crate::learnings::recall::{RecallParams, recall_learnings};
use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::context::scan_source_context;
use crate::loop_engine::prompt_sections::learnings::build_learnings_section;
use crate::loop_engine::prompt_sections::truncate_to_budget;
use crate::models::Task;

/// Format a task as a JSON string suitable for the prompt's task block.
///
/// The output is pretty-printed JSON containing at minimum the task id, title,
/// and `files`. Optional fields (description, notes, model, difficulty,
/// escalationNote) are included only when present, mirroring the camelCase
/// shape the sequential builder emits today.
///
/// `files` is taken from the caller (typically the `task_files` join) rather
/// than the `Task` model so the slot builder can pre-load it on the main
/// thread and pass the resulting JSON across thread boundaries without a
/// `&Connection`.
pub fn format_task_json(task: &Task, files: &[String]) -> String {
    format_task_json_raw(
        &task.id,
        &task.title,
        task.priority,
        task.status.as_db_str(),
        &task.acceptance_criteria,
        files,
        task.description.as_deref(),
        task.notes.as_deref(),
        task.model.as_deref(),
        task.difficulty.as_deref(),
        task.escalation_note.as_deref(),
    )
}

/// Format a `NextTaskOutput` as a JSON string suitable for the prompt's task
/// block. Delegates to [`format_task_json_raw`] so both sequential and slot
/// builders share a single canonical implementation.
pub fn format_next_task_json(task: &NextTaskOutput) -> String {
    format_task_json_raw(
        &task.id,
        &task.title,
        task.priority,
        &task.status,
        &task.acceptance_criteria,
        &task.files,
        task.description.as_deref(),
        task.notes.as_deref(),
        task.model.as_deref(),
        task.difficulty.as_deref(),
        task.escalation_note.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn format_task_json_raw(
    id: &str,
    title: &str,
    priority: i32,
    status: &str,
    acceptance_criteria: &[String],
    files: &[String],
    description: Option<&str>,
    notes: Option<&str>,
    model: Option<&str>,
    difficulty: Option<&str>,
    escalation_note: Option<&str>,
) -> String {
    let mut json = json!({
        "id": id,
        "title": title,
        "priority": priority,
        "status": status,
        "acceptanceCriteria": acceptance_criteria,
        "files": files,
    });

    if let Some(desc) = description {
        json["description"] = Value::String(desc.to_owned());
    }
    if let Some(n) = notes {
        json["notes"] = Value::String(n.to_owned());
    }
    if let Some(m) = model {
        json["model"] = Value::String(m.to_owned());
    }
    if let Some(d) = difficulty {
        json["difficulty"] = Value::String(d.to_owned());
    }
    if let Some(e) = escalation_note {
        json["escalationNote"] = Value::String(e.to_owned());
    }

    serde_json::to_string_pretty(&json).unwrap_or_else(|_| format!("{{\"id\":\"{id}\"}}"))
}

/// Build the completion-instruction section that tells the agent how to
/// signal task completion (commit message + `<completed>` tag).
///
/// Both the task id and title are referenced so the agent can copy-paste the
/// commit message verbatim.
pub fn completion_instruction(task_id: &str, title: &str) -> String {
    format!(
        "## Completing This Task\n\n\
         When all acceptance criteria for **{title}** pass:\n\
         1. Commit with message: `feat: {task_id}-completed - {title}`\n\
            If completing multiple tasks: `feat: ID1-completed, ID2-completed - [Title]`\n\
         2. Output `<completed>{task_id}</completed>` (using the full task ID shown above).\n\
         3. Stop immediately. Do NOT wait on background tasks, `Monitor` streams, \
            polling loops, or `run_in_background` commands after emitting `<completed>`. \
            If any background process is still running, kill it (`KillShell`, SIGTERM) \
            and exit this turn. The loop treats `<completed>` as terminal — anything \
            you wait for afterward is wasted wall-clock until the base timeout kills \
            the subprocess.\n\n\
         The loop will automatically mark the task done and update the PRD.\n\
         Do NOT run `task-mgr done` manually.\n\n",
    )
}

/// Build the learnings block by recalling task-relevant learnings and
/// formatting them. Returns the rendered section plus the IDs of the
/// learnings that were shown (so the caller can record bandit feedback).
///
/// Returns `("", vec![])` on any retrieval error — for example, a fresh DB
/// where migration v8 (the `learnings_fts` virtual table) hasn't run yet.
/// This degradation is intentional: a missing learning section must never
/// turn into a hard prompt-build failure.
pub fn build_learnings_block(conn: &Connection, task: &Task, budget: usize) -> (String, Vec<i64>) {
    let recall_params = RecallParams {
        for_task: Some(task.id.clone()),
        limit: 5,
        ..Default::default()
    };

    let learnings: Vec<LearningSummaryOutput> = match recall_learnings(conn, recall_params) {
        Ok(result) => result
            .learnings
            .into_iter()
            .map(LearningSummaryOutput::from)
            .collect(),
        Err(e) => {
            eprintln!("Warning: failed to retrieve learnings: {}", e);
            return (String::new(), Vec::new());
        }
    };

    if learnings.is_empty() {
        return (String::new(), Vec::new());
    }

    let shown_ids: Vec<i64> = learnings.iter().map(|l| l.id).collect();
    let section = truncate_to_budget(&build_learnings_section(&learnings), budget);
    (section, shown_ids)
}

/// Build the source-context block by scanning `touches_files` rooted at
/// `project_root`. Returns `""` when `project_root` does not exist — a
/// missing root is treated as "nothing to scan", not an error, so prompt
/// assembly survives `--project-root` typos and detached worktrees.
pub fn build_source_context_block(
    touches_files: &[String],
    budget: usize,
    project_root: &Path,
) -> String {
    if !project_root.exists() {
        return String::new();
    }
    scan_source_context(touches_files, budget, project_root).format_for_prompt()
}

/// Build the tool-awareness block describing the tools the agent has
/// access to under `permission_mode`.
///
/// Prevents the "I need Bash access" behavioral pattern by explicitly
/// informing the agent about its available tools based on the resolved
/// permission mode.
pub fn build_tool_awareness_block(permission_mode: &PermissionMode) -> String {
    match permission_mode {
        PermissionMode::Scoped {
            allowed_tools: Some(tools),
        } => {
            let bash_prefixes: Vec<&str> = tools
                .split(',')
                .filter_map(|t| {
                    let t = t.trim();
                    t.strip_prefix("Bash(").and_then(|s| s.strip_suffix(":*)"))
                })
                .collect();

            let tool_count = tools.split(',').count();
            let mut section = format!(
                "## Available Tools\n\n\
                 You have {tool_count} pre-approved tools. "
            );

            if !bash_prefixes.is_empty() {
                section.push_str(&format!(
                    "Bash commands are scoped to: `{}`.\n",
                    bash_prefixes.join("`, `")
                ));
            }

            section.push_str(
                "\nDo NOT say \"I need Bash access\" or ask for permission. \
                 You already have these permissions — just use the tools.\n\n\
                 **Environment variables**: Commands like `VAR=val command` will be denied \
                 because the shell sees `VAR=val` as the first token, not `command`. \
                 Use `env VAR=val command` instead — `env` is an allowed prefix.\n\n",
            );

            section
        }
        PermissionMode::Dangerous => "## Available Tools\n\n\
             You have unrestricted tool access. Just use any tool you need.\n\n"
            .to_string(),
        PermissionMode::Auto { .. } => "## Available Tools\n\n\
             You have auto-approved tool access. Just use any tool you need.\n\n"
            .to_string(),
        PermissionMode::Scoped {
            allowed_tools: None,
        } => String::new(),
    }
}

/// Build the key-decision-points instruction block. For tasks whose ID
/// contains `REVIEW` or `VERIFY`, an extra paragraph asks the agent to
/// actively look for architectural alternatives.
pub fn build_key_decisions_block(task: &Task) -> String {
    let is_review = task.id.contains("REVIEW") || task.id.contains("VERIFY");

    let review_emphasis = if is_review {
        "\n\nFor this task (code review / verification), **actively look for architectural \
         alternatives** and flag any decision forks where a different approach would have \
         significant long-term impact on maintainability, performance, or correctness.\n"
    } else {
        ""
    };

    format!(
        "## Key Decision Points\n\n\
         If you discover an important architectural decision during this task — a fork in \
         the road where different choices have significant long-term consequences — emit a \
         `<key-decision>` tag so it can be reviewed and stored for follow-up.\n\
         {review_emphasis}\n\
         **Format:**\n\
         ```xml\n\
         <key-decision>\n\
           <title>Short descriptive title</title>\n\
           <description>Why this decision matters and what the trade-offs are</description>\n\
           <option label=\"Option A\">Trade-offs for A</option>\n\
           <option label=\"Option B\">Trade-offs for B</option>\n\
         </key-decision>\n\
         ```\n\n\
         Only emit this for genuine architectural forks. Skip trivial implementation details.\n\n"
    )
}
