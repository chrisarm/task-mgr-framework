# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Agent task-ops UX PR-3 — export scoped + docs/prompt alignment** for **task-mgr**.

## Problem Statement

`task-mgr export --to-json PATH` dumps **every** unarchived task in the DB and stamps **the first** `prd_metadata` row (`ORDER BY id LIMIT 1`) onto the file. Operator docs and crash-recovery recipes point that dump at `tasks/<prd>.json`. Export is lossy (no `taskPrefix`, extra keys stripped, status collapsed to `passes`), so the write is not a merge — it **smashes** the live task-list, including sibling PRDs’ tasks and any `humanReviewOutcome` PR-2 just made persist.

Agents are told to pin with `--from-json` and to jq `.userStories[]`, but `task_ops` still shows `.tasks[]` and has no `add --from-json` / `task-mgr update` one-liners. Intents “where will my add land” / “view active” talk about `from-json` without showing the real flag. README still teaches the smash recipe. `scripts/claude-loop.sh` is the live smash caller (`export --to-json "$PRD_FILE" || true`).

Goal: export cannot smash a PRD; every agent-facing surface tells the same story.

**This list ships PR-3 only.** PR-1 is **this tree** (`context.rs`, `prd_json.rs`, add/current `--from-json`, `unique_tmp_path`, `paths_identify`). PR-2 (`task-mgr update --stdin`, enhance CLARIFY) is assumed **at merge** — name `update --stdin` in `task_ops` / cheatsheet; **do not implement `update.rs`**. If PR-2 already rewrote CLARIFY, only remaining prompt/cheatsheet/`task_ops`/best-practices residual.

---

## PR-3 scope lock (read every iteration)

In scope: scoped export dump (active prefix / `--from-json` source pin / `--all` = today’s dump); overwrite-guard (`--force` dump, pin-19 dest identity, `LockGuard` **inside** `export()` after `dest.is_file()`); clap `--from-json` / `--all` / `--force`; identity helper returns `prd_id`; `task_ops` jq / add pin / update one-liner; enhance spawn-fixup (a) stays + cheat-sheet add `--from-json`; intents where/land + view active; cheatsheet update + export recipes; README / INTEGRATION / QUICKSTART / ARCHITECTURE / CHANGELOG; `scripts/claude-loop.sh` smash callers; worktree dest rust tests; verify-task-mgr feature **create then drive**.

**Out of scope (do not implement, do not spawn as "helpful" follow-ups):**

- Remapper / `add --from-json` / `current --from-json` clap (pins 1–4, 14–15, 20) — consume PR-1; do not re-ship
- `task-mgr update` overlay / `humanReviewOutcome` field / error_recovery CLARIFY writer (pins 5–7, 10, 12, 17) — PR-2; this PR only **names** `update --stdin`
- Claim-scoped short `<task-status>` ids; a `set-status` command; MCP wrappers
- `add --from-json` creating/registering a new PRD
- Changing DB anchoring (main checkout `.task-mgr` from a worktree stays)
- Putting `priority` on the update whitelist
- Copying or editing `~/.claude/docs/task-mgr-best-practices.md` (not in this repo; CHANGELOG residual only)
- Adding `taskPrefix` / extra keys to `ExportedPrd` so dump is “safe” without `--force`
- Making `--force` a merge / Value-preserve of dest
- Calling `export()` from the loop engine
- Adding `--force` onto `scripts/claude-loop.sh` `$PRD_FILE`
- Reusing `add::preflight_from_json_path` from export
- Prefix-scoping `--with-progress` / `--learnings-file`
- Using `refuse_unpinned_write` for export default
- Sharing CLI overwrite-guard / `exists()` into loop startup
- `WHERE task_prefix IS NULL` for empty-prefix metadata
- Rewriting historical `tasks/*-prompt.md`

Pins (do not rewrite; **1–21 verbatim from the ledger**. Pins 8–9 / 18–19 and the docs/prompt slice of 1, 11, 13, 21 are law for this PR. Pins 5–7 / 10 / 12 / 17 must not be implemented. Pins 1–4 / 14–15 / 20 are consumed from PR-1, not re-shipped):

