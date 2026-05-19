//! Intent → recipe table for `task-mgr how`.
//!
//! Static, deterministic, case-insensitive `contains_all` matching:
//! an INTENT entry matches when EVERY token in its keyword bag appears
//! (as a lowercased substring) in the lowercased query. Multiple intents
//! may match the same query; `task-mgr how` prints all matches separated
//! by `---`. No LLM, no embeddings, no I/O — purely a hand-curated map.

/// Ordered list of (keyword bag, recipe text) pairs. Order is the print
/// order when listing intents (`task-mgr how` with no arg) and when
/// multiple intents match. Keep bags 1–3 tokens; longer bags make natural
/// queries hard to hit.
pub const INTENTS: &[(&[&str], &str)] = &[
    // 1. Change task status
    (
        &["status"],
        "## Change task status\n\
         \n\
         | Verb | When | Command |\n\
         | --- | --- | --- |\n\
         | complete | task is finished and verified | `task-mgr complete <id>` (alias `done`) |\n\
         | fail | task hit an unrecoverable error | `task-mgr fail <id> --status blocked` |\n\
         | skip | defer for later, not failed | `task-mgr skip <id> --reason '...'` |\n\
         | unblock | retry a previously blocked task | `task-mgr unblock <id>` |\n\
         | unskip | retry a previously skipped task | `task-mgr unskip <id>` |\n\
         | irrelevant | no longer needed | `task-mgr irrelevant <id> --reason '...'` |\n\
         | reset | force back to todo for re-run | `task-mgr reset <id>` |\n\
         \n\
         From inside a loop iteration emit `<task-status>TASK-ID:done</task-status>` instead.\n",
    ),
    // 2. Add follow-up task (three canonical --depended-on-by forms)
    (
        &["follow-up"],
        "## Add a follow-up task\n\
         \n\
         Three canonical `task-mgr add --stdin --depended-on-by ...` forms — pick by context:\n\
         \n\
         - From a REVIEW iteration spawn (review-spawned fix):\n\
         ```\n\
         echo '{...}' | task-mgr add --stdin --depended-on-by REVIEW-001\n\
         ```\n\
         - Attached to a milestone of a known PRD:\n\
         ```\n\
         echo '{...}' | task-mgr add --stdin --depended-on-by <prd-prefix>-MILESTONE-FINAL\n\
         ```\n\
         - Attached to a contract task in a known PRD:\n\
         ```\n\
         echo '{...}' | task-mgr add --stdin --depended-on-by <prd-prefix>-CONTRACT-001\n\
         ```\n\
         \n\
         Priority is auto-computed (top task priority - 1). Run `task-mgr current`\n\
         first to confirm which PRD will receive the row.\n",
    ),
    // 3. Spawn fix from REVIEW
    (
        &["fix", "review"],
        "## Spawn a fix task from REVIEW\n\
         \n\
         ```\n\
         echo '{\"id\":\"FIX-001\",\"title\":\"...\",\"description\":\"From REVIEW-001: ...\",\"priority\":60}' \\\n\
           | task-mgr add --stdin --depended-on-by REVIEW-001\n\
         ```\n\
         \n\
         Wires the fix into REVIEW-001's dependsOn AND updates the PRD JSON atomically.\n",
    ),
    // 4. Sync JSON into running effort
    (
        &["sync"],
        "## Sync JSON into a running effort\n\
         \n\
         ```\n\
         task-mgr loop init <prd>.json --append --update-existing --dry-run  # preview\n\
         task-mgr loop init <prd>.json --append --update-existing             # apply\n\
         ```\n\
         \n\
         Preserves `status` / `started_at` / `completed_at` on existing rows; refreshes\n\
         description / acceptanceCriteria / notes / files / relationships; adds new\n\
         tasks. Safe against an in-progress loop. Do NOT run bare\n\
         `task-mgr init --from-json` mid-effort — it wipes status fields.\n",
    ),
    // 5. Ratify a tm-decision
    (
        &["ratify"],
        "## Ratify a tm-decision\n\
         \n\
         ```\n\
         task-mgr decisions list                    # see pending decisions\n\
         task-mgr decisions resolve <id> <letter>   # ratify option A/B/...\n\
         ```\n\
         \n\
         Ratification records the choice. If the chosen option diverges from\n\
         the current code, follow up with an empty commit (or a fix-up commit)\n\
         so the decision is discoverable in `git log`.\n",
    ),
    // 6. Recall learnings for a task
    (
        &["recall"],
        "## Recall learnings for a task\n\
         \n\
         ```\n\
         task-mgr recall --for-task <task-id>             # file + type + error pattern match\n\
         task-mgr recall --query \"<text>\"                # vector search (needs Ollama)\n\
         task-mgr recall --query \"<text>\" --allow-degraded   # offline-tolerant\n\
         ```\n\
         \n\
         Confirm useful results with `task-mgr apply-learning <id>`; invalidate\n\
         wrong ones with `task-mgr invalidate-learning <id>`.\n",
    ),
    // 7. Find tasks by file
    (
        &["file"],
        "## Find tasks by file\n\
         \n\
         ```\n\
         task-mgr list --file 'src/foo.rs'                  # exact path or glob\n\
         task-mgr list --file 'src/**/*.rs' --status todo\n\
         ```\n\
         \n\
         Matches tasks whose `touchesFiles` includes a matching path.\n",
    ),
    // 8. Where will my add land
    (
        &["where", "land"],
        "## Where will my next `add` land?\n\
         \n\
         ```\n\
         task-mgr current\n\
         ```\n\
         \n\
         Prints the active prefix, how it was resolved (env / single-prefix /\n\
         from-json / none), and the target PRD JSON path. Run this before any\n\
         `task-mgr add` if you have more than one PRD registered.\n",
    ),
    // 9. Switch active PRD
    (
        &["switch", "prd"],
        "## Switch the active PRD\n\
         \n\
         ```\n\
         export TASK_MGR_ACTIVE_PREFIX=<prefix>        # pins active PRD for this shell\n\
         task-mgr current                              # verify\n\
         ```\n\
         \n\
         `task-mgr list` (no filter) shows which prefixes exist in the DB.\n",
    ),
    // 10. View active PRD
    (
        &["view", "active"],
        "## View the currently active PRD\n\
         \n\
         ```\n\
         task-mgr current                              # text\n\
         task-mgr --format json current | jq '.context'\n\
         ```\n\
         \n\
         Source can be: env (TASK_MGR_ACTIVE_PREFIX), single-prefix (auto),\n\
         from-json (--from-json flag), or none (ambiguous / empty DB).\n",
    ),
    // 11. PRD JSON handling — pairs with #4 so "sync json" matches both
    (
        &["json"],
        "## Don't hand-edit `tasks/*.json`\n\
         \n\
         The PRD task JSON is the source of truth for the loop engine. Editing\n\
         it by hand corrupts loop state (the engine re-imports the file on each\n\
         iteration and may revert your edit). Use the CLI subcommands instead:\n\
         \n\
         - `task-mgr add --stdin` to add new tasks\n\
         - `task-mgr loop init <prd>.json --append --update-existing` to sync\n\
         - Emit `<task-status>` tags to change status from inside a loop\n",
    ),
];

