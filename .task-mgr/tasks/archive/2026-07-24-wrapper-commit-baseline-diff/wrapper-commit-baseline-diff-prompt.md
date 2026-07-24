# Claude Code Agent Instructions

You are an autonomous coding agent implementing **wrapper-commit baseline-diff fix** for **task-mgr**.

## Problem Statement

In sequential `task-mgr loop run`, when a task completes but the spawned agent couldn't self-commit (scoped-permission footgun), the loop commits "on the agent's behalf" via `git_reconcile::wrapper_commit` (src/loop_engine/git_reconcile.rs). That function stages with **`git add -A`**, which cannot tell the iteration's own changes from files already dirty in the operator's working tree. In the incident that prompted this, it swept ~254 unrelated uncommitted files (notebooks, generated `rag_chunks/*.md`, probe scripts) into the feature branch — twice.

The fix: snapshot the working-tree dirty set **before** the agent runs (a pre-iteration baseline), and at commit time stage only the paths that became dirty *after* the baseline (set difference). On a clean tree at iteration start, behavior is identical to today; the change only bites when the tree was already dirty — exactly the incident scenario. The wave/parallel path is unaffected (`wrapper_commit = false`; slot merge-back carries the commit).

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). **Safety over convenience** — when the baseline is unknown, SKIP the commit rather than risk sweeping; never fall back to `git add -A`. For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why.

**Prohibited outcomes:**

- Any `git add -A` (or `git add .`) WITHOUT a `-- <pathspec>` scope in the wrapper-commit path — that is the exact bug being fixed
- Falling back to staging everything when the baseline is None — must skip+warn instead
- Tests that only assert 'no crash' or check the return type without verifying which paths were committed vs left dirty
- A `-z` parser that mis-handles rename entries (off-by-one consuming the FROM field) or treats the Y status slot as a rename indicator
- Committing gitignored or pre-existing-dirty files (the regression this fix exists to prevent)
- Error messages that don't identify what went wrong

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` in `## Current Task` are layered on top. If any fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: Scoped tests pass with `cargo test -p task-mgr <module>` (full suite at REVIEW-001)
- Rust: `cargo fmt --check` passes
- No breaking changes to existing public APIs unless explicitly required (`wrapper_commit` is `pub(crate)` — its signature change is in-scope)
- Status mutations (if any) go through `TaskLifecycle`, never raw UPDATE SQL — N/A here (no status writes)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything global is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/wrapper-commit-baseline-diff.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Inspect this iteration's task           | `task-mgr show <TASK-ID>` using the task ID from `## Current Task`                                                                                                                 |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID`                                                                                                                                       |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001`                                                                                                                |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`)                                                            |

### Files you DO touch

| File                                              | Purpose                                                                |
| ------------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/wrapper-commit-baseline-diff-prompt.md`    | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`                       | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — never Read the whole log:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read on the first iteration (file won't exist). Create it with a one-line header if missing.

---

## Your Task (every iteration)

1. **Work the task in `## Current Task`** — the loop engine already selected and claimed it. Use `task-mgr show <TASK-ID>` to re-inspect if needed. If `## Current Task` says there is no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.
2. **Pull only the progress context you need** — most iterations want just the most recent section.
3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do NOT Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly. Do NOT Read `CLAUDE.md` in full — the relevant excerpts are below.
4. **Verify branch** — `git branch --show-current` matches `feat/wrapper-commit-baseline-diff`. Switch if wrong.
5. **Think before coding** — state assumptions; for each `edgeCases`/`failureModes` entry note how it's handled; for cross-module data access consult **Data Flow Contracts** below or grep 2-3 call sites.
6. **Implement** — single task, code and tests in one coherent change.
7. **Run the scoped quality gate** (Quality Checks below — scoped tests only).
8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).
9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`.
10. **Append progress** — ONE block, terminated with `---`.

To request a different pick on the **next** iteration, emit `<reorder>TASK-ID</reorder>`. Never use `next --claim` in a loop iteration.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

Both FEAT tasks are `modifiesBehavior: true` (they change `wrapper_commit`'s staging behavior and its call site).

1. Read the named caller: `wrapper_commit` is called only from `src/loop_engine/reactions/post_completion.rs` (the `for id in completed_ids` loop, ~234-242). It is `pub(crate)`; no other production caller exists (grep `wrapper_commit` to confirm — the other hits are the wave/shim construction of `wrapper_commit: bool`, a different thing).
2. FEAT-001 changes the signature → FEAT-002 updates that one call site. The CONTRACT-001 exhaustive destructure on `PostCompletionParams` makes any missed construction site a compile error.

---

## Quality Checks

### Per-iteration scoped gate (FEAT tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test -p task-mgr loop_engine::git_reconcile        # FEAT-001
cargo test -p task-mgr --test reaction_parity            # FEAT-002 (plus the line above)
```