1. `--from-json` on add/update/current/export ships as “pin this already-registered effort”. `--depended-on-by` cannot pin a worktree-only file.
2. Unregistered `--from-json` path: Refuse (`loop init` first). Identity must treat relative `prd_files` + worktree remap as registered.
3. Ambiguous prefix (≥2 non-NULL, no env, no flag): Refuse the write. Zero prefixes / `--no-prefix` still allow DB insert — loop does *not* always set `TASK_MGR_ACTIVE_PREFIX` (`PrefixMode::Disabled`).
4. Worktree JSON: Pure remap, then CLI existence check. Loop remap stays unconditional. `--from-json PATH` always writes that PATH (never remapped away).
5. `task-mgr update` is a real command with load-merge-write. Must not reuse `init::import::update_task` (full-row SET + clears `archived_at`).
6. Status via `update`: Hard-error `status` / `passes` (including a full story blob). Lifecycle SSoT; do not silently skip.
7. Unknown overlay keys: Hard-error. Silent drop is the original `humanReviewOutcome` bug.
8. Export default: Active-PRD only; `--all` restores today’s dump.
9. Overwrite a registered task-list: Always refuse without `--force`, even when scoped to that PRD. Export is a lossy dump, not a merge.
10. `tasks.status` is lifecycle-only. `update` / JSON patch never write it. `passes` in an overlay is a hard error, not an ignore.
11. JSON sync is best-effort; DB commits first. Failure copy names `task-mgr current` and retry `--from-json`, never `export`.
12. One JSON write chokepoint: unique tmp + rename (reuse `prd_reconcile::unique_tmp_path` scheme: pid + counter + nanos). Preserve unknown keys on patch. Do not deserialize an existing story to `PrdUserStory` and write it back.
13. `--from-json` never registers a PRD and never remaps the write target. It only pins an already-registered effort.
14. Live remap is path math, not discovery. No `exists()`, no basename search. Relative `prd_files` rows are joined to `source_root` before remap.
15. Loop remap stays unconditional. CLI existence checks are caller-side and must not be shared into startup.
16. Refuse-without-pin applies iff ≥2 registered non-NULL prefixes. Zero-prefix / `--no-prefix` is a different mode: DB insert OK; JSON sync only if exactly one `task_list` is registered.
17. `humanReviewOutcome` is not a DB column. Persistence is the task-list JSON; `PrdUserStory` must not strip it on import.
18. Export to a registered `task_list` is opt-in `--force`. `--force` is a dump, not a merge. Take the same `LockGuard` as add if the destination is a live PRD.
19. Path identity (canonicalize + `source_root.join` + worktree remap) is one function, used by add / update / current / export overwrite-guard.
20. Do not ship the multi-prefix refuse before clap has `--from-json` (docs already tell agents to pass it). PR-1 ships both together.
21. Out of scope: claim-scoped short `<task-status>` ids; a generic `set-status` command; `add --from-json` creating/registering a new PRD; rewriting historical `tasks/*-prompt.md`; changing DB anchoring (main checkout `.task-mgr` from a worktree stays); MCP task wrappers; putting `priority` on the update whitelist.

