//! Task lifecycle rules section — injected into every loop iteration prompt.
//!
//! This is non-negotiable hard-rule content. It must appear above learnings
//! and synergy sections so the agent reads the rules before context-sensitive material.

/// The exact markdown section text to inject.
///
/// Tells the loop agent: never edit tasks/*.json directly; use <task-status> tags
/// to update status and `task-mgr add --stdin` to create new tasks.
pub(crate) fn task_ops_section() -> &'static str {
    "## Task lifecycle — CLI only, never edit the JSON\n\
     \n\
     You MUST NOT edit `tasks/*.json` directly. Instead:\n\
     \n\
     - **Mark a task's status**: emit `<task-status>TASK-ID:done</task-status>`\n\
       (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`). The loop\n\
       engine parses these and applies them via `task-mgr`.\n\
     - **Add a new task** (review fix / refactor / follow-up): pipe a single task\n\
       JSON to `task-mgr add --stdin`. Example:\n\
     \n\
     \u{20}     echo '{\"id\":\"CODE-FIX-001\",\"title\":\"Fix race in X\",\"difficulty\":\"medium\",\"touchesFiles\":[\"src/foo.rs\"],\"dependsOn\":[]}' \\\\\n\
     \u{20}       | task-mgr add --stdin\n\
     \n\
       Priority is auto-computed to rank ahead of the current `next` pick. Omit it\n\
       unless you explicitly need a lower priority.\n\
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
            section.contains("MUST NOT edit"),
            "section must contain 'MUST NOT edit'"
        );
        assert!(
            section.contains("task-mgr add --stdin"),
            "section must contain 'task-mgr add --stdin'"
        );
        assert!(
            section.contains("<task-status>"),
            "section must contain '<task-status>'"
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
            section.len() < 1024,
            "section is {} bytes, must be < 1024 to stay within prompt budget",
            section.len()
        );
    }
}
