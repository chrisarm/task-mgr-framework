# Claude Code Agent Instructions — `task-mgr add` CLI + `<task-status>` Tag + Prompt Hardening

You are an autonomous coding agent implementing the **`task-mgr add` subcommand, `<task-status>` bracket-tag detection, iteration-prompt hardening, and `tasks/*.json` permission guard** for **task-mgr** (Rust CLI at `$HOME/projects/task-mgr`).

## Problem Statement

Claude loop iterations burn enormous amounts of context rewriting `tasks/*.json` — the PRD file is thousands of lines and a single `"passes": true` flip sends the entire file through Claude's conversation, then again on commit. Even worse, dynamically-generated review/fix/refactor tasks are appended by editing the JSON in place, corrupting state the loop engine depends on.

The fix is to make task-mgr the ONLY writer of the PRD JSON. The loop agent:

- Gets its next task via `task-mgr next --claim` (never reads the JSON)
- Creates new dynamic tasks via `echo '{...}' | task-mgr add --stdin`
- Links a new task into an existing milestone's `dependsOn` via `--depended-on-by MILESTONE-X`
- Marks status via a `<task-status>TASK-ID:done</task-status>` bracket tag

This PRD ships the CLI surface, the tag parser, the prompt instructions, and a permission guard that prevents accidental JSON edits.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Internalize quality targets** — read the task's `qualityDimensions`; define what "done well" looks like for THIS task
2. **Map edge cases to implementation plan** — for each `edgeCases` / `failureModes` entry, decide HOW it will be handled before coding
3. **Choose your approach** — state assumptions, consider 2–3 approaches with tradeoffs, pick the best, document briefly in `progress-{{TASK_PREFIX}}.txt`
4. **After coding, self-critique** — "Does this satisfy every `qualityDimensions` constraint? Every edge case? Is it idiomatic and efficient?" — revise before moving on

---

## How to Work

