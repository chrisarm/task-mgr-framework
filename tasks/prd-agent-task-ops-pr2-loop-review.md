# Loop Review: Agent task-ops UX PR-2 — `task-mgr update` + `humanReviewOutcome`

- **Worktree**: `/home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-agent-task-ops-pr2`
- **Branch**: `feat/agent-task-ops-pr2` (JSON `branchName` matches)
- **HEAD**: `625c28e` (17 commits beyond `main`; tip is loop reconcile after `e4f2bcd` REVIEW-001)
- **Reviewer**: rust-python-code-reviewer (read-only) + inline PRD coherence
- **Date**: 2026-09-18

## Summary

PR-2 ships a real `task-mgr update --stdin/--json` load-merge-write path that does not call `init::import::update_task`, does not SET `status` / `archived_at` / `priority` / `id`, and fail-closes overlay `status`/`passes` / unknown keys / type-null before any write. `humanReviewOutcome` is `Option<Value>` on `PrdUserStory` and `AddTaskInput` with no DB column; JSON-only overlays `invalid_state` on missing path or patch `Err`, while mixed overlays keep pin 11 (DB commits, skip/warn, never `export`). Pin/write policy matches add (`default_prd_roots` → `resolve_context_with_roots`, shared `preflight_from_json_path` / `refuse_unpinned_write`), clap `Commands::Update` is distinct from `RunAction::Update`, and CLARIFY docs/hints/verify artifacts point at `update --stdin` then `complete`.

**Verdict: CLEAN**

## Code Review Summary

- **Files reviewed**: 20 load-bearing (plus verify artifacts and PRD)
- **Critical findings**: 0
- **High findings**: 0
- **Medium findings**: 0
- **Low findings**: 3

Grep of production `update.rs`: no `delete_task_relationships` call; `update_task` only in `#[cfg(test)]` (CONTRACT-003 re-import survival); no `SET status` / `priority` / `id`.

## Critical

None.

## High

None.

## Medium

None.

## Low

1. **`src/commands/mod.rs` + `src/main.rs:884-887` — overlay vs run-session `update` namespace.** `pub mod update` coexists with `pub use run::update`. Overlay dispatch is correctly path-qualified (`commands::update::update`); `commands::update(...)` is run-session. Documented at the call site; still a future footgun if a caller uses the re-export.

2. **`src/commands/add.rs:97` — dead serde attr.** `AddTaskInput` only derives `Deserialize`, so `skip_serializing_if = "Option::is_none"` on `human_review_outcome` does nothing. The required copy is `into_prd_user_story` (`add.rs:153`). Harmless; `PrdUserStory` serialize-omit is the real skip.

3. **`src/commands/update.rs` size (~2159 lines).** Validator + writer + ~900 lines of tests in one file. Matches CONTRACT-001/002 ownership; not a defect.

## Coherence Assessment

- **PRD alignment**: FULL
- **Deviations found**: None that contradict pins 5–7, 10–13, 16–17, 19, 21 or US-001–008.
- **Cross-PRD contract status**: CONTRACT-001/002/003 text in `src/commands/CLAUDE.md` matches the writer. `import::update_task` SQL unchanged (re-import revive). No PR-3 export scoping, no remapper/add clap rewrite.

User-story spot-check:

| Story | Status | Evidence |
| --- | --- | --- |
| US-001 CONTRACT-001 | Satisfied | Partial `UPDATE` + scoped `dependsOn` delete (`update.rs:771-910`); JSON-only refuse (`613-627`); mixed pin 11 (`630-651`) |
| US-002 CONTRACT-002 | Satisfied | Value walk; lifecycle before unknown keys (`75-99`); type/null table bound to `PrdUserStory` fields |
| US-003 CONTRACT-003 | Satisfied | Field on `parse.rs:93-94` and `add.rs:97-98`; pragma tests; verify `hro-reimport` |
| US-004 `patch_user_story` | Satisfied | Skip overlay `id`; `strip_prefix_in_id_array`; `atomic_write(..., command)`; extra-key test |
| US-005 clap + pin | Satisfied | `Commands::Update`; preflight before parse; `default_prd_roots` → `with_roots`; help says pin |
| US-006 hints/docs | Satisfied | `update` removed from `WRONG_SUBCOMMAND_HINTS`; edit/change → `update --stdin`; enhance + regenerated `CLAUDE.md`; `how` clarify test |
| US-007 worktree | Satisfied | `live_worktree_file_exists_update_*`, fallback-to-main, `--from-json` not remapped (`tests/worktree_db_resolution.rs:652-754`) |
| US-008 verify | Satisfied | Feature file + README link; artifacts under `.claude/skills/verify-task-mgr/artifacts/20260918T181305-*` / `181350-*` / `181400-*` |

## Residual (accepted, not findings)

- Inner `humanReviewOutcome` keys are opaque (`Option<Value>`).
- `init::import::update_task` still full-row SETs and clears `archived_at` (different verb).
- `maxRetries` accepts negative integers (PRD: integer, not unsigned).
- Id-only overlay is rejected after `LockGuard` but before any `UPDATE`.
- Verify harness does not cover worktree live path (owned by rust tests, as specified).
- REVIEW-001 full-suite green was not re-run in this review.

Working tree at review time: untracked `tasks/.loop-pid-a8855e28` only (loop pid file). Product sources are committed.

## Action Items

None required for merge from this review. Optional nits above may be cleaned in a follow-up; do not spawn CODE-FIX tasks.

`/compound` was not run (headless review instruction). Review is clean; a human may run `/compound` to capture forward-looking learnings.
