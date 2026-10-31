# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Provider-Aware Routing, Phase 1: Grok limit detection and session-id cleanup** for **task-mgr**.

## Problem Statement

Two Grok-runner bugs come before any routing work:

1. **P1c.** Grok prints `Error: You’ve hit the rate limit for your plan. Upgrade your account or try again later.` on stderr (curly apostrophe, no reset time). `GrokRunner::spawn` sniffs stderr only for auth and 5xx, so the iteration becomes a crash, burns crash budget, and can auto-block tasks. FIX-001 already added the clause inside `is_rate_limited` for Claude stdout. This loop does not edit `detection.rs` and does not call `is_rate_limited` from the Grok stderr sniff. The sniff matches only the case-insensitive phrase `rate limit for your plan`. `TaskMgrError::RunnerRateLimited` is already declared: return it. Do not add a second variant. Map it to `IterationOutcome::RateLimit` on the sequential and wave paths. A non-zero stderr buffer that contains `429` and `rate` but not that phrase is not this error. A `<completed>` tag is not stolen on either exit arm. Auth still wins. `dispatch` still cleans up only on `Ok`.
2. **P1e.** `GrokRunner::spawn` finds its session directory by diffing `~/.grok/sessions/<urlencoded cwd>/` before and after the run and picks the newest by mtime. An interactive Grok session in the same cwd can be picked and deleted. Fix: pass `--session-id <uuid>` (grok 1.0.50 supports it) and clean up exactly that directory, like the Claude runner.

Source PRD: `tasks/prd-model-routing.md` (covers all phases; this loop is P1c + P1e only). Do not re-read it in full unless a task names a section you do not have below.

## Pins

These sentences are the acceptance. Do not reopen D1–D6. Do not restore a cut phase in this effort.

1. Keep P6.1. The tier line stays a phase so a null Grok standard rung is not read as "no route." Display-only, after P1c's banner sites are committed. It adds no routing rule.
2. Cut P6.3. Operators edit the class block in config JSON. Restore the subcommands after P1d only if hand-edited lists are actually mis-set.
3. Cut P6.5. Review does not move to another provider in this effort. D2 stands. Restore from the P6 guide after the floor, never in the same edit, and do not enable reviewCascade.
4. Cut P3. One project config is the rollout unit. Restore FR-P3 only when two PRDs in one project must route differently, and do not treat unknown keys as a validation error.
5. Cut v22. One routing-events.jsonl line per attempt, and stats reads that file. No migration. Restore a migration only if that file cannot be kept for the comparison window.
6. Parallel sets, cuts applied. One loop per checkout. A second checkout may run only a file-clean set from guide §2, with P3, P6.3, P6.5, and v22 removed. P6.1 stays out of any set that still edits iteration.rs, slot.rs, or iteration_pipeline.rs.
7. D1–D6 in the parent PRD stay. This acceptance does not reopen them.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Satisfy this task** — meet the acceptance criteria and prohibited outcomes in this prompt.
2. **Plan edge-case handling** — for each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it will be handled before coding.
3. **Pick an approach** — state assumptions. For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.
4. **Red first** — write the task's tests, run them, and paste the failing output into your progress block before changing production code.

After writing code, run the scoped quality gate. This PRD does not map onto a verify-task-mgr feature. FIX-002 and FIX-003 name `.claude/skills/verify-task-mgr/SKILL.md` for the operator proof. Do not hand-roll a drive. Do not invent a second harness and do not launch `loop run`.

---

## Priority Philosophy

In order: **PLAN** (ANALYSIS-001 first) → **PHASE 2 FOUNDATION** (typed signals, exact identifiers) → **FUNCTIONING CODE** → **CORRECTNESS** (red-first, idempotent) → **CODE QUALITY** → **POLISH**.

**Prohibited outcomes:**