Scope from `touchesFiles`. **Do NOT** run the entire workspace suite during FEAT iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

If ANY test fails — including pre-existing failures — REVIEW-001 fixes them (default: attempt every failure). **Gotcha:** mass `cargo test` failures whose errors all name a removed `…-slot-N` worktree path are stale shared-target test binaries, NOT a regression — `touch tests/<binary>.rs` and rebuild before concluding. Escape hatch: >~12 clearly-unrelated failures → fix what's attributable to this diff inline, spawn one `FIX-xxx` for the rest, `<promise>BLOCKED</promise>` with that ID.

---

## Common Wiring Failures (REVIEW-001 reference)

- Baseline captured but not threaded → wrapper-commit silently reverts to the bug. Grep the full chain: `run_loop` capture → `PostCompletionParams.git_status_baseline` → `react_to_completions_inner` destructure → `wrapper_commit` 4th arg.
- New `HashSet` field but missing `use std::collections::HashSet;` import → compile error.
- Wave/shim/parity construction sites not updated → CONTRACT-001 exhaustive-destructure compile error (good — that's the safety net).
- An unscoped `git add -A` left behind anywhere in the staging path.

---

## Review Tasks

| Review         | Priority | Spawns (priority)                  | Focus                                                                 |
| -------------- | -------- | ---------------------------------- | -------------------------------------------------------------------- |
| REFACTOR-001   | 98       | `REFACTOR-FIX-xxx` (50-97)         | DRY (the two porcelain parsers), function length, coupling, clarity  |
| REVIEW-001     | 99       | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) | No unscoped `git add -A`, baseline fully wired, full-suite green, docs |

Use the **rust-python-code-reviewer** agent when reviewing. Spawn follow-ups:

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 60,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

