//! `task-mgr cheatsheet` — one canonical, always-accurate cheat sheet.
//!
//! Three sections in fixed order:
//!
//! 1. **Common Recipes** — curated, hand-written intent → command map.
//!    Capped at ~30 lines so the top of the file fits a single screen.
//!    Hand-written because clap can't express the WHY of a recipe.
//!
//! 2. **Command Reference** — generated from
//!    [`crate::cli::introspect::generate_command_reference`], which walks
//!    `clap::Command::get_subcommands()`. Any subcommand added to the
//!    clap derive shows up automatically. `tests/cheatsheet_drift.rs` is
//!    the CI gate that ensures the reference table contains every
//!    non-hidden subcommand.
//!
//! 3. **State Inspection** — a paragraph reminding operators to run
//!    `task-mgr current` before piping IDs into other commands. The most
//!    common source of cross-PRD bugs is silent context drift, and
//!    `task-mgr current` makes the active prefix explicit.
//!
//! The same `generate_command_reference()` call also drives the
//! `enhance agents` / `enhance show` body (via
//! [`crate::commands::enhance::templates::EnhanceProfile::body`]), so the
//! cheatsheet and the CLAUDE.md-embedded block never drift apart.

use serde::Serialize;

use crate::cli::introspect::generate_command_reference;

/// Result of `task-mgr cheatsheet`.
///
/// `content` is the full rendered cheat sheet (curated recipes +
/// generated command reference + state-inspection paragraph) as a single
/// string. Callers print it as-is via [`format_text`] for text output, or
/// emit `{ "content": "…" }` for `--format json`.
#[derive(Debug, Clone, Serialize)]
pub struct CheatsheetResult {
    /// Full rendered cheat sheet body.
    pub content: String,
}

/// Curated narrative section.
///
/// Hand-written, capped at ≤30 lines per the FEAT-002 acceptance budget.
/// Every `task-mgr <subcommand> --<flag>` token in this string is
/// verified by `tests/cheatsheet_drift.rs` against clap's known
/// subcommand + flag set, so a typo or stale command name here fails CI.
///
/// Maintainers: keep the entry count under 16 to stay inside the budget,
/// and prefer the `task-mgr <subcommand>` form (in backticks) so the
/// drift parser can lift it.
pub const CURATED_RECIPES: &str = "## Common Recipes\n\
\n\
- Look up one task: `task-mgr show <task-id>`\n\
- List tasks: `task-mgr list` (filter with `--status` / `--prefix` / `--task-type`)\n\
- Pick and claim the next eligible task: `task-mgr next --claim`\n\
- Add a fixup / follow-up task: pipe JSON to `task-mgr add --stdin --depended-on-by <id>`\n\
- Mark status from a loop iteration: emit `<task-status>TASK-ID:done</task-status>` (done / failed / skipped / irrelevant / blocked)\n\
- Mark status outside the loop: `task-mgr complete <id>` (also: `task-mgr fail`, `task-mgr skip`, `task-mgr unblock`, `task-mgr unskip`, `task-mgr reset`)\n\
- Record a learning: `task-mgr learn --outcome <success|failure|workaround|pattern> --title \"...\"`\n\
- Recall learnings for THIS task: `task-mgr recall --for-task <task-id>`\n\
- Recall learnings by query (offline OK with `--allow-degraded`): `task-mgr recall --query \"<text>\"`\n\
- Initialize from a PRD JSON: `task-mgr loop init <prd>.json` (mid-loop sync: add `--append --update-existing`)\n\
- Run the autonomous loop: `task-mgr loop run <prd>.json --yes`\n\
- Run multiple PRDs: `task-mgr batch init '<glob>'` then `task-mgr batch run '<glob>' --yes`\n\
- List architectural decisions: `task-mgr decisions list`\n\
- Ratify a decision: `task-mgr decisions resolve <id> <letter>`\n\
- Inspect the active PRD context: `task-mgr current`\n";