- Classifying Grok's plan rate-limit message as Crash, Empty, or TransientBackend
- Adding a bare 'rate limit' pattern, or narrowing or removing any existing is_rate_limited pattern
- Appending a marker or the stderr text to RunnerResult.output to signal a limit (use a typed error)
- Adding Anthropic usage I/O or a Claude probe because of a Grok limit. The existing Claude-enabled `load_usage` on `RateLimit` stays until P1b. This phase does not edit `account.rs`
- Keeping the before/after session-dir diff or pick_newest_by_mtime alongside --session-id
- Deleting any ~/.grok/sessions entry other than the one UUID task-mgr passed
- Running grok in a project root or touching existing ~/.grok/sessions directories while testing
- Running cargo install or replacing the installed task-mgr binary
- Editing any project's .task-mgr/config.json
- Tests that only assert no crash, or that call a live CLI or the live Usage API
- Hand-editing tasks/*.json
- Any per-task model field or top-level PRD model field

---

## Global Acceptance Criteria

These apply to every implementation task. The task-level criteria in `## Current Task` are layered on top.

- Rust: cargo fmt --check passes
- Rust: cargo check has no warnings
- Rust: cargo clippy --all-targets -- -D warnings passes for the touched crate
- Scoped tests for the touched modules pass
- Red-first: new tests were seen failing before the production change
- No per-task model field and no top-level PRD model field

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Everything about this iteration's task is in `## Current Task`. PRD-wide rules are in this prompt.

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/model-routing-p1-grok-safety.json)
```

| Need | Command |
| --- | --- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` |
| List remaining tasks | `task-mgr list --prefix $PREFIX --status todo` |
| Recall | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up | `echo '{...}' \| task-mgr add --stdin --from-json tasks/model-routing-p1-grok-safety.json --depended-on-by REVIEW-001` |
| Patch a task (ANALYSIS-001 corrections) | `echo '{...}' \| task-mgr update --stdin --from-json tasks/model-routing-p1-grok-safety.json` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| --- | --- |
| `tasks/model-routing-p1-grok-safety-prompt.md` | This prompt (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — tail, then append |

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read on the first iteration. ANALYSIS-001 findings live under `## ANALYSIS-001`. FIX tasks read that section, not the whole log.

---

## Your Task (every iteration)

1. Work the task in `## Current Task`.
2. Pull only the progress context you need (ANALYSIS-001 findings for FIX tasks).
3. Recall with `task-mgr recall --for-task`. Do not Read `CLAUDE.md` in full. The excerpts below are what applies.
4. `git branch --show-current` is `feat/model-routing`.
5. Think before coding. For high-effort or behavior-modifying tasks, name one rejected alternative.
6. Write the tests, run them, see them fail, record the output.
7. Implement the one task.
8. Run the scoped gate, then commit.
9. Commit: `fix: <TASK-ID>-completed - [Title]` (or `test:` / `docs:` / `chore:` as appropriate).
10. Emit `<task-status><TASK-ID>:done</task-status>`.
11. Append one progress block terminated with `---`.

---

## Quality Checks

### Per-iteration scoped gate

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test -p task-mgr <module_or_fn_name>
```

Do not run the unscoped suite during a FIX. That is REVIEW-001. The machine may be heavily loaded by other projects' builds; a slow cargo is fine. Never kill processes you did not start.

### Final gate at REVIEW-001

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

Fix pre-existing failures too. Under about 12 unrelated failures, fix them here. Above that, spawn one FIX via `task-mgr add --stdin --from-json tasks/model-routing-p1-grok-safety.json --depended-on-by REVIEW-001` and emit `<promise>BLOCKED</promise>`.

---

## Project Verification Skills

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** none of the feature ids. The change is in the Grok runner and outcome mapping; the harness refuses `loop run`. Unit and mock-script runner tests are the proof.

---

## Review Tasks

| Review | Priority | Spawns | Focus |
| --- | --- | --- | --- |
| PRUNE-001 | 60 | PRUNE-FIX at 61-69 | Existence on the branch diff only. Do not edit production code. Confirm the old session-dir heuristic is gone |
| REVIEW-001 | 99 | FIX if the full suite needs it | Full suite. AC includes bare `PRUNE-FIX-xxx` |

Spawn shape:

```sh
echo '{
  "id": "PRUNE-FIX-001",
  "title": "Remove: <specific item>",
  "description": "From PRUNE-001: <details>",
  "acceptanceCriteria": ["Item removed", "Scoped tests pass"],
  "priority": 61,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --from-json tasks/model-routing-p1-grok-safety.json --depended-on-by REVIEW-001
```

---

## Progress Report Format

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence]
Red: [test names + one failing line, for FIX tasks]
Files: [paths]
Learnings: [1-3 one-line bullets]
---
```

---

## Key Learnings (from task-mgr recall)

- **[5364]** Live CLI copy uses "reached your … limit". Keep every widened pattern. Add, do not narrow.
- **[5298]** Do not collapse the pre-iteration usage check and the post-output probe onto one flag.
- **[5900]** The rate-limit lift probe scans text only after a non-zero exit. Do not route it differently.
- **[3754]** Session cleanup is an unconditional `LlmRunner::cleanup_session` called from `dispatch`, with one warn banner on error.
- **[4696]** `grok_runner_cleanup_session_is_idempotent` has an env/order race. Use a per-test temp HOME via the existing helpers; do not mutate process env concurrently.
- **[3204]** Document provider-specific workarounds in cleanup code (`WORKAROUND(<tag>)` comments).
- **[1617]** `cleanup_ghost_sessions` is for `~/.claude/sessions` stubs. Do not reuse it for Grok.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

- Product UX goes through `ui::*`; diagnostics through `tracing`. Grok child stderr is captured per slot under `.task-mgr/logs/` (`grok stderr → <path>`). Tracing events must not carry output or stderr text.
- Runner errors that are not task failures map to outcomes in `iteration.rs` and `slot.rs` (`GrokAuthFailure` → `Crash(GrokAuthFailure)`, `TransientBackend` → `TransientBackend`). Add the new arm the same way on both paths.
- Coordinators destructure param structs exhaustively (no `..`). `iteration.rs`, `slot.rs`, and `wave_scheduler.rs` deny deprecated leaves.
- Grok takes `--output-format streaming-json` without `--verbose`, the prompt via `--prompt-file`, and always `--no-subagents`.
- `~/.grok/sessions/<urlencoded cwd>/prompt_history.jsonl` accumulates across sessions by design and must never be removed.

---

## Data Flow Contracts

- Grok stderr → `stderr_buf` (`spawn_grok_stderr_sniffer`) → read once after `child.wait()` into `stderr_str` → auth sniff → **plan-limit sniff** (case-insensitive `rate limit for your plan` only; do not call `is_rate_limited`; do not edit `detection.rs`) → transient sniff → `Ok(RunnerResult)`. A `<completed>` tag on stdout skips that return on either exit arm. Auth still wins. `dispatch` cleans up only on `Ok`.
- New error → `iteration.rs` / `slot.rs` match on `Err(TaskMgrError::<Variant> { .. })` → `IterationResult { outcome: IterationOutcome::RateLimit, task_id: Some(..), effective_runner: Some(..), output: String::new(), .. }`, copying the `TransientBackend` arm's other fields.
- Session id: `Uuid::new_v4()` → argv `--session-id <uuid>` → `RunnerResult.session_id = Some(uuid)` → `dispatch` → `GrokRunner::cleanup_session(uuid, cwd)` → `remove_dir_all(grok_encoded_session_dir(cwd, home).join(uuid))`.

---

## Feature-Specific Checks

- Never run `grok` in a project root, and never touch an existing `~/.grok/sessions` directory. Interactive Grok sessions run in other project roots on this machine. Tests use a temp HOME and a mock grok script.
- Never `cargo install` or replace the installed `task-mgr` binary; other loops use it.
- Do not edit any `.task-mgr/config.json`.
- `feat/cli-error-gated-detection` is landed on `main` (`eded39d`) and merged into this branch. Build on its Grok stderr block in `runner.rs`. Do not edit `detection.rs` for the sniff. The plan-phrase clause already in `is_rate_limited` stays for Claude stdout. Keep hunks small and local.

---

## Important Rules

- Work on one story per iteration.
- Commit after each passing story. No push, no merge.
- Read before writing.
- Minimal changes.
- Mark status with `<task-status>`. Do not edit the JSON.

## Stop and Blocked Conditions

When every story has `passes: true` and the final review spawned nothing new:

```
<promise>COMPLETE</promise>
```

If blocked, append the reason to the progress log and:

```
<promise>BLOCKED</promise>
```
