//! Deprecated synergy section: gutted to no-ops after parallel-task-execution
//! dropped the `synergyWith` / `batchWith` / `conflictsWith` relationship types
//! in favour of runtime file-overlap detection.
//!
//! The remaining functions are retained so `prompt.rs` call sites don't change,
//! but they no longer query partners or resolve models — model selection is
//! owned entirely by `model::resolve_execution_plan` at the spawn site.

use rusqlite::Connection;

use crate::loop_engine::prompt::assembler::{PromptContext, Rendered, SectionKind, SectionSpec};

/// Stable section identifier for the synergy section. Matches the
/// `section_sizes` key the sequential builder already uses for this section.
pub const SYNERGY_SECTION: &str = "synergy";

/// No-op: synergy context sections were removed along with synergy relationships.
///
/// Always returns an empty string. Parameters are kept to preserve the call-site
/// signature in `prompt.rs`.
pub fn build_synergy_section(_conn: &Connection, _task_id: &str, _run_id: Option<&str>) -> String {
    String::new()
}

/// Render the synergy section for the data-driven assembler (CONTRACT-001).
/// Sequential-only — the slot roster omits it (wave slots are disjoint by
/// design). This is the **single render site** for the section; it wraps the
/// permanent no-op [`build_synergy_section`], so the rendered text is always
/// empty. The [`SectionKind`] budget is ignored (the section has no cap).
pub fn render_synergy_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: build_synergy_section(ctx.conn, &ctx.task.id, ctx.run_id),
        ..Default::default()
    }
}

/// Build the synergy [`SectionSpec`] (trimmable, no independent cap).
///
/// Present in the sequential roster only. The `budget` is `usize::MAX` because
/// the no-op never emits text — `assemble` gates it against the remaining total
/// budget and the render fn ignores the budget entirely.
pub fn synergy_spec() -> SectionSpec {
    SectionSpec {
        name: SYNERGY_SECTION,
        kind: SectionKind::Trimmable { budget: usize::MAX },
        render: render_synergy_section,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::loop_engine::test_utils::setup_test_db;

    #[test]
    fn test_build_synergy_section_always_empty() {
        let (_temp_dir, conn) = setup_test_db();
        assert_eq!(build_synergy_section(&conn, "TASK-001", None), "");
        assert_eq!(build_synergy_section(&conn, "TASK-001", Some("run-1")), "");
    }
}
