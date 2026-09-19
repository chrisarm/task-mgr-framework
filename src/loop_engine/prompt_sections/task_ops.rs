//! Task lifecycle rules section — injected into every loop iteration prompt.
//!
//! This is non-negotiable hard-rule content. It must appear above learnings
//! and synergy sections so the agent reads the rules before context-sensitive material.

use crate::loop_engine::prompt::assembler::{PromptContext, Rendered, SectionKind, SectionSpec};

/// Stable section identifier for the task-lifecycle-rules section. Matches the
/// `section_sizes` key both prompt builders use for this section.
pub const TASK_OPS_SECTION: &str = "task_ops";

/// Render the task-lifecycle-rules section for the data-driven assembler
/// (CONTRACT-001). This is the **single render site** for the section, shared
/// verbatim by both paths' rosters — the content is identical static text with
/// no per-path or per-context variation, so the [`SectionKind`] argument is
/// deliberately ignored.
pub fn render_task_ops(_ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: task_ops_section().to_string(),
        ..Default::default()
    }
}

/// Build the task-ops [`SectionSpec`] (critical — never dropped by budget).
///
/// Shared by both prompt paths; each roster places the returned spec at its own
/// legacy display position.
pub fn task_ops_spec() -> SectionSpec {
    SectionSpec {
        name: TASK_OPS_SECTION,
        kind: SectionKind::Critical,
        render: render_task_ops,
    }
}