Architect fold (encode on the matching FEAT, not a new phase): `LockGuard` **inside** `export()` only, after `dest.is_file()` — never in `main.rs` Export arm. Missing/directory/unregistered `--from-json` go through `resolve_context(..., "export")` — not `preflight_from_json_path`. Empty-prefix metadata is `prd_metadata.id = identity-matched prd_files.prd_id` — **forbid** `WHERE task_prefix IS NULL`. No-active error names `--from-json` / `--all` / `task-mgr current`. `claude-loop.sh` three `$PRD_FILE` dumps removed or retargeted — **never** `--force` onto `$PRD_FILE`. After `enhance agents`, fenced block still has spawn-fixup (a) **and** (if PR-2 landed) `update --stdin` CLARIFY.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). If a **Project Verification Skills** section applies to this task, follow that skill after the language gate. Don't add a separate self-critique step; the linters, type-checker, targeted tests, and (when present) the project verification skill catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Re-shipping PR-1 remapper / add `--from-json` clap / `current --from-json`, or implementing PR-2 `update.rs` / overlay whitelist / `humanReviewOutcome` field
- `LockGuard` in `main.rs` Export arm AND inside `export()` (non-reentrant flock deadlock)
- Calling `add::preflight_from_json_path` from export (errors say `"add"`; coupling forbids `export` → `add`)
- `WHERE task_prefix IS NULL` for empty-prefix metadata (two `--no-prefix` inits stamp the wrong row)
- Empty prefix LIKE `"-%"`
- Using `refuse_unpinned_write` for export default (wrong copy; `current` stays a probe)
- Dest write via `cli_write_path` / remapping `--to-json PATH`
- Overwrite-guard using match (a) instead of pin-19 `find_registered_by_path_identity`
- Skipping `--force` when dest is the same PRD (lossy smash of extra keys / `humanReviewOutcome`)
- `--force` as a Value-merge / `PrdUserStory` round-trip of dest
- Adding `taskPrefix` or extra keys to `ExportedPrd` so dump is “safe” without `--force`
- Making `--all` a JSON array of PRDs or dropping LIMIT 1
- Default dest = `ctx.prd_json_path` when `--to-json` omitted
- JSON-sync failure copy naming `task-mgr export` (pin 11)
- jq `.tasks[]` left in `task_ops`
- Adding `--force` onto `scripts/claude-loop.sh` `$PRD_FILE`
- Calling `export()` from the Rust loop engine
- Copying or editing `~/.claude/docs/task-mgr-best-practices.md` into this repo
- Rewriting historical `tasks/*-prompt.md`
- Dropping enhance spawn-fixup form (a); restoring hand-edit + `loop init` CLARIFY if PR-2 already rewrote it
- Re-adding `add --from-json` to the cheatsheet forbidden list
- Two `--no-prefix` inits as ≥2-prefix sandbox proof
- Driving verify-task-mgr on a task before the feature file exists; claiming worktree live-path via that harness; driving `loop run` / `batch run`
- Inventing a second verification harness; treating compile/unit tests as proof for scoped export / `--force` / `--all` / default error
- Sharing CLI overwrite-guard / `exists()` into loop startup (pin 15)
- Changing DB anchoring; MCP wrappers; a `set-status` command; claim-scoped short `<task-status>` ids; putting `priority` on the update whitelist
- Manual edits to `tasks/*.json` for status (use task-mgr CLI / task-status tags)
- unwrap() on filesystem or SQLite in export unless a prior invariant makes it unreachable
- tracing for operator-facing refuse/`--force` copy (use `ui::emit` / `ui::emit_err`)
- Tests that only assert 'no crash' or check type without verifying content

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy -- -D warnings` (per-iteration). `--all-targets` is REVIEW-001 only
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() on filesystem or SQLite in new production paths (`export/`)
- Operator refuse/`--force` copy uses `ui::emit` / `ui::emit_err` (CONTRACT-LOG-001), never tracing
- Do not re-ship PR-1 remapper / add clap / `current --from-json`, and do not implement PR-2 `update.rs` / overlay whitelist
- Later FEATs grep symbols (`load_prd_metadata`, `load_tasks`, `write_json_atomic`, `find_registered_by_path_identity`, `unique_tmp_path`, `resolve_context`) — do not freeze `file:NNN` from the PRD-input table (HEAD is `120c5a2`; table may describe older trees)
- Clap parse tests live in `src/cli/tests.rs` and are run with `cargo test -p task-mgr cli::` (not `cargo test --test cli_tests`)
- User-facing export/docs proof is FEAT-012 + REVIEW-001 via `.claude/skills/verify-task-mgr/SKILL.md` feature `export-scoped-and-force` (compile/unit tests alone are not proof for those stories). FEAT-001–011 keep rust tests / docs greps only — no skill drive except FEAT-011 creating the recipe
- Existing `tests/worktree_db_resolution.rs` DB-anchoring tests stay green

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Project Verification Skills, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/agent-task-ops-pr3.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need | Command |
| --- | --- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` using the task ID from `## Current Task` |
| List remaining tasks (debug only) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings relevant to a task | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`) |
| Add a follow-up task (review spawns) | `echo '{...}' \| task-mgr add --stdin --from-json tasks/agent-task-ops-pr3.json --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| --- | --- |
| `tasks/agent-task-ops-pr3-prompt.md` | This prompt file (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log.

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist).