`--depended-on-by` wires the task into REVIEW-001's `dependsOn` and syncs the JSON atomically — don't edit JSON yourself. Commit `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, emit `<task-status><REVIEW-ID>:done</task-status>`.

---

## Progress Report Format

APPEND to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight (~10 lines):

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

---

## Learnings Guidelines

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval scored for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries

Record learnings with `task-mgr learn` (don't append to the learnings files directly). Write concise 1-2 line learnings.

---

## Stop and Blocked Conditions

### Stop Condition

Before `<promise>COMPLETE</promise>`: verify ALL tasks `passes: true`, no new tasks created in final review, REVIEW-001 passed with full suite green.

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked: document in the progress file, create a clarification task via `task-mgr add --stdin --depended-on-by <blocked-task>` (priority 0), then:

```
<promise>BLOCKED</promise>
```

---

## Key Learnings (from task-mgr recall)

Treat as authoritative — do NOT Read the learnings files unless a task needs one not here.

- **[5270]** Feature branches accumulate unrelated dirty files (task JSON, command files, progress files); staging must be scoped to the iteration's own changes — this fix is the structural version of that discipline.
- **[1808] / [2966] / [3302]** `tasks/progress-*.txt` are gitignored by design and never appear in `git status --porcelain` — the baseline diff excludes them automatically; no special-casing needed.
- **[2445]** `git stash`/status without explicit untracked flags only covers tracked files — mirror this with `--untracked-files=all` so the baseline and current captures both see untracked entries at file granularity.
- **[1762] / [1645]** Verify staging with `git diff --staged` and keep staging minimal/focused — avoid sweeping in adjacent dirty files mid-iteration.
- **[1225] / [413]** `git commit` with nothing staged fails with "no changes added to commit" — the empty-diff path must `return None` BEFORE attempting the commit, not commit-then-fail.
- **[4747]** When these tests run under the loop, git mutations (`git rm`/`git commit`) need `dangerouslyDisableSandbox: true` to avoid approval prompts. (In plain `cargo test` the `std::process::Command` git calls are unaffected.)
- **[2305]** (Adjacent) the slot-0 ephemeral guard rejects `slot==0` at the glob/parse boundary — unrelated to this change, but confirms the loop engine's git code already keys on strict parsing; mirror that rigor in the `-z` parser.

---

## CLAUDE.md Excerpts (only what applies to this change)

From the root and `src/loop_engine/CLAUDE.md` — the only CLAUDE.md content you need:

- **Wrapper-commit (#8)** is sequential-only (`wrapper_commit = true`); the wave path passes `wrapper_commit = false` because slot merge-back already carries the commit. (`src/loop_engine/CLAUDE.md:558` — the table row you must update to the baseline-diff contract.)
- **CONTRACT-001 single-home lock**: `PostCompletionParams` is destructured exhaustively (no `..`) in `react_to_completions_inner`. Adding a field is a compile error until every coordinator/construction site accounts for it — lean on this; don't use `..`.
- **`ui::*` vs `tracing`** (CONTRACT-LOG-001): operator-facing contract lines go through `ui::emit` (stderr, exact bytes), internal diagnostics through `tracing`. The None-baseline "could not safely stage" warning is operator-facing → `ui::emit`.
- **Status mutations** in `loop_engine/` go through `TaskLifecycle` verbs, never raw `UPDATE tasks SET status` SQL. (This change writes no status — listed for completeness.)
- **Gitignored progress files** (`tasks/progress-*.txt`, `.task-mgr/logs/`) are covered by init's managed `.gitignore` block; `.task-mgr/` (the SQLite DB) is NOT in that block, so in a target repo the DB path can sit in the baseline and is correctly excluded from the wrapper commit (benign improvement, not a regression).
- **Stale slot-worktree test binaries**: mass `cargo test` failures all naming a removed `…-slot-N` path are stale shared-target binaries, not a code regression — `touch tests/<binary>.rs` and rebuild.

---

## Data Flow Contracts

Verified access pattern for the baseline — use exactly; do not guess types.

```
Capture (orchestrator.rs run_loop, immediately BEFORE run_iteration ~329):
    let git_baseline: Option<HashSet<String>> =
        git_reconcile::capture_status_paths(&working_root);   // working_root: PathBuf, in scope

Thread (orchestrator.rs sequential PostCompletionParams ~422):
    git_status_baseline: git_baseline.as_ref(),               // Option<&HashSet<String>>

Field (reactions/post_completion.rs, PostCompletionParams<'a> ~71-94):
    pub git_status_baseline: Option<&'a HashSet<String>>,

Destructure (react_to_completions_inner ~218-229, exhaustive, no `..`):
    let &PostCompletionParams { /* ...existing fields..., */ git_status_baseline } = params;

Consume (post_completion.rs wrapper-commit loop ~234-242):
    git_reconcile::wrapper_commit(working_root, id, "loop wrapper commit", git_status_baseline)

Signature (git_reconcile.rs, FEAT-001):
    pub(crate) fn wrapper_commit(
        working_root: &Path, task_id: &str, message_suffix: &str,
        baseline: Option<&HashSet<String>>,
    ) -> Option<String>

Other construction sites pass None (wrapper_commit=false there):
    wave_scheduler.rs ~1381 ; orchestrator.rs ~912 (human-review shim) ; tests/reaction_parity.rs ~2282
```

`-z` porcelain format (FEAT-001 parser): split `git status --porcelain -z --untracked-files=all` output on `\0`. Each entry is `XY<sp><path>` (X = index status, Y = worktree status). A rename/copy is keyed on **X only** (`bytes[0] == b'R' || b'C'`) and spans TWO fields: the entry holds the **new** path (TO), the **next** NUL field holds the **old** path (FROM) — insert both. Drop the trailing empty field from the split.

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep the scoped gate green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- Work on the correct branch: **feat/wrapper-commit-baseline-diff**