/// The exact markdown section text to inject.
///
/// Tells the loop agent: never edit tasks/*.json directly; use <task-status> tags
/// to update status, `task-mgr add --stdin --from-json` to create tasks, and
/// `task-mgr update --stdin` for whitelist field overlays.
pub(crate) fn task_ops_section() -> &'static str {
    "## Task lifecycle — CLI only, never read or edit the JSON\n\
     \n\
     You MUST NOT read or edit `tasks/*.json` directly. The PRD task JSON is large;\n\
     pulling one into context can push you past the model window mid-iteration and\n\
     force a retry. Use the `task-mgr` CLI for every task operation instead:\n\
     \n\
     - **Work selection**: the loop engine already claimed `## Current Task`.\n\
       Work ONLY that task. NEVER run `task-mgr next --claim` or `task-mgr next`\n\
       to pick work during loop iterations; use `task-mgr show <task-id>` for\n\
       re-reads.\n\
     - **Mark a task's status**: emit `<task-status>TASK-ID:done</task-status>`\n\
       (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`). The loop\n\
       engine parses these and applies them via `task-mgr`.\n\
     - **Look up another task**: ALWAYS prefer `task-mgr show <task-id>` (or\n\
       `task-mgr list` / `task-mgr next`). Last resort: `jq` for a field slice —\n\
       e.g. `jq '.userStories[]|select(.id==\"FEAT-007\")|{id,title,acceptanceCriteria}' tasks/<prd>.json`.\n\
       Never `cat`, `Read`, or `grep` the whole file.\n\
     - **List tasks / check status**: `task-mgr list`, `task-mgr next`.\n\
     - **Add a new task** (review fix / refactor / follow-up): pipe JSON to\n\
       `task-mgr add --stdin` (pin with `--from-json tasks/<prd>.json`; priority\n\
       is auto-computed). Example:\n\
     \n\
     \u{20}     echo '{\"id\":\"CODE-FIX-001\",\"title\":\"Fix race in X\",\"difficulty\":\"medium\",\"touchesFiles\":[\"src/foo.rs\"],\"dependsOn\":[]}' \\\\\n\
     \u{20}       | task-mgr add --stdin --from-json tasks/<prd>.json\n\
     \n\
     - **Fix in response to a milestone**: pass `--depended-on-by <id>`.\n\
     - **Update whitelist fields**: overlay JSON with `id` + fields →\n\
       `task-mgr update --stdin` (pin with `--from-json` when ≥2 prefixes).\n\
     - **Auto-prefix**: loop sets `TASK_MGR_ACTIVE_PREFIX`; bare IDs are\n\
       auto-prefixed to the active PRD. Cross-PRD IDs are rejected.\n\
     \n"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parity: the data-driven render fn must emit byte-identical text to the
    /// legacy `task_ops_section()` helper it wraps — the single shared critical
    /// across both prompt paths.
    #[test]
    fn render_task_ops_matches_legacy_section() {
        use crate::loop_engine::config::PermissionMode;
        use crate::models::Task;
        use rusqlite::Connection;
        use std::path::Path;

        let conn = Connection::open_in_memory().expect("in-memory db");
        let task = Task::new("T-1", "title");
        let mode = PermissionMode::Dangerous;
        let ctx = PromptContext {
            conn: &conn,
            task: &task,
            task_files: &[],
            project_root: Path::new("/tmp"),
            base_prompt_path: Path::new("/tmp/prompt.md"),
            permission_mode: &mode,
            steering_path: None,
            session_guidance: "",
            run_id: None,
            task_prefix: None,
            reorder_hint: None,
            batch_sibling_prds: None,
            resolved_model: None,
            resolved_models: crate::loop_engine::model::builtin_resolved_models(),
            next_task_output: None,
            recalled_learnings: None,
        };

        let spec = task_ops_spec();
        let rendered = (spec.render)(&ctx, spec.kind);
        assert_eq!(
            rendered.text,
            task_ops_section().to_string(),
            "render_task_ops must be byte-identical to task_ops_section()"
        );
        assert!(matches!(spec.kind, SectionKind::Critical));
    }

    #[test]
    fn test_section_contains_critical_phrases() {
        let section = task_ops_section();

        assert!(
            section.contains("MUST NOT read or edit"),
            "section must warn against both reading and editing the JSON"
        );
        assert!(
            section.contains("past the model window"),
            "section must explain why (context-window risk) so the rule sticks"
        );
        assert!(
            section.contains("task-mgr show"),
            "section must offer the CLI alternative for looking up other tasks"
        );
        assert!(
            section.contains("NEVER"),
            "section must hard-prohibit claiming another task"
        );
        assert!(
            section.contains("next --claim"),
            "section must explicitly prohibit next --claim"
        );
        assert!(
            section.contains("## Current Task"),
            "section must point at the pinned current task"
        );
        assert!(
            section.contains("jq"),
            "section must point at jq as the field-extraction fallback when JSON access is unavoidable"
        );
        assert!(
            section.contains("task-mgr add --stdin"),
            "section must contain 'task-mgr add --stdin'"
        );
        assert!(
            section.contains("<task-status>"),
            "section must contain '<task-status>'"
        );
        assert!(
            section.contains("--depended-on-by"),
            "section must teach --depended-on-by for milestone-spawned fixes"
        );
        assert!(
            section.contains("in response to a milestone"),
            "section must reference 'in response to a milestone' so the rule context is clear"
        );

        // All 5 status keywords
        assert!(section.contains("done"), "must contain status: done");
        assert!(section.contains("failed"), "must contain status: failed");
        assert!(section.contains("skipped"), "must contain status: skipped");
        assert!(
            section.contains("irrelevant"),
            "must contain status: irrelevant"
        );
        assert!(section.contains("blocked"), "must contain status: blocked");
    }

    #[test]
    fn test_section_uses_correct_path() {
        let section = task_ops_section();
        assert!(
            section.contains("tasks/*.json"),
            "must reference 'tasks/*.json' (not '.task-mgr/tasks/*.json')"
        );
        assert!(
            !section.contains(".task-mgr/tasks/"),
            "must NOT reference '.task-mgr/tasks/' — user-corrected path"
        );
    }

    #[test]
    fn test_section_teaches_user_stories_pin_and_update() {
        let section = task_ops_section();
        assert!(
            section.contains(".userStories[]"),
            "jq example must use .userStories[] (PRD JSON key; learnings #1252/#4114/#3332)"
        );
        assert!(
            !section.contains(".tasks[]"),
            "jq example must not use .tasks[] — that key does not exist on PRD JSON"
        );
        assert!(
            section.contains("--from-json"),
            "add/update examples must teach --from-json pinning"
        );
        assert!(
            section.contains("task-mgr update"),
            "section must name task-mgr update for whitelist overlays"
        );
        // Pin 11: JSON-sync failure copy names current + retry --from-json, never export.
        assert!(
            !section.contains("task-mgr export") && !section.contains("via export"),
            "section must not tell agents to recover JSON-sync via export"
        );
    }

    #[test]
    fn test_section_size_within_budget() {
        let section = task_ops_section();
        assert!(
            section.len() < 2048,
            "section is {} bytes, must be < 2048 to stay within prompt budget",
            section.len()
        );
    }
}
