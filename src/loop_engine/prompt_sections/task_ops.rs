//! Task lifecycle rules section — injected into every loop iteration prompt.
//!
//! This is non-negotiable hard-rule content. It must appear above learnings
//! and synergy sections so the agent reads the rules before context-sensitive material.

/// The exact markdown section text to inject.
///
/// Tells the loop agent: never edit tasks/*.json directly; use <task-status> tags
/// to update status and `task-mgr add --stdin` to create new tasks.
pub(crate) fn task_ops_section() -> &'static str {
    "## Task lifecycle — CLI only, never read or edit the JSON\n\
     \n\
     You MUST NOT read or edit `tasks/*.json` directly. The PRD task JSON is large;\n\
     pulling one into context can push you past the model window mid-iteration and\n\
     force a retry. Use the `task-mgr` CLI for every task operation instead:\n\
     \n\
     - **Mark a task's status**: emit `<task-status>TASK-ID:done</task-status>`\n\
       (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`). The loop\n\
       engine parses these and applies them via `task-mgr`.\n\
     - **Look up another task**: ALWAYS prefer `task-mgr show <task-id>` (or\n\
       `task-mgr list` / `task-mgr next`) — these cover almost every read. Only\n\
       as a last resort, if the CLI can't give you what you need, use `jq` to pull\n\
       just the field(s) — e.g.\n\
       `jq '.tasks[]|select(.id==\"FEAT-007\")|{id,title,acceptanceCriteria}' tasks/<prd>.json`.\n\
       Never `cat`, `Read`, or `grep` the whole file.\n\
     - **List tasks / check status**: `task-mgr list`, `task-mgr next`.\n\
     - **Add a new task** (review fix / refactor / follow-up): pipe a single task\n\
       JSON to `task-mgr add --stdin`. Example:\n\
     \n\
     \u{20}     echo '{\"id\":\"CODE-FIX-001\",\"title\":\"Fix race in X\",\"difficulty\":\"medium\",\"touchesFiles\":[\"src/foo.rs\"],\"dependsOn\":[]}' \\\\\n\
     \u{20}       | task-mgr add --stdin\n\
     \n\
       Priority is auto-computed; omit for lower priority.\n\
     - **Fix in response to a milestone**: pass `--depended-on-by <id>`.\n\
     - **Auto-prefix**: loop exports `TASK_MGR_ACTIVE_PREFIX`; bare IDs are\n\
       auto-prefixed to the active PRD. Cross-PRD IDs are rejected.\n\
     - For anything else (dependencies, status queries, etc.), use the `task-mgr`\n\
       CLI — see `task-mgr --help`.\n\
     \n"
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_section_size_within_budget() {
        let section = task_ops_section();
        assert!(
            section.len() < 2048,
            "section is {} bytes, must be < 2048 to stay within prompt budget",
            section.len()
        );
    }
}
