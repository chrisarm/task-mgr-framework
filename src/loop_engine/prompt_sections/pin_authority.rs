//! Late authority pin for loop task selection.
//!
//! This section is emitted after the base prompt so stale generated templates
//! cannot override the engine-owned current-task contract.

use crate::loop_engine::prompt::assembler::{PromptContext, Rendered, SectionKind, SectionSpec};

/// Stable section identifier for the late authority-pin section.
pub const PIN_AUTHORITY_SECTION: &str = "pin_authority";

/// Render the authority-pin section for the data-driven assembler.
pub fn render_pin_authority(_ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: pin_authority_section().to_string(),
        ..Default::default()
    }
}

/// Build the authority-pin [`SectionSpec`] (critical — never dropped by budget).
pub fn pin_authority_spec() -> SectionSpec {
    SectionSpec {
        name: PIN_AUTHORITY_SECTION,
        kind: SectionKind::Critical,
        render: render_pin_authority,
    }
}

/// The exact markdown authority pin injected after the base prompt.
pub(crate) fn pin_authority_section() -> &'static str {
    "## Task selection authority\n\
     \n\
     The loop engine has already selected and claimed the task in `## Current Task`.\n\
     During loop iterations, NEVER run `task-mgr next --claim` and do not run\n\
     `task-mgr next` to pick work. Work only the pinned current task.\n\
     \n\
     To influence the NEXT iteration, emit `<reorder>TASK-ID</reorder>`; the engine\n\
     will claim that task on the next iteration. For re-reading task details, use\n\
     `task-mgr show <task-id>`.\n\
     \n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_authority_contains_claim_prohibition_and_current_task() {
        let section = pin_authority_section();

        assert!(
            section.contains("next --claim"),
            "section must explicitly prohibit next --claim"
        );
        assert!(
            section.contains("NEVER"),
            "section must use hard prohibition language"
        );
        assert!(
            section.contains("## Current Task"),
            "section must point at the pinned current task"
        );
        assert!(
            section.contains("<reorder>TASK-ID</reorder>"),
            "section must explain reorder next-iteration semantics"
        );
    }

    #[test]
    fn render_pin_authority_matches_section() {
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

        let spec = pin_authority_spec();
        let rendered = (spec.render)(&ctx, spec.kind);
        assert_eq!(
            rendered.text,
            pin_authority_section().to_string(),
            "render_pin_authority must be byte-identical to pin_authority_section()"
        );
        assert!(matches!(spec.kind, SectionKind::Critical));
    }
}