---

## Your Task (every iteration)

1. **Work the task in `## Current Task`**. If it says there is no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.
2. **Pull only the progress context you need** (`tac | awk | tac`, or grep a `dependsOn` block). Skip on the first iteration.
3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do not Read `tasks/long-term-learnings.md` / `tasks/learnings.md`. Never Read `CLAUDE.md` in full (excerpts are below). When a verification skill applies, Read that SKILL.md at verification time.
4. **Verify branch** — `git branch --show-current` matches `feat/agent-task-ops-pr3`.
5. **Think before coding** — assumptions, edge cases, Data Flow Contracts. Pick and go unless `estimatedEffort: "high"` or `modifiesBehavior: true` (then one rejected alternative).
6. **Implement** — single task, code and tests in one coherent change.
7. **Run the floor gate** (Quality Checks). If a **Project Verification Skills** entry covers this task (FEAT-012 / REVIEW-001), Read SKILL.md and follow it after the language gate. A green compile/test run is not proof for those stories. If the skill is blocked, emit `<promise>BLOCKED</promise>`.
8. **Commit**: `feat: <TASK-ID>-completed - [Title]`.
9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`. Do NOT edit the JSON.
10. **Append progress** — ONE post-implementation block, terminated with `---`.

---

## Task Selection (reference)

The loop engine owns selection and claim. Work only the pinned `## Current Task`.

To request a different pick on the **next** iteration, emit `<reorder>TASK-ID</reorder>`. Never combine reorder with `next --claim`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

