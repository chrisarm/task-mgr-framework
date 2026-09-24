# Claude Code Agent Instructions

You are an autonomous coding agent implementing **loop stop locations** for **task-mgr**.

## Problem Statement

A running loop notices `.stop-<prefix>` only in `ResolvedPaths.tasks_dir` (the PRD parent from `resolve_paths`). Startup remaps `paths.prd_file` to the worktree copy and leaves `tasks_dir` alone. The banner prints that path relative to the process cwd, and the loop-operator skill says `touch tasks/.stop-<prefix>` from the repo root. The file lands in the main checkout and the worktree loop keeps going.

Batch's between-PRD check is `.task-mgr/tasks/.stop` (`cli.dir.join("tasks")`), not `tasks/.stop`.

Fix: watch the launch `tasks/` directory, the feature worktree `tasks/`, and the main checkout `tasks/` for a prefix file whose mtime is strictly after one process-start `SystemTime`, and add `task-mgr loop stop --prefix` so the operator does not choose a directory. `run_batch` calls `engine::run_loop` in-process and copies that same timestamp into every inner `SignalLocations`. Leave account-wait `check_stop_signal(tasks_dir, None)` alone. The in-flight iteration still finishes; operator stop stays exit 0 and is not `HorizonStopped`.

Read `tasks/loop-stop-locations.md` for the folded scope. Do not reopen the architect rejections listed there.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). If a **Project Verification Skills** section applies to this task, follow that skill after the language gate. Don't add a separate self-critique step; the linters, type-checker, targeted tests, and (when present) the project verification skill catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context
- `SystemTime::now()` at `run_loop` entry or beside the stale extra-dir sweep
- `cleanup_signal_files_for_prefix` on an extra directory
- Absolute stop paths added to `format_session_banner`
- Treating flat `task-mgr loop <prd>` argv as a live loop-stop target
- Conflating operator stop with `HorizonStopped`

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy` output
- Rust: All tests pass with `cargo test`
- Rust: `cargo fmt --check` passes
- No breaking changes to existing APIs unless explicitly required

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything global is already embedded in **this prompt file**. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/loop-stop-locations.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need | Command |
| --- | --- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` using the task ID from `## Current Task` |
| List remaining tasks (debug only) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings relevant to a task | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up task (review spawns) | `echo '{...}' \| task-mgr add --stdin --from-json tasks/loop-stop-locations.json --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| --- | --- |
| `tasks/loop-stop-locations-prompt.md` | This prompt file (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — tail for recent context, append after each task |

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

---

## Your Task (every iteration)

1. Work the task in `## Current Task`.
2. Pull only the progress context you need. Skip entirely on the first iteration.
3. Recall focused learnings with `task-mgr recall --for-task <TASK-ID>`. Do not Read `CLAUDE.md` in full. When a verification skill applies, Read that SKILL.md at verification time.
4. Verify branch — `git branch --show-current` matches the `branchName` task-mgr printed.
5. Think before coding. For `estimatedEffort: "high"` or `modifiesBehavior: true`, name one rejected alternative.
6. Implement — code and tests in one change.
7. Run the floor gate below. If the verification skill covers this task, follow it after the language gate. If it is blocked, emit `<promise>BLOCKED</promise>`.
8. Commit: `feat: <TASK-ID>-completed - [Title]`.
9. Emit `<task-status><TASK-ID>:done</task-status>`. Do not edit the JSON.
10. Append one progress block terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

When a task declares `modifiesBehavior: true`:

1. Read the callers named in the task description.
2. Decide per-caller: `OK`, `BREAKS` (split via `task-mgr add --stdin`, then skip the original), or `NEEDS_REVIEW`.
3. Account-wait functions are listed so you leave them alone. That is `OK` by not editing them.

---

## Quality Checks

Per-iteration tasks run a **scoped** gate. **REVIEW-001** runs the full gate and must leave the repo green, including pre-existing failures.