/// Match `query` against [`INTENTS`] using case-insensitive substring
/// `contains_all`. Returns indices into [`INTENTS`] in declaration order.
///
/// Pure function — no I/O, deterministic. Called by [`crate::commands::how::how`].
pub fn match_intents(query: &str) -> Vec<usize> {
    let lc_query = query.to_lowercase();
    INTENTS
        .iter()
        .enumerate()
        .filter(|(_, (bag, _))| {
            // contains_all: every keyword in the bag must appear in the query.
            bag.iter().all(|kw| lc_query.contains(&kw.to_lowercase()))
        })
        .map(|(idx, _)| idx)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_status_intent() {
        let m = match_intents("change task status");
        assert!(!m.is_empty(), "must match the status intent");
        // Recipe must mention status verbs the criteria require.
        let recipe = INTENTS[m[0]].1;
        for v in ["complete", "fail", "skip", "reset"] {
            assert!(recipe.contains(v), "status recipe missing '{v}'");
        }
    }

    #[test]
    fn match_no_match_returns_empty() {
        assert!(match_intents("totally unrelated nonsense query").is_empty());
    }

    #[test]
    fn match_case_insensitive() {
        let a = match_intents("Change Task STATUS");
        let b = match_intents("change task status");
        assert_eq!(a, b);
    }

    #[test]
    fn match_sync_json_returns_multiple_recipes() {
        // 'sync json' must match >1 intent (acceptance criteria).
        let m = match_intents("sync json");
        assert!(
            m.len() >= 2,
            "expected >= 2 matches for 'sync json', got {m:?}"
        );
    }

    #[test]
    fn match_where_will_my_add_land_hits_current_recipe() {
        let m = match_intents("where will my add land");
        assert!(!m.is_empty());
        let recipe = INTENTS[m[0]].1;
        assert!(
            recipe.contains("task-mgr current"),
            "recipe must point at task-mgr current: {recipe}"
        );
    }

    #[test]
    fn intents_recipes_all_nonempty() {
        for (bag, recipe) in INTENTS {
            assert!(!bag.is_empty(), "keyword bag must not be empty");
            assert!(
                !recipe.trim().is_empty(),
                "recipe text must not be empty for bag {bag:?}"
            );
        }
    }
}