PRD §6 already has the consumer impact table (embedded on FEAT-001/002/003 as `consumerAnalysis`). There is no `ANALYSIS-xxx` task. `BREAKS` consumers are the in-tree `export()` callers and CLI `--no-prefix` tests — update them in FEAT-002 / FEAT-003 as specified. Do not split those FEATs unless a consumer is a different semantic context (init `--from-json` import vs export `--from-json` pin — keep those distinct; do not change init).

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **REVIEW-001** runs the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (implementation / test / fix tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
# scope tests to touched crate/module (grep touchesFiles)
cargo test -p task-mgr cli::                         # clap (src/cli/tests.rs) — NOT cargo test --test cli_tests
cargo test -p task-mgr <module_or_fn_name>
```

Do **NOT** run the entire workspace test suite during regular iterations — that's REVIEW-001's job.

**Project verification skill:** FEAT-012 and REVIEW-001 only (plus FIX spawned from those). Compile/unit tests alone are not proof for scoped export / `--force` / `--all` / default error.

### Final gate at REVIEW-001 (the milestone)

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

If ANY test fails — including pre-existing failures — the milestone fixes them (or spawns `FIX-xxx` via `add --stdin --from-json tasks/agent-task-ops-pr3.json --depended-on-by REVIEW-001` when >~12 unrelated failures). Also drive the mapped verify-task-mgr feature.

---

## Project Verification Skills

This repo ships a project-level verification skill. Language-level gates are **necessary but not sufficient** for user-facing changes the skill covers. Follow the skill literally — do not invent a second harness, and do not paste the skill body into the progress log.

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** `export-scoped-and-force` (FEAT-011 creates the recipe; FEAT-012 + REVIEW-001 drive it)

**Per-iteration:** if this task is FEAT-012 or a FIX / WIRE-FIX spawned from FEAT-012 / REVIEW-001, Read that SKILL.md and the matching `features/export-scoped-and-force.md` and drive that recipe after the scoped language gate. Capture evidence where the skill says.

**REVIEW-001:** drive `export-scoped-and-force`. A skipped sub-feature is reported skipped, not verified via a sibling path.

**Blocked skill:** if you cannot launch or a precondition fails, emit `<promise>BLOCKED</promise>` with the unmet precondition. Do not skip the drive and mark the task done.

Worktree dest-identity is **not** this harness (FEAT-010 rust tests). Helper unsets `TASK_MGR_ACTIVE_PREFIX`. ≥2-prefix proof is two prefixed `loop init`s, never `--no-prefix` twice. Do not drive `loop run` / `batch run`.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

- FEAT-001 changes `load_*` signatures but leaves `export()` 4-arg dump-all (`load_tasks(&conn, None)` + unscoped LIMIT 1). Growing arity or introducing `ExportOpts` in that task fails `cargo check` at FEAT-001
- FEAT-002 introduces `ExportOpts` while clap is still 3-field: `main.rs` must forward `{ from_json: None, all: false, force: false, … }`. No `LockGuard` on the Export arm. Do not grow clap (FEAT-003). Do not restore a smash default to keep CLI `--no-prefix` tests green — those wait for FEAT-003 `--all`
- Clap `Commands::Export` grew fields but `src/cli/tests.rs` still matches three fields → `cli::` does not compile (FEAT-003)
- Library `export()` still 4-arg / silent smash default after CLI scoped (post-FEAT-003)
- `LockGuard` copied onto `main.rs` Export arm (deadlock with lock inside `export()`)
- `--from-json` errors say `"add"` because `preflight_from_json_path` was reused
- `--to-json` dest run through `cli_write_path`
- Overwrite-guard uses JSON `taskPrefix` (match a) instead of pin-19
- Empty-prefix metadata `WHERE task_prefix IS NULL`
- `task_ops` still `.tasks[]` or over 2048 bytes
- `enhance agents` restored hand-edit CLARIFY
- `claude-loop.sh` still smashes `$PRD_FILE` (or added `--force` onto it)

---

## Contract Tasks

`CONTRACT-xxx` tasks are **design-only**. Do not write production code or full test suites. Record the **full contract text** in the progress log under `## CONTRACT-001` / `## CONTRACT-002`. Downstream FEATs implement against that text.

---

## Review Tasks

| Review | Priority | Spawns | Before | Focus |
| --- | --- | --- | --- | --- |
| CODE-REVIEW-1 | 13 | `CODE-FIX` / `WIRE-FIX` (14-16) | CONTRACT + FEAT-001–011 | Lock site, coupling, pin-19 vs match (a), LIKE/ESCAPE, clap, docs smash, feature file exists |
| REFACTOR-REVIEW-FINAL | 70 | `REFACTOR-xxx` (71-85) | all implementation | DRY, coupling budget, contract fidelity |

Spawn with `--from-json` (pin 20) **and** `--depended-on-by REVIEW-001`:

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-1: <details>",
  "rootCause": "<file + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 14,
  "estimatedEffort": "high",
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --from-json tasks/agent-task-ops-pr3.json --depended-on-by REVIEW-001
```

When a **Project Verification Skills** entry covers the issue, set `verifyCommand` to that skill's drive, not a unit-test invocation. Commit `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit status. If no issues, emit status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it **tight**.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

CONTRACT tasks: also include the full contract text under `## CONTRACT-00N` in the same (or immediately following) block so dependents can grep it.

---

## Learnings Guidelines

Do not Read `tasks/long-term-learnings.md` / `tasks/learnings.md`. Use `task-mgr recall --for-task` / `--query`. Record new learnings with `task-mgr learn`.

**Write concise learnings** (1-2 lines each):
- GOOD: "`export()` locks inside the command after `dest.is_file()`; `main.rs` only forwards `ExportOpts`"
- BAD: "We discussed putting the lock in main.rs like other write commands but add actually locks inside add.rs so we should match that because flock is non-reentrant..."

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`: all stories `passes: true`; no new tasks from final review; REVIEW-001 passed.

### Blocked Condition

Document in progress; spawn `CLARIFY-001` via `add --stdin --from-json tasks/agent-task-ops-pr3.json --depended-on-by REVIEW-001`; output `<promise>BLOCKED</promise>`.

---

## Milestones

Milestones (here: `REVIEW-001`) are **full-gate checkpoints**.

1. All `dependsOn` have `passes: true`.
2. Full unscoped suite + drive `export-scoped-and-force`.
3. Leave the repo green. Spawn FIX via `add --stdin --from-json tasks/agent-task-ops-pr3.json --depended-on-by REVIEW-001` when needed.
4. Mark `<task-status>REVIEW-001:done</task-status>` only when the full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md`.

- **[1252]** / **[4114]** / **[3332]** / **[3756]** PRD JSON key is `userStories`, not `tasks`. `task_ops` jq must be `.userStories[]`.
- **[2236]** Omitted `--from-json` leaks fixups — pin + refuse; docs must show the flag.
- **[5596]** `--from-json` is a pin, not an import (export source pin too).
- **[5615]** / **[1561]** JSON-sync miss copy: `task-mgr current` + retry `--from-json`, **never** `export`.
- **[5577]** / **[2667]** / **[4564]** / **[1562]** Unique tmp `.{base}.{pid}-{n}-{nanos}.tmp`, same-dir rename — export dump must leave `.json.tmp`.
- **[2588]** `export/prd.rs` graceful fallbacks can hide a wrong prefix filter — do not `unwrap_or` all rows.
- **[5654]** / **[5689]** Pin-19 identity is `git::paths_identify` (b)+(c) only; callers consume; match (a) is a separate prefix OR.
- **[5605]** Dual-PRD fixtures need unique story ids across PRDs.
- **[5642]** Grep treating `--from-json` as a flag can false-positive.
- **[5641]** verify-task-mgr pin-init JSON spacing can false-positive; assert parsed fields.
- **[5443]** Isolating `HOME` for verify-task-mgr breaks rustup unless `RUSTUP_HOME` / `CARGO_HOME` are kept; use the in-tree helper.
- **[2903]** `TASK_MGR_ACTIVE_PREFIX` leaks into subprocess tests — helper unsets it; do not re-export it in the sandbox.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

- **CONTRACT-LOG-001:** `ui::*` for product UX / CLI data / byte-locked operator contracts; `tracing` for internal diagnostics only. Refuse/`--force` copy is `ui::emit` / `ui::emit_err`.
- **Never edit `tasks/*.json` directly.** Use CLI + `<task-status>`. JSON-sync recovery names `current` + retry `--from-json`, never `export`.
- **Spawn-fixup PRD targeting:** `task-mgr add --stdin` MUST pass `--from-json tasks/agent-task-ops-pr3.json` (or `--depended-on-by REVIEW-001` / `CONTRACT-00x`) or the entry leaks. Form (a) **stays** after `enhance agents`.
- **Human-in-the-loop CLARIFY:** PR-2 owns `update --stdin` then `complete`. This PR does **not** redo that block; after `enhance agents` it must still contain `update --stdin` if PR-2 already rewrote it (must not restore hand-edit + `loop init`).
- **Sticky path identity:** first Auto registration hashes and freezes; path identity is canonicalize + `source_root.join` + worktree remap. Overwrite-guard reuses that function (not JSON `taskPrefix`).
- DB from a worktree cwd already lands in **main** `.task-mgr`. Do not change that. `--to-json PATH` writes **that** PATH.
- Tests that spawn the real binary with an init-family command MUST set `HOME` to a tempdir. verify-task-mgr already does this.

---

## Data Flow Contracts

These are **verified access patterns**. Use these exactly.

**Active prefix** — `resolve_context` → `ResolvedContext.prefix: String`:

```rust
let ctx = resolve_context(conn, from_json, "export")?;
// None + not --all → invalid_state("export", ...) naming --from-json / --all / task-mgr current
// Empty prefix → no LIKE filter
```

**Task filter** — prefix `String` → LIKE `"{escaped}-%"`:

```rust
let (clause, pat) = db::prefix::prefix_and(Some(&prefix));
// WHERE archived_at IS NULL {clause}  bind pat
// --all / empty prefix: omit clause. Do not LIKE "-%"
```

**Metadata scoped (named)** — `prd_metadata.task_prefix` TEXT UNIQUE:

```sql
SELECT … FROM prd_metadata WHERE task_prefix = ?  -- &ctx.prefix when non-empty
```

**Metadata empty-prefix pin** — `prd_files.prd_id` INTEGER → `prd_metadata.id`:

```rust
// identity match of --from-json PATH returns prd_id
// SELECT … FROM prd_metadata WHERE id = ?
// Grep export/: no WHERE task_prefix IS NULL
```

**Metadata `--all`** — keep `ORDER BY id ASC LIMIT 1`.

**`--from-json` source pin** — clap `Option<PathBuf>` → `resolve_context(conn, path, "export")`. Missing / directory / unregistered **all** go through this call. Registered via (a) **or** pin-19 (b)/(c). `prd_json_path` on ctx is **ignored for dest**. Errors command-name `"export"`.

**`--to-json` dest** — clap `PathBuf` (required). Write **that** PATH. Never `cli_write_path`, never remap.

**Overwrite-guard** — dest `Path` → identity `Option<(prd_id, prefix)>`:

```rust
// Inside export(), after dest.is_file() + LockGuard:
find_registered_by_path_identity(...) -> Some => require --force
// Match (a) is NOT this function
```

**Live-path pair** — `paths_identify(dest_canon, registered, source_root, worktree_root)` after joining relative `prd_files.file_path` to `source_root`.

**Dump body** — `ExportedPrd { user_stories: Vec<ExportedUserStory> }`:

```rust
serde_json::to_string_pretty(&prd)  // no dest read, no PrdUserStory, no taskPrefix field
```

**Tmp name** — `.{basename}.{pid}-{n}-{nanos}.tmp` next to dest: `prd_json::unique_tmp_path(dest)` then rename.

**`LockGuard`** — **Inside** `export()` only:

```rust
if dest.is_file() {
    let _lock = LockGuard::acquire(db_dir)?;
}
// then identity then write. main.rs does not acquire. Missing dest → no lock
```

**`passes`** — `tasks.status` TEXT → JSON bool **unchanged**: `status == Done`. Dump is not a lifecycle write.

**HEAD extraction targets (grep symbols, do not freeze NNN):** `export::export` still `(dir, to_json, with_progress, learnings_file)` dump-all; `write_json_atomic` uses `with_extension("json.tmp")`; `load_prd_metadata` LIMIT 1; `load_tasks` all unarchived; `Commands::Export` three fields; `main.rs` Export arm no lock; `find_registered_by_path_identity` private prefix-only (`SELECT pf.file_path, pm.task_prefix`); `unique_tmp_path` already in `prd_json.rs`; `task_ops` jq `.tasks[]`; cheatsheet already has `add --from-json`; enhance spawn-fixup (a) present; CLARIFY on HEAD may still be hand-edit (PR-2); `scripts/claude-loop.sh` three `$PRD_FILE` dumps. No `update.rs` on this HEAD.

---

## Feature-Specific Checks

- Grep `export/`: no `cli_write_path`; no `WHERE task_prefix IS NULL`; no `invalid_state` command-name `"add"`; no `preflight_from_json_path`; `write_json_atomic` uses `unique_tmp_path` not `.json.tmp`.
- Grep `main.rs` Export arm: no `LockGuard::acquire`.
- Grep `src/loop_engine`: no `export::export` call.
- Grep `scripts/claude-loop.sh`: no `export --to-json "$PRD_FILE"`.
- `task_ops_section().len() < 2048`; contains `.userStories[]`, `--from-json`, `task-mgr update`; does **not** contain `.tasks[]`; does **not** name `export` as JSON-sync recovery.
- After `task-mgr enhance agents`: fenced block still has spawn-fixup (a) `--from-json tasks/<correct-prd>.json`. If PR-2 already put `update --stdin` in the template, it survived (no hand-edit + `loop init` CLARIFY path).
- `export --help` says **pin**, not import.
- `--all` conflicts `--from-json` (`cargo test -p task-mgr cli::`).
- Library `PrefixMode::Disabled` callers pass `All`; CLI `--no-prefix` export tests pass `--all`; dest stays a **new** file.
- ≥2-prefix default error: two prefixed `loop init`s, never `--no-prefix` twice.
- `ExportedPrd` has no `taskPrefix` field.
- No in-tree copy of `~/.claude/docs/task-mgr-best-practices.md`.
- Do not rewrite historical `tasks/*-prompt.md`.
- Clap tests: `cargo test -p task-mgr cli::` (not `cargo test --test cli_tests` for `src/cli/tests.rs`).
- Later tasks grep symbols, not frozen `export/prd.rs:NNN`.
- FEAT-001: `export()` stays 4-arg dump-all; call `load_tasks(&conn, None)` + unscoped `load_prd_metadata` (LIMIT 1); no `ExportOpts`, no clap, no LockGuard, no `write_json_atomic` change. `touchesFiles` includes `src/commands/export/mod.rs` (call-site only).
- FEAT-002: `main.rs` in `touchesFiles`. Forward `ExportOpts { from_json: None, all: false, force: false, … }` from still-3-field clap. No LockGuard on the Export arm. Do not grow clap. CLI `--no-prefix` export tests (`human_review_cli` / `model_fields_cli` / `test_export_roundtrip`) wait for FEAT-003 `--all`; do not restore smash default.
- FEAT-007: if `Commands::Update` is absent, name `task-mgr update --stdin --from-json` in curated prose; only emit clap-parsed flag tokens that exist on this binary. Export `--force` / `--all` / `--from-json` tokens are in-scope.
- FEAT-010: `touchesFiles` is `tests/worktree_export_dest.rs` only. Follow `tests/worktree_db_resolution.rs` as a read-only pattern; do not edit it.
- FEAT-011 creates the feature file; FEAT-012 drives SKILL.md (do not edit SKILL.md; keep the feature file); REVIEW-001 drives it again.

---

## Key Context (HEAD `120c5a2`)

- PR-1 is merged on this checkout. Reuse `resolve_context` / `paths_identify` / `unique_tmp_path` / `choose_cli_write_path`. **Do not** use `cli_write_path` for export dest.
- `find_registered_by_path_identity` is **private** and returns `Option<Option<String>>` (prefix only). This PR promotes it to return `(prd_id, prefix)`. `match_registered_from_json` stays prefix-only at the resolve_context layer (match (a) first, then identity).
- **Arity window:** FEAT-001 keeps `export()` 4-arg dump-all and patches `export/mod.rs` call sites only. FEAT-002 introduces `ExportOpts`; `main.rs` still matches 3-field clap and forwards `from_json: None, all: false, force: false`. FEAT-003 grows clap. Do not collapse those windows.
- `add()` acquires `LockGuard` **inside** `add.rs`, not `main.rs`. Export matches that site (starting FEAT-002).
- Cheatsheet **already** contains `add --from-json` and no longer forbids it. This PR **adds** update + export recipes. If `Commands::Update` is absent, name `update --stdin --from-json` in prose; only clap-parsed tokens that exist on this binary.
- Enhance spawn-fixup form (a) is **already** present. CLI cheat sheet add example still lacks `--from-json`.
- `task-mgr update` may still be absent on HEAD. Name it; do not stub a writer.
- Worktree dest tests live in **new** `tests/worktree_export_dest.rs`. `tests/worktree_db_resolution.rs` is the pattern, not a write target.
- verify-task-mgr sandboxes are **not** linked git worktrees. Helper unsets `TASK_MGR_ACTIVE_PREFIX`. FEAT-012 drives SKILL.md; does not edit it.

---

## Important Rules

- Work on **ONE story per iteration**
- **For high-effort tasks** (`estimatedEffort: "high"` or 10+ acceptance criteria): consider using `/ralph-loop` to iterate within the task until all acceptance criteria pass
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** — prefer this prompt's excerpts over re-reading `CLAUDE.md`