### Per-iteration scoped gate

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test -p task-mgr signals::
cargo test -p task-mgr <module_or_fn_name>
```

Widen to `cargo test -p task-mgr` if the scope is unclear. Do not run an unscoped workspace suite during FEAT iterations.

The JSON principles mention `bash bin/gate`. This repo's per-iteration critic is the scoped cargo gate above. REVIEW-001 runs the unscoped suite. Paste the gate's success line in the progress trailer.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

If more than about 12 failures are clearly unrelated, fix this diff, spawn one `FIX-xxx` via `task-mgr add --stdin --from-json tasks/loop-stop-locations.json --depended-on-by REVIEW-001`, and emit `<promise>BLOCKED</promise>`. Below that threshold, fix them.

---

## Project Verification Skills

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This change maps to:** `loop-stop` (FEAT-003, REVIEW-001). The harness refuses `loop run` and `batch run`. Prove `loop stop` help, missing-record refusal, and invalid-prefix refusal. Report the live-loop path skipped.

**Per-iteration:** FEAT-003 and any FIX spawned from it Read the skill and `features/loop-stop.md` after the language gate.

**REVIEW-001:** drive `loop-stop`. A skipped live loop is reported skipped, not verified through another command.

**Blocked skill:** emit `<promise>BLOCKED</promise>` with the unmet precondition.

---

## Key Learnings

- **[623]** `tasks_dir` is the PRD parent from `resolve_paths`, not the project root and not the remapped worktree PRD.
- **[2798]** The loop reads the worktree copy of the PRD. That copy's directory is not automatically the stop directory.
- **[1885]** Operator `.stop` exits 0. SIGINT/SIGTERM exits 130. Do not mix them.
- **[5558]** A mid-wait `.stop` during Ask is `StopSignaled`. Horizon `onLow: stop` is not operator stop. Do not retarget account waits.
- **[5567]** Ask `onLow: stop` is `HorizonStopped`, not operator `StopSignaled`.
- **[5469]** `QuotaAccountAction::Stop` must not reuse `UsageCheckResult::StopSignaled`.
- **[1359] / [1304]** Some trees keep task JSON under `.task-mgr/tasks/`. That is why batch's canonical dir is `db_dir.join("tasks")`. It is not a reason to add `.task-mgr/tasks` as an extra candidate whenever the directory exists.
- **[5620] / [5595]** Commit feature source and docs. Do not hand-edit PRD task JSON; the loop owns `passes`.
- **[5835]** Do not treat process cwd as the worktree root when deciding path identity.

---

## CLAUDE.md excerpts

- Stop files are `.stop` / `.stop-<prefix>` under the tasks directory the loop resolved. Cleanup with a prefix also removes the global file in that same directory so the fallback cannot stick.
- `recover_in_progress` on loop exit resets `in_progress` to `todo` only. Do not change recovery while adding a stop path.
- `ui::*` is product UX. `tracing` is diagnostics. The banner stop hint goes through the existing display helpers.
- One loop per `.task-mgr` database. A stray worktree DB is ignored; `resolve_db_dir` anchors a worktree default at the main repo.
- Do not edit the `TASK_MGR:BEGIN` / `TASK_MGR:END` block by hand.

---

## Data Flow Contracts

`SignalLocations` stays inside `loop_engine`. The CLI reads a JSON record, it does not construct candidates from the caller's cwd.

```text
started_at: SystemTime
  captured once before run_loop / run_batch (loop-run arm, or the start of run_batch)
  LoopRunConfig.started_at in src/loop_engine/engine.rs
  run_batch copies that same value onto every inner LoopRunConfig
  run_loop and the stale sweep read it; they do not call SystemTime::now()

resolve_paths(...).tasks_dir: PathBuf     // canonical for a loop; pre-remap
git::main_repo_root_at(&source_root): Option<PathBuf>   // None drops that candidate only
SignalLocations { canonical: PathBuf, extras: Vec<PathBuf>, started_at: SystemTime }

extras, after canonicalize, dropping missing dirs and canonicalize errors
  (a canonicalize error does not abort startup):
  launch: cwd if its final component is "tasks", else cwd/tasks
  actual_worktree_path/tasks when Some (not a slot worktree)
  main_repo_root_at(source_root)/tasks when Some
  if a candidate equals canonical, remove it from extras (no-mtime rule stays)

stop_requested(&locations, prefix: Option<&str>) -> bool
  canonical: check_stop_signal(&canonical, prefix)     // no mtime
  extras + Some(prefix): .stop-{prefix} mtime > started_at   // equal is stale
  extras + None: ignored

batch_stop_requested(&locations) -> bool
  canonical: db_dir.join("tasks") global .stop exists   // no mtime
  extras: global .stop mtime > started_at               // not prefix files