1. Run `task-mgr next --claim` — this selects the best eligible task AND claims it (transitions to `in_progress`). The output contains the task's `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `notes` — everything you need.
2. **DO NOT read `.task-mgr/tasks/task-mgr-add-cli.json`.** It's thousands of lines; it'll blow out your context and you don't need it — task-mgr next prints the claimed task.
3. Read `.task-mgr/tasks/progress.txt` (create if missing) for prior iteration context.
4. Read `CLAUDE.md` for project conventions.
5. Confirm branch with `git branch --show-current` — should be `feat/task-mgr-add-cli`.
6. **Before coding**: read the task's `DO` / `DO NOT` sections from `task-mgr next`'s output. State your approach briefly (one paragraph in progress.txt).
7. **Implement**: code + tests together in one coherent change.
8. **After coding, self-critique**: check each acceptance criterion, especially negative ones and known-bad discriminators.
9. Run quality checks (below) — single-command pattern, sandbox-safe.
10. Commit: `feat: TASK-ID-completed - [Title]` (conventional commits style).
11. Emit `<task-status>TASK-ID:done</task-status>` as the LAST line of your output. The loop engine will parse this, apply the DB transition via `task-mgr`, and sync the PRD JSON. **Do NOT edit the JSON yourself.**

    - Legacy fallback: if you're working mid-feature and the `<task-status>` dispatcher isn't wired yet (it's FEAT-003 of this PRD), use the existing `<completed>TASK-ID</completed>` tag instead — the engine already honors it. Once FEAT-003 lands, prefer `<task-status>`.
12. Append progress to `.task-mgr/tasks/progress.txt`.

---

## Priority Philosophy

1. **PLAN** — Anticipate edge cases. Read `qualityDimensions` first. Consider approaches.
2. **FUNCTIONING CODE** — Pragmatic, reliable code, wired in according to the plan.
3. **CORRECTNESS** — Self-critique after code. Compiles, clippy-clean, tests pass.
4. **CODE QUALITY** — Clean code, good patterns, no warnings.

**Prohibited outcomes:**

- Tests that only assert "no crash" or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context
- New `INSERT` / `UPDATE` / `DELETE` SQL for tasks/task_files/task_relationships — reuse the helpers in `src/commands/init/import.rs`
- New status-transition SQL — dispatch through the existing `complete` / `fail` / `skip` / `irrelevant` command handlers
- Rewriting commit `4eda061` (prompt-too-long + effort ladder) — it's already on `main`

---

## Key Context

### Branch + worktree

- Feature branch: `feat/task-mgr-add-cli` (already created; `git status` should be on it)
- Working tree has partial scaffolding for FEAT-001 left uncommitted by the user on purpose: `src/commands/add.rs` (new), `src/commands/init/parse.rs` (`Serialize` derive), `src/commands/mod.rs` (pub mod), `src/cli/commands.rs` (`Commands::Add` variant), `src/handlers.rs` (`impl_text_formattable!`). **Read these before touching anything — FEAT-001's job is to finalize and commit them, not overwrite.**

### Files you'll touch (by task)

| Task        | Files                                                                                      |
| ----------- | ------------------------------------------------------------------------------------------ |
| FEAT-001    | `src/main.rs`, `src/cli/tests.rs`, existing WIP files (verify + commit)                   |
| FEAT-002    | `tests/add_integration.rs` (new)                                                          |
| FEAT-003    | `src/loop_engine/detection.rs`, `src/loop_engine/engine.rs`, `src/commands/add.rs`, `src/commands/prd_json.rs` (likely new) |
| FEAT-004    | `src/loop_engine/prompt_sections/task_ops.rs` (new), `prompt_sections/mod.rs`, `prompt.rs` |
| FEAT-005    | `src/loop_engine/config.rs`, `src/loop_engine/engine.rs`, `src/loop_engine/claude.rs`     |
| FEAT-006    | `src/cli/commands.rs`, `src/main.rs`, `src/commands/add.rs`, `tests/add_integration.rs`   |
| FEAT-007    | `CHANGELOG.md`, `.claude/commands/tasks.md` (regenerated by gen-docs)                     |

### Key functions / types to reuse (do NOT reinvent)

- `insert_task(conn, &story, prd_default_max_retries)` — `src/commands/init/import.rs:297`
- `insert_task_relationships(conn, &story)` — `src/commands/init/import.rs:370`
- `insert_task_file(conn, task_id, file_path)` — `src/commands/init/import.rs:347`
- `insert_relationship(conn, task_id, related_id, rel_type)` — `src/commands/init/import.rs:356` (reuse for FEAT-006's reverse-link logic)
- `select_next_task(conn, &[], &[], task_prefix)` — `src/commands/next/mod.rs:146` (already used in `resolve_priority` in add.rs)
- `PrdUserStory` — `src/commands/init/parse.rs:11` (now also `Serialize`)
- `complete(conn, task_ids, run_id, commit, force)` — `src/commands/complete.rs:68`
- `fail(...)`, `skip(...)`, `irrelevant(...)`, `unblock(...)`, `reset_tasks(...)` — one per file in `src/commands/`
- `extract_reorder_task_id(output)` + `extract_key_decisions(output)` — `src/loop_engine/detection.rs` — copy the string-slicing idiom for `<task-status>` parsing
- `atomic_write(target, content)` in the WIP `src/commands/add.rs` — factor this into `src/commands/prd_json.rs` in FEAT-003 so FEAT-003 AND FEAT-006 can reuse it

### Key learnings from `task-mgr recall` (prior loop runs)

- **[1069]** Adding a clap enum variant breaks **ALL** exhaustive matches in `src/cli/tests.rs`. Audit every one — patching the first is not enough.
- **[1479]** Use `impl_text_formattable!(TypeName, format_fn)` in `src/handlers.rs` to wire CLI output (already done for `AddResult` in the WIP).
- **[1475]** Task state machine: `complete()` requires the task to be `in_progress` first. `<task-status>:done` on an unclaimed task must log a warning and continue — do NOT auto-claim (that hides bugs).
- **[1480]** Flat match dispatch in `src/main.rs` is idiomatic Rust CLI style. Don't abstract it.
- **[1430]** `cargo test / clippy / fmt` must be run as a SINGLE command with `tee | tail && grep` — split commands hit the sandbox approval flow twice and fail.
- **[1434]** `cargo fmt --check` reformats long multi-arg function calls. Keep new test code within ~100 columns per line to avoid fmt churn.
- **[193]** String `find()` for tag parsing is a footgun (matches unintended substrings). Use the two-stage open+close slice pattern from `extract_reorder_task_id`.
- **[1361]** Wiring failure: when a new code path updates multiple stores (DB + JSON), every test must verify BOTH halves update. A DB-only assertion would let JSON sync silently break.

### Callers to preserve compatibility with

- `src/cli/tests.rs` — multiple exhaustive `match` statements on `Commands` (learning [1069]). Update every one when adding `Commands::Add`.
- `src/handlers.rs` — `output_result()` generic over `TextFormattable`. `AddResult` impl already in WIP — verify it's still there after merging.
- The loop engine's existing `<completed>` and `<reorder>` tag parsing — must NOT regress. New `<task-status>` is additive.
- Commit `4eda061` on `main` — prompt-too-long + effort ladder. Don't rewrite or squash.

---

## What Good Looks Like vs What Bad Looks Like

### Good patterns

- New subcommand handler signature: `pub fn add(db_dir: &Path, input_json: &str, priority_override: Option<i32>) -> TaskMgrResult<AddResult>` — thin shell over an `add_with_conn` helper that takes an already-open `Connection` (unit-testable with in-memory DB)
- Error construction: `TaskMgrError::invalid_state(resource, field, expected, actual)` — never panic, never unwrap
- Transactional multi-insert: open via `conn.unchecked_transaction()?`, call insert helpers, `tx.commit()?`
- Atomic file write: temp file next to the target + `fs::rename` (already in WIP `src/commands/add.rs::atomic_write`)
- Tag parser: open/close slice with iterated position advance — see `extract_reorder_task_id`
- Tests: in-memory SQLite via `Connection::open_in_memory()` + `create_all_tables()`, then direct `INSERT` for minimal seed data

### Bad patterns to avoid

- New top-level `INSERT INTO tasks (...)` SQL — reuse `insert_task`
- Auto-claiming in the `<task-status>:done` dispatcher — hides state-machine bugs
- `output.find("<task-status>")` as a single-shot match — corrupts parse on second tag
- `.unwrap()` / `.expect()` in production code paths — use `?` + custom error variants
- Editing `.task-mgr/tasks/task-mgr-add-cli.json` directly — the whole point of this PRD is to stop doing that. New tasks go through `task-mgr add --stdin`.
- Reading the full PRD JSON during an iteration — use `task-mgr next` / `task-mgr show <id>`

---

## Smart Task Selection

Use `task-mgr next --claim`. It already implements:

- Eligibility filter: `passes: false` AND all `dependsOn` complete AND `requiresHuman` not true
- Score = `priority_base - priority` + 10 × file_overlap + 3 × synergy - 5 × conflict
- Tie-break: total_score DESC, then priority ASC

You don't need to replicate this logic. Just ask `task-mgr next`.

---

## Common Wiring Failures (from prior task-mgr work)

| Symptom                                                     | Cause                                                   | Fix                                                                                           |
| ----------------------------------------------------------- | ------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `non-exhaustive patterns` error in `src/cli/tests.rs`       | New `Commands` variant not added to every match         | Grep for every `Commands::` match and add the new arm                                         |
| `cannot find function format_add_text` in handlers.rs       | Missing `pub use` in `src/commands/mod.rs`              | Re-export `format_text as format_add_text` alongside other commands                           |
| Integration test passes but `task-mgr add` silently no-ops  | Test uses raw SQL to seed tasks, skipping `prd_files`   | Integration test must go through `commands::init::init()` so `prd_files` is populated         |
| `<task-status>` dispatcher not called                       | Wired in detection.rs but no call site in engine.rs     | Grep engine.rs after `analyze_output` to confirm the iteration loop invokes `apply_status_updates` |
| New prompt section never appears in iteration prompt        | Created file but forgot to wire into `prompt_sections/mod.rs` or `prompt.rs` assembly | Grep the prompt builder for your section function name — must be called                      |
| PRD JSON sync passes test but breaks in real loop           | Test didn't register `prd_files` path; add() took DB-only path | Test MUST go through `init()` so the prd_files row exists                                     |

---

## Quality Checks (REQUIRED every iteration, single-command pattern)

```sh
cargo fmt --check 2>&1 | tee /tmp/fmt.txt | tail -5 && grep "Diff" /tmp/fmt.txt | head -5
cargo check --lib 2>&1 | tee /tmp/check.txt | tail -5 && grep "^error" /tmp/check.txt | head -10
cargo clippy --lib -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -5 && grep "^error" /tmp/clippy.txt | head -10
cargo test --lib 2>&1 | tee /tmp/test.txt | tail -5 && grep "FAILED\|error\[" /tmp/test.txt | head -10
```

For integration tests (FEAT-002, FEAT-006):

```sh
cargo test --test add_integration 2>&1 | tee /tmp/it.txt | tail -5 && grep "FAILED" /tmp/it.txt | head -10
```

Fix any failures before committing. Never commit broken code. Pre-existing clippy debt in `tests/concurrent.rs`, `tests/selection_benchmark.rs`, `tests/cli_tests.rs`, `src/commands/curate/tests.rs`, `src/commands/doctor/mod.rs`, `src/db/migrations/v15.rs` is OUT OF SCOPE — only the library (`--lib`) must be clippy-clean.

---

## Review Tasks

### REFACTOR-001 (priority 98)

Audit the delivered files for DRY/complexity/coupling. If issues found:

```sh
echo '{"id":"REFACTOR-FIX-001","title":"...","touchesFiles":["..."]}' \
  | task-mgr add --stdin --depended-on-by REVIEW-001
```

If no issues: emit `<task-status>REFACTOR-001:done</task-status>` with a brief summary in progress.txt.

### REVIEW-001 (priority 99)

Runs full suite + manual smoke. If issues: `task-mgr add --stdin` for FIX-xxx tasks. Smoke test documented in commit message.

---

## Rules

- **One task per iteration**
- **Commit after each task** (conventional commits: `feat:`, `fix:`, `test:`, `docs:`)
- **Read before writing** — always read files first. Especially: read the WIP in `src/commands/add.rs` before editing it.
- **Minimal changes** — only what the task requires.
- **Never edit `.task-mgr/tasks/*.json`** — the whole point of this PRD. New tasks: `task-mgr add --stdin`. Status changes: `<task-status>` tag.
- **Never skip hooks / signing.** If `cargo fmt --check` or a pre-commit hook fails, fix the issue — don't `--no-verify`.
- Work on the correct branch: `feat/task-mgr-add-cli`.
