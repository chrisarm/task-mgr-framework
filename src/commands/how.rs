//! `task-mgr how '<intent>'` — natural-language intent → recipe.
//!
//! Hand-curated, deterministic keyword → recipe map (see [`crate::commands::intents`]).
//! Matching is case-insensitive substring `contains_all`. No LLM, no embeddings,
//! no I/O. With no arg (or an empty arg), prints every intent's keyword bag so
//! operators can discover what's available; on no-match, points the operator
//! at `task-mgr cheatsheet`.

use serde::Serialize;

use crate::commands::intents::{INTENTS, match_intents};

const NO_MATCH: &str = "no recipe matched. Run `task-mgr cheatsheet` for the full surface.";
const SEPARATOR: &str = "\n---\n";

/// Result of `task-mgr how`. `content` is the rendered text body (or a
/// machine-discoverable string for `--format json`).
#[derive(Debug, Clone, Serialize)]
pub struct HowResult {
    pub content: String,
}

/// Render `task-mgr how`'s response for the given query.
///
/// * `None` / empty / whitespace-only query → list every intent's keyword bag
///   on its own line so the operator can discover what's available.
/// * One or more intents match → recipes joined by `---` (a blank-separated
///   horizontal rule), in stable declaration order.
/// * No matches → a single line pointing the operator at `task-mgr cheatsheet`.
pub fn how(query: Option<&str>) -> HowResult {
    let trimmed = query.unwrap_or("").trim();
    if trimmed.is_empty() {
        return HowResult {
            content: render_intent_list(),
        };
    }
    let matches = match_intents(trimmed);
    if matches.is_empty() {
        return HowResult {
            content: format!("{NO_MATCH}\n"),
        };
    }
    let recipes: Vec<&str> = matches.iter().map(|&i| INTENTS[i].1).collect();
    HowResult {
        content: format!("{}\n", recipes.join(SEPARATOR)),
    }
}

fn render_intent_list() -> String {
    let mut out = String::with_capacity(64 + INTENTS.len() * 32);
    out.push_str("Available intents (pass any matching keywords to `task-mgr how '<text>'`):\n\n");
    for (bag, _) in INTENTS {
        out.push_str(&bag.join(" "));
        out.push('\n');
    }
    out
}

/// Format for `--format text`. The text output IS `result.content` verbatim.
pub fn format_text(result: &HowResult) -> String {
    result.content.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arg_lists_intent_keyword_bags_one_per_line() {
        let r = how(None);
        assert!(r.content.starts_with("Available intents"));
        // Every intent's keyword bag appears, joined by a space, on its own line.
        for (bag, _) in INTENTS {
            let line = bag.join(" ");
            assert!(
                r.content.lines().any(|l| l == line),
                "intent bag {:?} not listed; got:\n{}",
                bag,
                r.content,
            );
        }
    }

    #[test]
    fn empty_string_same_as_no_arg() {
        assert_eq!(how(None).content, how(Some("")).content);
        assert_eq!(how(None).content, how(Some("   ")).content);
    }

    #[test]
    fn change_task_status_returns_transition_table() {
        let r = how(Some("change task status"));
        for v in ["complete", "fail", "skip", "reset"] {
            assert!(
                r.content.contains(v),
                "status recipe must contain {v}: {}",
                r.content,
            );
        }
        // Must format as a transition table.
        assert!(
            r.content.contains("| Verb |") && r.content.contains("| --- |"),
            "expected markdown table header: {}",
            r.content,
        );
    }

    #[test]
    fn add_follow_up_returns_three_canonical_forms() {
        let r = how(Some("add a follow-up task"));
        let count = r.content.matches("--depended-on-by").count();
        assert!(
            count >= 3,
            "expected >= 3 --depended-on-by forms; got {count} in:\n{}",
            r.content,
        );
    }

    #[test]
    fn where_will_my_add_land_points_at_task_mgr_current() {
        let r = how(Some("where will my add land"));
        assert!(
            r.content.contains("task-mgr current"),
            "recipe must point at task-mgr current: {}",
            r.content,
        );
    }

    #[test]
    fn unrelated_query_returns_no_match_pointer() {
        let r = how(Some("totally unrelated nonsense query"));
        assert!(
            r.content.contains("no recipe matched"),
            "got: {}",
            r.content,
        );
        assert!(
            r.content.contains("task-mgr cheatsheet"),
            "got: {}",
            r.content,
        );
    }

    #[test]
    fn multi_match_joined_with_horizontal_rule() {
        let r = how(Some("sync json"));
        assert!(
            r.content.contains("---"),
            "expected --- separator: {}",
            r.content,
        );
        // Multiple recipes → multiple `## ` headings.
        assert!(
            r.content.matches("## ").count() >= 2,
            "expected >= 2 recipe headings: {}",
            r.content,
        );
    }

    #[test]
    fn deterministic_across_repeated_calls() {
        let r1 = how(Some("sync json"));
        let r2 = how(Some("sync json"));
        assert_eq!(r1.content, r2.content);
    }

    #[test]
    fn case_insensitive() {
        let lower = how(Some("change task status"));
        let upper = how(Some("CHANGE TASK STATUS"));
        let mixed = how(Some("Change Task STATUS"));
        assert_eq!(lower.content, upper.content);
        assert_eq!(lower.content, mixed.content);
    }

    #[test]
    fn format_text_returns_content_verbatim() {
        let r = how(Some("change task status"));
        assert_eq!(format_text(&r), r.content);
    }
}