```

Exit cleanup: canonical uses `cleanup_signal_files_for_prefix` (prefix and global files). Each extra deletes `.stop-<prefix>` and `.pause-<prefix>` only. Do not call `cleanup_signal_files_for_prefix` on an extra.

Banner: the Stop row inside the box, from `format_session_banner`, is `task-mgr loop stop --prefix <prefix>`. After `print_session_banner` returns, `ui::emit` (stderr) prints the canonical and extra absolute paths untruncated. Do not add those paths to `format_session_banner`. The Pause row inside the box may stay truncated.

Run record path: `{resolve_db_dir}/loop-runs/<validate_prefix>.json` or `loop-runs/batch.json`.

```text
cmdline: Vec<OsString> from /proc/<pid>/cmdline split on NUL
qualifies when some element's final path component is exactly "task-mgr"
  AND other elements are ("loop" AND "run") OR ("batch" AND "run")
not a substring; "task-mgr-helper" and a single "loop-run" token do not qualify
flat "task-mgr loop <prd>" has no "run" token and fails closed
after the write, re-check; if the pid died, unlink only the path this command wrote
```

`validate_prefix` allows `[a-zA-Z0-9.-]` and rejects empty. Call it before `Path::join`. Do not take `LockGuard`.

---

## Reference

At loop start, after `started_at` is captured and the prefix run record is written, delete this prefix's extra-dir `.stop-<prefix>` when its mtime is before or equal to that same `started_at`. `ui::emit` (not tracing-only) names the absolute path, says it was removed because it predates this process and would have been ignored, and tells the operator to run `task-mgr loop stop --prefix <prefix>` or create the file again at that same absolute path or at `<canonical>/.stop-<prefix>`. A failed delete still warns and does not abort startup. Do not delete a canonical prefix file, a global `.stop`, a `.pause-<prefix>`, or another prefix's file. Do not sweep stale extra-dir global files at batch start. Do not call `SystemTime::now()` next to this sweep.

Rejected alternative (do not implement): thread `SignalLocations` through `wait_for_usage_reset`, `wait_for_ask_ttl_inner`, and `transient_backoff_wait`. Those four `check_stop_signal(tasks_dir, None)` sites only see a global file, and the signature change touches the usage-gate seams and about ten tests. A prefix stop is observed at the next iteration or wave boundary. Do not add a field to account param structs, `UsageGateFn`, or `ResetWaitFn`.

Rejected alternative: `git rev-parse --git-common-dir` from an implicit cwd. Use `git::main_repo_root_at(source_root)`.

Rejected alternative: call `SystemTime::now()` at `run_loop` entry so each inner PRD gets a later clock. A file created after the batch process started must stay fresh for later inner loops.

### Accepted residuals (do not file tasks)

- Prefix stop does not interrupt an account wait.
- Deprecated flat `task-mgr loop <prd>` has no `run` argv token, so `loop stop` fails closed. Do not treat it as a live target.
- Early `initialize_loop` Err paths may leave a prefix run record. Overwrite it on the next successful start.
- The Pause row inside the banner box may stay truncated.
- Do not conflate operator stop with quota `HorizonStopped`.

---

## Common Wiring Failures (REVIEW-001 reference)

- `LoopCommand::Stop` added to the enum but not matched in `src/main.rs`.
- Helper tested and never called from `iteration.rs`, `wave_orchestration.rs`, and `batch.rs`.
- Canonical taken from `paths.prd_file.parent()` after the worktree remap.
- `SystemTime::now()` inside `run_loop` or beside the stale sweep, so inner PRDs do not share the batch clock.
- `cleanup_signal_files_for_prefix` called on an extra, which also deletes a global `.stop` there.
- Absolute paths appended inside `format_session_banner` instead of `ui::emit` after `print_session_banner`.
- Cmdline match is a substring, or a dead pid unlinks a stop file this command did not write.
- `loop stop` writes next to the operator's cwd when the run record is missing.
- Skill Abort headings updated while Step 1 still sets `STOP_FILE=tasks/.stop-<PREFIX>`.

---

## Review Tasks

| Review | Priority | Spawns | Focus |
| --- | --- | --- | --- |
| REFACTOR-001 | 98 | `REFACTOR-FIX-xxx` (50-97) | DRY, the mtime rule and cmdline check each exist once |
| REVIEW-001 | 99 | `FIX-xxx` (50-97) | Wiring, full suite, verify-task-mgr `loop-stop` |

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 60,
  "passes": false,
  "taskType": "implementation",
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --from-json tasks/loop-stop-locations.json --depended-on-by REVIEW-001
```

---

## Progress Log Format

```markdown
## <Date> - <TASK-ID>
Commit: <hash>
Gate: <scoped command and result>
<what changed and which known-bad test covers it>
---
```