/// State-inspection paragraph rendered as the trailing section.
///
/// Single paragraph by design: this is the "what do I run when I'm
/// confused?" pointer. Anything longer here gets ignored.
pub const STATE_INSPECTION: &str = "## State Inspection\n\
\n\
Run `task-mgr current` to print the currently-resolved active task prefix, how it was \
resolved (env / single-prefix / from-json / none), and the target PRD JSON path. Always \
inspect before piping task IDs into other commands — silent fallback to a different PRD \
is the most common source of cross-PRD bugs.\n";

/// Render the cheat sheet body.
///
/// The function is pure: it always returns the same output for the same
/// build of the binary (the curated recipes are a `const &str` and the
/// reference table is a deterministic walk of `Cli::command()`). Safe to
/// call from any thread; no I/O.
pub fn cheatsheet() -> CheatsheetResult {
    let mut content = String::with_capacity(
        CURATED_RECIPES.len() + STATE_INSPECTION.len() + 4096, // table headroom
    );
    content.push_str(CURATED_RECIPES);
    content.push_str("\n## Command Reference\n\n");
    content.push_str(&generate_command_reference());
    content.push('\n');
    content.push_str(STATE_INSPECTION);
    CheatsheetResult { content }
}

/// Format for `--format text`. The text output IS `result.content`
/// verbatim — no extra prefix/suffix, because the curated recipes
/// already start with `## Common Recipes`.
pub fn format_text(result: &CheatsheetResult) -> String {
    result.content.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheatsheet_starts_with_common_recipes_heading() {
        let r = cheatsheet();
        assert!(
            r.content.starts_with("## Common Recipes\n"),
            "must start with curated heading; got: {:?}",
            &r.content[..r.content.len().min(40)]
        );
    }

    #[test]
    fn cheatsheet_contains_three_sections_in_order() {
        let r = cheatsheet();
        let recipes_idx = r
            .content
            .find("## Common Recipes")
            .expect("must have Common Recipes");
        let reference_idx = r
            .content
            .find("## Command Reference")
            .expect("must have Command Reference");
        let inspection_idx = r
            .content
            .find("## State Inspection")
            .expect("must have State Inspection");
        assert!(recipes_idx < reference_idx);
        assert!(reference_idx < inspection_idx);
    }

    #[test]
    fn cheatsheet_command_reference_section_contains_generated_table() {
        let r = cheatsheet();
        // The generated table's table header must appear inside the
        // Command Reference section.
        assert!(r.content.contains("| Command | Description |"));
        assert!(r.content.contains("| `task-mgr current` |"));
        assert!(r.content.contains("| `task-mgr loop init` |"));
    }

    #[test]
    fn cheatsheet_state_inspection_points_at_task_mgr_current() {
        let r = cheatsheet();
        // Extract just the State Inspection section to ensure the
        // reference to `task-mgr current` is there (not just elsewhere
        // in the cheatsheet).
        let idx = r.content.find("## State Inspection").unwrap();
        let section = &r.content[idx..];
        assert!(
            section.contains("`task-mgr current`"),
            "State Inspection must reference task-mgr current"
        );
    }

    #[test]
    fn curated_recipes_under_thirty_lines() {
        // qualityDimensions: curated recipes <= 30 lines.
        let line_count = CURATED_RECIPES.lines().count();
        assert!(
            line_count <= 30,
            "curated recipes must be <= 30 lines, was {line_count}"
        );
    }

    #[test]
    fn cheatsheet_does_not_contain_known_wrong_strings() {
        // Negative anchors per FEAT-002 acceptance: these substrings
        // were clap-verified WRONG on 2026-05-18 and must never appear
        // in the cheatsheet stdout.
        let content = cheatsheet().content;
        for anchor in [
            "set-status",
            "add --from-json",
            "recall --top-k",
            "learnings show ",
        ] {
            assert!(
                !content.contains(anchor),
                "cheatsheet contains forbidden anchor {anchor:?}"
            );
        }
    }

    #[test]
    fn format_text_returns_content_verbatim() {
        let r = cheatsheet();
        assert_eq!(format_text(&r), r.content);
    }
}
