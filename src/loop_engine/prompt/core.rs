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

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::commands::next::output::{LearningSummaryOutput, NextTaskOutput};
use crate::learnings::recall::{RecallParams, recall_learnings};
use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::context::scan_source_context;
use crate::loop_engine::prompt::assembler::{PromptContext, Rendered, SectionKind, SectionSpec};
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

/// Build a steering section from a `steering.md` file path.
///
/// Returns `""` when the file is missing, unreadable, or contains only
/// whitespace — sequential and slot prompt builders both treat absence as
/// "no project-wide steering" rather than an error. Wrapping the content in
/// a `## Steering` header keeps display order deterministic across builders.
pub fn build_steering_block(steering_path: &Path) -> String {
    match fs::read_to_string(steering_path) {
        Ok(content) if !content.trim().is_empty() => {
            format!("## Steering\n\n{}\n\n", content.trim())
        }
        _ => String::new(),
    }
}

/// Build a session-guidance section from operator pause feedback. Returns
/// `""` when `guidance` is empty so callers can append unconditionally
/// without rendering an empty header.
pub fn build_session_guidance_block(guidance: &str) -> String {
    if guidance.is_empty() {
        String::new()
    } else {
        format!("## Session Guidance\n\n{guidance}\n\n")
    }
}

/// Build the key-decision-points instruction block. For tasks whose ID
/// contains `REVIEW` or `VERIFY`, an extra paragraph asks the agent to
/// actively look for architectural alternatives.
pub fn build_key_decisions_block(task_id: &str) -> String {
    let is_review = task_id.contains("REVIEW") || task_id.contains("VERIFY");

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

// ===========================================================================
// CONTRACT-001 shared section specs (FEAT-005)
//
// The five sections below are rendered identically by both the sequential and
// slot paths (each wraps a `build_*_block` helper above with no per-path
// variation), so each gets ONE shared `SectionSpec` — the single render site
// for the section's bytes. Each path's roster places the returned spec at its
// own legacy display position.
// ===========================================================================

/// Stable section identifier for the source-context section.
pub const SOURCE_SECTION: &str = "source";
/// Stable section identifier for the steering section.
pub const STEERING_SECTION: &str = "steering";
/// Stable section identifier for the session-guidance section.
pub const SESSION_GUIDANCE_SECTION: &str = "session_guidance";
/// Stable section identifier for the tool-awareness section.
pub const TOOL_AWARENESS_SECTION: &str = "tool_awareness";
/// Stable section identifier for the key-decision-points section.
pub const KEY_DECISION_SECTION: &str = "key_decision";

/// Byte budget for the source-context section, shared by both prompt paths.
///
/// This is the single source of truth for the 2000-byte cap that the
/// sequential and slot builders each previously held as a private
/// `SOURCE_CONTEXT_BUDGET` const. It rides on the source spec's
/// [`SectionKind::Trimmable`] budget — deliberately kept distinct from the
/// slot learnings budget so a source overflow never shrinks the learnings cap
/// (and vice versa).
pub const SOURCE_CONTEXT_BUDGET: usize = 2000;

/// Render the source-context section, reading its per-section byte cap from the
/// [`SectionKind::Trimmable`] budget. `Critical` is unreachable for this spec
/// but maps to "no cap" defensively.
fn render_source_section(ctx: &PromptContext<'_>, kind: SectionKind) -> Rendered {
    let budget = match kind {
        SectionKind::Trimmable { budget } => budget,
        SectionKind::Critical => usize::MAX,
    };
    Rendered {
        text: build_source_context_block(ctx.task_files, budget, ctx.project_root),
        ..Default::default()
    }
}

/// Build the source-context [`SectionSpec`] (trimmable, capped at
/// [`SOURCE_CONTEXT_BUDGET`]). Shared by both prompt paths.
pub fn source_spec() -> SectionSpec {
    SectionSpec {
        name: SOURCE_SECTION,
        kind: SectionKind::Trimmable {
            budget: SOURCE_CONTEXT_BUDGET,
        },
        render: render_source_section,
    }
}

/// Render the steering section from [`PromptContext::steering_path`]. `None`
/// (no project `steering.md`) renders an empty section.
fn render_steering_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: ctx
            .steering_path
            .map(build_steering_block)
            .unwrap_or_default(),
        ..Default::default()
    }
}

/// Build the steering [`SectionSpec`] (trimmable, no independent cap — the
/// section either fits whole into the remaining total budget or is dropped).
pub fn steering_spec() -> SectionSpec {
    SectionSpec {
        name: STEERING_SECTION,
        kind: SectionKind::Trimmable { budget: usize::MAX },
        render: render_steering_section,
    }
}

/// Render the session-guidance section from [`PromptContext::session_guidance`].
/// An empty guidance string renders an empty section.
fn render_session_guidance_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: build_session_guidance_block(ctx.session_guidance),
        ..Default::default()
    }
}

/// Build the session-guidance [`SectionSpec`] (trimmable, no independent cap).
pub fn session_guidance_spec() -> SectionSpec {
    SectionSpec {
        name: SESSION_GUIDANCE_SECTION,
        kind: SectionKind::Trimmable { budget: usize::MAX },
        render: render_session_guidance_section,
    }
}

/// Render the tool-awareness section from [`PromptContext::permission_mode`].
fn render_tool_awareness_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: build_tool_awareness_block(ctx.permission_mode),
        ..Default::default()
    }
}

/// Build the tool-awareness [`SectionSpec`] (trimmable, no independent cap).
pub fn tool_awareness_spec() -> SectionSpec {
    SectionSpec {
        name: TOOL_AWARENESS_SECTION,
        kind: SectionKind::Trimmable { budget: usize::MAX },
        render: render_tool_awareness_section,
    }
}

/// Render the key-decision-points section from the task id.
fn render_key_decision_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: build_key_decisions_block(&ctx.task.id),
        ..Default::default()
    }
}

/// Build the key-decision-points [`SectionSpec`] (trimmable, no independent cap).
pub fn key_decision_spec() -> SectionSpec {
    SectionSpec {
        name: KEY_DECISION_SECTION,
        kind: SectionKind::Trimmable { budget: usize::MAX },
        render: render_key_decision_section,
    }
}
