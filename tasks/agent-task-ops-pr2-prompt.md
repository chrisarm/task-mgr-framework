# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Agent task-ops UX PR-2 — `task-mgr update` + `humanReviewOutcome`** for **task-mgr**.

## Problem Statement

Agents cannot change task fields without hand-editing `tasks/*.json` or going through `loop init --append --update-existing`. The latter is a full-row SET that **clears `archived_at`**, DELETE+reinserts files/relationships, and (when `passes: true`) raw-SETs `status` — so a notes-only “update” would clobber priority, title, archive state, and possibly status. Clap has no `update` command; `error_recovery` still says “edit the JSON then loop init”. `humanReviewOutcome` is documented as the CLARIFY persistence path but `PrdUserStory` has no field, so serde **silently drops** it on import and on any typed round-trip (the original bug). `AddTaskInput` would also drop it on a spawned CLARIFY row.

Goal: agents can patch whitelist fields (including `humanReviewOutcome`) without editing JSON and without touching `tasks.status`.

**This list ships PR-2 only.** PR-1 **is this tree** (`120c5a2` / squash `3fc38b4` / GitHub #43): remapper, `commands/context.rs`, `commands/prd_json.rs`, and add/current `--from-json` already exist here. Do **not** look for a looping PR-1 worktree. PR-3 is export scoping + remaining docs. Do not implement those.

---

## PR-2 scope lock (read every iteration)

In scope: `task-mgr update` load-merge-write; overlay whitelist + reject (`status`/`passes`/unknown keys/type-null); `prd_json::patch_user_story`; `humanReviewOutcome` on `PrdUserStory` + `AddTaskInput` (JSON-only, no DB column); clap `Update` + same `--from-json` pin as add; error_recovery + enhance CLARIFY + intents (`update --stdin` then `complete`); worktree live-path rust tests; verify-task-mgr feature + drive.

**Out of scope (do not implement, do not spawn as "helpful" follow-ups):**

- Export default / `--all` / `--force` overwrite (pins 8–9, 18)
- Remapper / `add --from-json` / `current --from-json` clap (pins 1–4, 14–15, 20) — consume PR-1; do not re-ship
- Cheatsheet / `task_ops` jq `.userStories[]` / remaining prompt one-liners / `~/.claude/docs/task-mgr-best-practices.md` (PR-3)
- Claim-scoped short `<task-status>` ids; a `set-status` command; MCP wrappers
- `add --from-json` creating/registering a new PRD
- Changing DB anchoring (main checkout `.task-mgr` from a worktree stays)
- Putting `priority` on the update whitelist
- Changing `init::import::update_task` SQL (re-import revive still clears `archived_at`)
- Schema-validating inner `humanReviewOutcome` keys
- Positional `task-mgr update <id> --title/--notes` flag-per-field CLI
- Rewriting historical `tasks/*-prompt.md`

Pins (do not rewrite; **1–21 verbatim from the ledger** — same 21 as PR-1. Pins 5–7 / 10–13 / 16–17 / 19 / 21 are law for this PR. Pins 8–9 / 18 must not be implemented. Pins 1–4 / 14–15 / 20 are consumed from PR-1, not re-shipped):

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

Architect fold (encode on the matching FEAT, not a new phase): JSON-only overlay cannot `Ok` skip; merge skips overlay `id` and writes `dependsOn` unprefixed; type/null table fail-closed bound to `PrdUserStory` field types — split `claimsSharedInfra` (bool|null), `humanReviewTimeout` (u32|null), `reviewScope` (JSON value|null) from string scalars; `dependsOn` DELETE is `rel_type = 'dependsOn'` only; pin order matches add (directory `is_file()` **before** parse via extracted `preflight_from_json_path(path, command)`); write-path is `default_prd_roots` → `resolve_context_with_roots` (not bare `resolve_context`); `ctx is None` → `sole_task_list_path` then `choose_cli_write_path` when roots known; leftover `"add"` includes **preflight** as well as `atomic_write`; mixed overlay + empty write path is pin 11; regenerate managed `CLAUDE.md` via `task-mgr enhance agents`; flip `context.rs` add-only refuse comments **and** `sole_task_list_path` rustdoc to **write-only**; US-007 follows `live_worktree_file_exists_writes_worktree_main_unchanged` (keep `add_from_worktree_root_lands_in_main_db` green — DB anchoring only); US-008 clones `.claude/skills/verify-task-mgr/features/add-and-current-from-json.md` (file **is on HEAD**).

---

## Write-path / preflight law (PR-1 is this tree — read every iteration)

Grep **symbols**. Do not freeze `file:NNN`.

- **`add(..., from_json)` exists.** `preflight_from_json_path` is **private in `add.rs` and hardcodes `"add"`** — extract `preflight_from_json_path(path, command: &str)` **next to** `refuse_unpinned_write` in `context.rs`. Add and update both call it. Update must **not** import `add`. Directory/missing errors on the update path must say `"update"`.
- **Write-path callers** (`add` today, `update` in this PR): `default_prd_roots(db_dir)` → `resolve_context_with_roots(conn, from_json, "update", Some(&source_root), Some(&worktree_root))`. Do **not** document or call bare `resolve_context` as the write-path (TempDir / `--dir` relative `prd_files` remap onto the developer checkout).
- **`ctx is None`:** `sole_task_list_path` then **`choose_cli_write_path` when roots are known**, else `cli_write_path`. `cli_write_path` is the live-git probe; `choose_cli_write_path` is the testable one.
- **`default_prd_roots` is still private in `add.rs`** — ACCEPTED residual: copy the same `InitOpts::resolve_roots` fallback into `update.rs` **or** extract next to context helpers; do **not** `use commands::add`.
- **`atomic_write` leftover `"add"`** must be parameterized. **`strip_prefix_in_id_array` is private** — promote to `pub(crate)` if needed; do not duplicate.
- **JSON-only overlay** + missing path / patch `Err` → `invalid_state`. **Mixed overlay** + empty path → pin 11 skip/warn (DB committed; never `export`; never `invalid_state` rollback).
- **Pin order matches add:** missing/directory **before** overlay parse; unregistered / ≥2 refuse **after** parse, before the write txn. Do not open a second connection only to beat parse.
- **Empty `ctx.prefix`:** skip `apply_prefix` **and** `prefix_id`.
- **`--from-json PATH` always writes that PATH** (never remapped away). `--from-json` never registers.
- **`refuse_unpinned_write`** stays **outside** `resolve_context` (current stays a probe). Flip rustdoc from add-only to **write-only**.
- **Do not implement PR-3.**

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

- Implementing PR-3 (export default/`--all`/`--force` overwrite, cheatsheet/`task_ops` jq `.userStories[]`, remaining prompt one-liners, `~/.claude/docs/task-mgr-best-practices.md`) or re-implementing PR-1 remapper / add `--from-json` clap / `current --from-json`
- Calling `init::import::update_task` from `task-mgr update` (full-row SET + `archived_at = NULL`)
- Calling `delete_task_relationships` from update (wipes every `rel_type`; old synergy/batch/conflicts rows must survive)
- SET `tasks.status`, `archived_at`, `priority`, or `id` from update / JSON patch
- Silent skip / drop of overlay `status` / `passes` / unknown keys (including `priority`, `synergyWith`)
- Deserializing the overlay or an existing story to `PrdUserStory` for merge/write-back (strips extra keys; `passes` defaults false)
- JSON-only overlay (`id` + `humanReviewOutcome` only, including `null` remove) returning `Ok` with a skip note when the file cannot be patched
- A `tasks.human_review_outcome` column or any new migration
- Copying overlay `id` onto the JSON story, or writing prefixed `dependsOn` ids (must use `strip_prefix_in_id_array`)
- `invalid_state` command-name leftover `"add"` on the update / `patch_user_story` / `atomic_write` / `preflight_from_json_path` path (HEAD helper is private in add.rs and hardcodes `"add"` — extract with `command: &str`)
- `update` importing `commands::add` (copy `InitOpts::resolve_roots` / `default_prd_roots` fallback, or extract next to context helpers)
- Documenting or calling bare `resolve_context` as the write-path resolver. Write-path is `default_prd_roots(db_dir)` → `resolve_context_with_roots(..., "update", Some(&source_root), Some(&worktree_root))`; `ctx is None` → `sole_task_list_path` then `choose_cli_write_path` when roots known, else `cli_write_path`
- Putting ≥2 refuse inside `resolve_context` (breaks `current` probe)
- `if ctx.is_none() { refuse }` (breaks `--no-prefix` / 0-prefix)
- `apply_prefix("")` or `prefix_id("", id)` on empty prefix (lookup id `-FEAT-001`)
- Opening a second connection only to beat overlay parse; unregistered/`≥2` refuse before parse disagrees with add
- Directory `--from-json` waiting for JSON parse (`canonicalize` on a dir succeeds — `is_file()` / `preflight_from_json_path` before parse)
- Remapping `--from-json PATH` away from the canonical flag path
- `--from-json` inserting `prd_files` / `prd_metadata` (pin, never register)
- JSON-sync failure copy (mixed overlay) naming `task-mgr export`
- `prd_json` importing `add` or `update`; `context` importing `prd_json` write helpers
- Hand-editing inside `TASK_MGR:BEGIN/END` instead of `task-mgr enhance agents` after the template rewrite
- Leaving `context.rs` rustdoc saying ≥2 refuse is add-only, or leaving `sole_task_list_path` rustdoc as add-only
- Putting `priority` on the update whitelist; a `set-status` command; claim-scoped short `<task-status>` ids; MCP wrappers; changing DB anchoring
- Rewriting historical `tasks/*-prompt.md`; changing `init::import::update_task` SQL
- Positional `task-mgr update <id> --title/--notes` flag-per-field CLI
- Schema-validating inner `humanReviewOutcome` keys (opaque `Value`)
- Two `--no-prefix` inits as ≥2-prefix sandbox proof (that is 0 prefixes)
- Treating `--no-prefix` as the JSON-only `invalid_state` (no `task_list`) case — `--no-prefix` with one `task_list` must succeed; JSON-only refuse is missing/empty write path
- Claiming in-tree `CLAUDE.md` from verify-task-mgr sandbox `--dir` artifacts (that proof is checkout after FEAT-006)
- Driving verify-task-mgr on a task before the feature file exists; claiming worktree live-path via that harness; driving `loop run` / `batch run`
- Inventing a second verification harness; treating compile/unit tests as proof for clap/overlay-reject/CLARIFY
- Manual edits to `tasks/*.json` for status (use task-mgr CLI / task-status tags)
- unwrap() on filesystem or SQLite in new modules unless a prior invariant makes it unreachable
- tracing for operator-facing overlay/refuse/sync-failure copy (use `ui::emit` / `ui::emit_err`)
- Tests that only assert 'no crash' or check type without verifying content

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy -- -D warnings` (per-iteration). `--all-targets` is REVIEW-001 only
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() on filesystem or SQLite in new production paths (`update.rs` / `patch_user_story`)
- Operator overlay/refuse/sync-failure copy uses `ui::emit` / `ui::emit_err` (CONTRACT-LOG-001), never tracing
- Do not implement PR-3 (export default/`--all`/`--force`, cheatsheet/`task_ops`/historical prompts/best-practices) or re-implement PR-1 remapper/add clap
- Grep `src/commands/update.rs` for `update_task` and `delete_task_relationships` → zero hits
- Grep `src/commands/prd_json.rs` for `use crate::commands::add` / `use crate::commands::update` → zero hits
- No `tasks.human_review_outcome` column and no new migration
- Existing `tests/worktree_db_resolution.rs` add DB-anchoring tests stay green
- Clap parse tests live in `src/cli/tests.rs` and are run with `cargo test -p task-mgr cli::` (not `cargo test --test cli_tests`)
- Later FEATs grep symbols (`refuse_unpinned_write`, `preflight_from_json_path`, `sole_task_list_path`, `choose_cli_write_path`, `cli_write_path`, `default_prd_roots`, `strip_prefix_in_id_array`, `resolve_context_with_roots`, `into_prd_user_story`, `atomic_write`) — do not freeze `file:NNN` line numbers from authoring
- User-facing update/overlay-reject/CLARIFY proof is FEAT-008 + REVIEW-001 via `.claude/skills/verify-task-mgr/SKILL.md` (compile/unit tests alone are not proof for those stories). FEAT-001–007 keep rust tests only — no skill drive

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Project Verification Skills, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

Do **not** edit `tasks/prd-agent-task-ops-pr2.md`, `tasks/prd-goal-agent-task-ops-ux-ledger.md`, `tasks/prd-agent-task-ops-pr1.md`, or later PR-3 files. PR-1 is already this tree — do not hunt a worktree or re-ship remapper/add clap.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/agent-task-ops-pr2.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Inspect this iteration's task          | `task-mgr show <TASK-ID>` using the task ID from `## Current Task`                                                                                                                 |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/agent-task-ops-pr2.json` — PR-1 shipped the flag; pass it so fixups do not leak |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task (rare — e.g., cross-PRD `requires[]`), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/agent-task-ops-pr2.json
jq '.globalAcceptanceCriteria' tasks/agent-task-ops-pr2.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/agent-task-ops-pr2-prompt.md` | This prompt file (read-only)                                               |
| `tasks/progress-$PREFIX.txt` | Progress log — **tail** for recent context, **append** after each task     |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration. Two targeted patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on, or a dependsOn task)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing; never crash on absent files.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Work the task in `## Current Task`** — the loop engine already selected and claimed it at iteration start. Use `task-mgr show <TASK-ID>` only if you need to inspect the pinned task details again. If `## Current Task` says there is no eligible task or unmet cross-PRD `requires`, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `## Current Task` lists a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log (`grep -n -A 40 '## .* - <THAT-TASK-ID>' tasks/progress-$PREFIX.txt`). Skip entirely on the first iteration (file won't exist). CONTRACT dependents: grep `## CONTRACT-001` / `## CONTRACT-002` / `## CONTRACT-003`.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, or the task touches a file that's likely documented there, `grep` for the relevant term and read only the surrounding lines:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```
   The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, Project Verification Skills, Key Context, and the CLAUDE.md excerpts that matter for this PRD) are already embedded in **this prompt file**. Prefer it over re-reading source docs. When a verification skill applies, Read that SKILL.md at verification time — do not paste it into the progress log.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed. Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section or grep 2-3 existing call sites. Never guess key types from variable names.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — and even then, one rejected alternative with a one-line reason is enough. For normal tasks: pick and go.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). If a **Project Verification Skills** entry covers this task, Read that SKILL.md and follow it after the language gate; do not invent a second harness. A green compile/test run is not proof for covered user-facing changes. If the skill is blocked (can't launch, unmet precondition), emit `<promise>BLOCKED</promise>` rather than marking the task done. Fix failures before committing; never commit broken code.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:` as appropriate). Multiple tasks per iteration: `feat: ID1-completed, ID2-completed - [Title]`.

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON. (Legacy `<completed>TASK-ID</completed>` still works; prefer `<task-status>`.)

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---` so the next iteration's tail works.

11. For TEST-xxx tasks: target 80%+ coverage on new methods; use `assert_eq!` on string outputs.

---

## Task Selection (reference)

The loop engine owns selection and claim at iteration start. It injects the claimed task into `## Current Task`; work only that pinned task during this iteration.

To request a different pick on the **next** iteration, emit `<reorder>TASK-ID</reorder>`. The engine will claim that task on the next iteration. Never combine reorder with `next --claim`.

Two runtime checks you DO own:

- If `## Current Task` has `preflightChecks`, run them. If any fails: emit `<task-status><TASK-ID>:skipped</task-status>` with the preflight reason and stop this iteration; the engine will pick the next task on the next iteration.
- If the previous task had a `completionCheck`, run it before starting the new one. If it fails: `task-mgr fail <prev-task> --error "completionCheck failed"` and fix it first.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

1. **ANALYSIS gate**: this PRD already carries `consumerAnalysis` on behavior-modifying FEATs (from the PRD consumer table). Do **not** spawn a new ANALYSIS task. Follow the listed BREAKS / NEEDS_REVIEW mitigations.
2. **Consumer Impact Table** (on the task / in the progress file from CONTRACTs):
   - `BREAKS` → implement the mitigation (do not silently keep old behavior).
   - `NEEDS_REVIEW` → verify callers before implementing.
   - `OK` → proceed.
3. **Semantic distinctions**: `init --append --update-existing` (`import::update_task`, revive) vs `task-mgr update` (partial field patch); mixed overlay pin-11 warning vs JSON-only `invalid_state`; `resolve_context` None (probe) vs write-only refuse at add **and** update; skip `prefix_id` when prefix is empty. Do not shoehorn these into one helper.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **milestones** run the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (implementation / test / fix tasks)

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
# Rust — scope tests to the touched crate/module (grep touchesFiles to pick)
cargo fmt --check
cargo check                                         # fast type check
cargo clippy -- -D warnings
cargo test -p task-mgr commands::update
cargo test -p task-mgr commands::prd_json
cargo test -p task-mgr commands::add
cargo test -p task-mgr commands::init::parse
cargo test -p task-mgr commands::enhance
cargo test -p task-mgr commands::intents
cargo test -p task-mgr commands::how
cargo test -p task-mgr cli::                         # src/cli/tests.rs parse tests (NOT cargo test --test cli_tests)
cargo test --test worktree_db_resolution
cargo test --test cli_tests                          # binary hint tests only when you touch error_recovery / clap UX
```

Scoping heuristic: start from `touchesFiles`. For each Rust file, run `cargo test -p <its crate>` with a module filter when possible. If you can't determine the scope confidently, widen to the whole package (still cheaper than the full workspace).

**Do NOT** run the entire workspace test suite (`cargo test` with no filter) during regular iterations — that's the milestone's job.

**Project verification skill:** if this prompt has a **Project Verification Skills** section, run it after the language gate for covered tasks (see that section). Compile/unit tests alone are not proof for those changes.

### Final gate at REVIEW-001 (the milestone)

The single `REVIEW-001` task at the end of the lean path runs the **full, unscoped** suite on a clean checkout and must finish green. There are no separate MILESTONE-1 or MILESTONE-2 tasks in the reduced-ceremony skeleton.

```bash
# Rust
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

If ANY test fails — including pre-existing failures that predate this PRD — the milestone fixes them. Default: **attempt every failure**, even ones that look out-of-scope. They become scope the moment the milestone gates the phase on the full suite being green. Trunk-green is the invariant this mechanism exists to protect.

Pragmatic escape hatch: if there are **more than ~12 failures AND they're all clearly unrelated to this PRD** (e.g., a sibling team's integration test against a now-missing service), don't try to do all of them inline. Triage:

1. Fix everything you can attribute to this PRD's changes, inline in the milestone commit.
2. For the remaining unrelated failures: spawn a single `FIX-xxx` or `CLARIFY-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/agent-task-ops-pr2.json` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID so a human can route ownership.

Below the ~12-failure threshold, just fix them. Each failure you punt is a tax on every future milestone, so the bar to punt is deliberately high.

If a **Project Verification Skills** section is present, this gate also includes that skill's mapped-feature drive (see that section).

---

## Project Verification Skills

This repo ships a project-level verification skill. Language-level gates (fmt, type-check, lint, scoped tests) are **necessary but not sufficient** for user-facing changes the skill covers. Follow the skill literally — do not invent a second harness, and do not paste the skill body into the progress log.

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** `update-and-human-review-outcome` (FEAT-008 **clones** `features/add-and-current-from-json.md` — file **is on HEAD** — then drives it; REVIEW-001 re-drives). FEAT-001–007 keep rust tests only — no skill drive. Worktree live-path is **not** this skill (FEAT-007 rust tests following `live_worktree_file_exists_writes_worktree_main_unchanged` + `test_from_json_relative_prd_files_worktree_registered`; keep `add_from_worktree_root_lands_in_main_db` green — DB anchoring only). ≥2-prefix refuse in the sandbox requires two `loop init`s **without** `--no-prefix` (distinct `taskPrefix`); do not use two `--no-prefix` copies of `sample_prd`. `--no-prefix` update **succeeds** when exactly one `task_list` is registered — it is **not** the JSON-only `invalid_state` case. JSON-only refuse is missing/empty write path (registered tasks, no usable `task_list` file). In-tree `CLAUDE.md` fenced-block proof is the **checkout** file after FEAT-006 `task-mgr enhance agents` — sandbox `--dir` artifacts cannot prove it. Keep clone traps: absolute sandbox paths; strip `taskPrefix` before `--no-prefix`; helper unsets `TASK_MGR_ACTIVE_PREFIX`.

**Per-iteration:** if this task is FEAT-008, REVIEW-001, or a FIX / WIRE-FIX spawned from them, Read that SKILL.md (and `features/update-and-human-review-outcome.md` if listed) and drive that recipe after the scoped language gate. Capture evidence where the skill says. A green compile/test run is not proof. Do **not** drive the skill on FEAT-001–007. FEAT-008 must **create** the feature file before it drives.

**REVIEW-001 / milestone:** drive every listed feature this PRD touched. A skipped sub-feature is reported skipped, not verified via a sibling path.

**Blocked skill:** if you cannot launch or a precondition fails, emit `<promise>BLOCKED</promise>` with the unmet precondition. Do not skip the drive and mark the task done.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production — CODE-REVIEW-1 verifies. Most common misses:

- Not registered in dispatcher/router → add to registration
- Test mocks bypass real wiring → verify production path separately
- Config field read but not passed through → wire through
- Unused-import warning on new code → call sites missing
- Wrong key type on map access (atom vs string) — struct keys ≠ JSONB keys → check Data Flow Contracts
- New CLI subcommand / DB column / JSON field defined but not threaded into the dispatcher / `TryFrom<Row>` / parse-to-task mapping
- `Commands::Update` clap fields added but `main.rs` dispatch ignores them
- `patch_user_story` defined but `update` still writes JSON itself or calls `append_user_story`
- Overlay validator defined but `update_with_conn` still `from_value::<PrdUserStory>`
- FEAT-003 `pub mod update` missing so `update.rs` is an orphan file; or FEAT-003 landing SQL/JSON writer that belongs in FEAT-004
- Overlay `difficulty`/`estimatedEffort` copied as-is so both keys remain on the JSON story (must write canonical `estimatedEffort` and remove leftover `difficulty`; DB `SET difficulty` is FEAT-004)
- `refuse_unpinned_write` extracted but update skips the call
- `preflight_from_json_path` still private in `add.rs` hardcoding `"add"` — update missing/directory errors say `"add"`
- Bare `resolve_context` used as the write-path (no `default_prd_roots` / `resolve_context_with_roots` / `choose_cli_write_path`)
- `update.rs` `use crate::commands::add` instead of copying `InitOpts::resolve_roots` fallback
- Enhance template rewritten but in-tree `CLAUDE.md` not regenerated
- `sole_task_list_path` rustdoc still says add-only so update skips the helper

---

## Contract Tasks

`CONTRACT-xxx` tasks (`taskType: "contract"`) are **design-only**. Their job is to produce a stable, reviewable foundational contract (interface, data shape, error model, ownership) that 2+ downstream implementation tasks will depend on.

**When you are given a CONTRACT task**:
- Do not write production code or full test suites.
- Produce the precise definition + extreme details (edge cases, invariants, known-bad discriminators, failure modes, alternatives considered + rationale).
- Explicitly list every downstream story / task ID that will depend on this contract.
- Record the **full contract text** in the progress log under a clear `## CONTRACT-001` (or equivalent) header so later agents can read it directly.
- Emit `<task-status>CONTRACT-001:done</task-status>` when the contract is recorded and the acceptance criteria are satisfied.

Downstream FEAT/FIX tasks that list a CONTRACT task in `dependsOn` are expected to implement against the recorded contract. If the contract needs revision, the revision must be done by re-opening the CONTRACT task (or spawning a follow-up CONTRACT-FIX via `task-mgr add`).

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REFACTOR-REVIEW-FINAL`) spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

### What each review looks for

| Review                  | Priority | Spawns (priority)                  | Before                  | Focus                                                                                                   |
| ----------------------- | -------- | ---------------------------------- | ----------------------- | ------------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | early FEATs + CONTRACT  | Language idioms, security, error handling, `qualityDimensions`, wiring, respect for any CONTRACT        |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)             | all implementation      | All code + tests: DRY, complexity, coupling, clarity, contract fidelity — full-context final pass        |

Use the **rust-python-code-reviewer** / equivalent language agent when reviewing code. Document findings in the progress file. If a specific prior iteration produced something ugly and you don't want to wait for REFACTOR-REVIEW-FINAL, invoke `/simplify` on that touchpoint directly — don't file a dedicated review task just for it.

### Spawning follow-up tasks

One shape covers CODE-FIX, WIRE-FIX, and all REFACTOR-N-xxx — vary `id`, `priority`, and include `rootCause`/`exactFix`/`verifyCommand` for fix tasks so the implementing agent lands the fix in one pass:

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-1: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 14,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/agent-task-ops-pr2.json
```

`--depended-on-by` wires the new task into the milestone's `dependsOn` AND syncs the PRD JSON atomically — don't edit the JSON yourself. When a **Project Verification Skills** entry covers the issue, set `verifyCommand` to that skill's drive (the helper or recipe the SKILL.md names), not a unit-test invocation. Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it **tight** — future iterations tail this; verbosity here bloats every later context.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block. If your entry is longer than ~25 lines, compress it — a future iteration has to read this.

CONTRACT tasks: also include the full contract text under `## CONTRACT-00N` in the same (or immediately following) block so dependents can grep it.

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw, auto-appended). **Do not Read those files directly** during a loop iteration — they grow unboundedly. Instead:

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval of learnings scored for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries when recall is sparse

Record your own learnings with `task-mgr learn` so they're indexed for future recall. Don't append directly to those files.

**Write concise learnings** (1-2 lines each):
- GOOD: "`update.rs` must not call `import::update_task`; notes-only leaves `archived_at`"
- BAD: "When implementing the updater we discussed several approaches including reusing the import writer with a flag to skip clearing archived_at but that still full-row SETs omitted fields which is bad."

**Group related tasks** when reporting:

- Instead of separate entries for FIX-001, FIX-002, FIX-003
- Write: "FIX-001 through FIX-003: Fixed X by doing Y"

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in the progress file
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Add to JSON and commit: `chore: Add blocker task CLARIFY-001`
4. Output:

```
<promise>BLOCKED</promise>
```

---

## Milestones

Milestones (here: `REVIEW-001`) are **full-gate checkpoints**: they prove the trunk is green before the next phase begins. They are NOT a sweep to rewrite remaining tasks — stale tasks self-correct when their agent picks them up.

### Milestone Protocol

1. Check all `dependsOn` tasks have `passes: true`. If any don't, the milestone can't run yet.
2. **Run the full quality gate** (see Quality Checks § Milestone gate — unscoped format, type-check, lint, and the complete test suite). This is the ONE place in the loop where the entire test suite runs. If a **Project Verification Skills** section is present, also drive every mapped feature this PRD touched.
3. **Leave the repo green.** For every failure, including pre-existing ones that predate this PRD:
   - Trivial fixes go in the milestone's own commit: `chore: REVIEW-001 - fix stale test <name>`.
   - Non-trivial failures → spawn a `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/agent-task-ops-pr2.json` with the failure's `verifyCommand`. The loop picks it up; the milestone re-runs when the FIX passes.
   - If the failure reveals that a remaining task in this PRD is stale or needs splitting, spawn the correction now. This is the ONLY time milestones touch the task graph — and only in response to a concrete test failure, not a speculative sweep.
4. **Batch sibling PRDs**: skip unless the full suite revealed cross-PRD breakage.
5. Mark the milestone `<task-status>REVIEW-001:done</task-status>` only when the full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

- **[1561]** / **[3440]** / **[5615]** JSON sync is best-effort after DB commit; do not roll back; failure copy names `task-mgr current` and retry `--from-json`, never `export`.
- **[4224]** Use task-mgr CLI exclusively for task operations; never hand-edit JSON.
- **[3419]** / **[5345]** All `tasks.status` mutations through `TaskLifecycle`; update is not a status verb.
- **[3916]** LIFECYCLE-EXCEPTION lint catches raw SQL status updates outside the lifecycle module.
- **[3498]** / **[3283]** / **[3156]** / **[3764]** `humanReviewOutcome` belongs in the task JSON; resolution lands there (now via `update --stdin`) then `complete` the CLARIFY.
- **[2667]** / **[1562]** / **[5577]** / **[4564]** Unique tmp `.{base}.{pid}-{n}-{nanos}.tmp` + same-directory rename (not a fixed `.task-mgr-add.tmp`; not `/tmp`).
- **[5578]** / **[5593]** Append/patch: Value round-trip of the file; only the new story is typed as `PrdUserStory`; centralize writes in `prd_json`.
- **[3923]** / **[5604]** Prefixed DB ids vs unprefixed JSON ids — match both on patch; write `dependsOn` unprefixed.
- **[5597]** Empty/NULL prefix must skip **both** `apply_prefix` and `prefix_id` (else `-FEAT-001`).
- **[5601]** Resolve PRD JSON path once onto `ctx`; never re-locate after commit.
- **[5602]** / **[5596]** `--from-json` is a pin; keep the canonical PATH; never remap it away; never register.
- **[5581]** `resolve_context` pin: flag→env→single prefix→`Ok(None)` for 0 **and** 2+; ≥2 refuse is write-only at the caller, not inside the resolver.
- **[4237]** / **[4441]** / **[3240]** Worktree cwd: write the live copy when it exists; DB stays main `.task-mgr`.
- **[3126]** Pre-flight validation before opening a write transaction.
- **[1252]** PRD JSON key is `userStories`, not `tasks`.
- **[2903]** `TASK_MGR_ACTIVE_PREFIX` leaks into subprocess tests — EnvIsolation required.
- **[5443]** Isolating `HOME` for verify-task-mgr breaks rustup unless `RUSTUP_HOME` / `CARGO_HOME` are kept; use the in-tree helper.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md` to pull just that block.

- **Lifecycle SSoT** (`src/lifecycle/CLAUDE.md`): every `tasks.status` write goes through a `TaskLifecycle` verb. Init's `passes → done` is the one marked `LIFECYCLE-EXCEPTION`. `task-mgr update` must never become a second status door.
- **Never edit `tasks/*.json` directly.** Use CLI subcommands plus `<task-status>`. This PR implements `task-mgr update --stdin` as the field-patch CLI.
- **Human-in-the-loop CLARIFY** (managed `TASK_MGR` block — rewrite via enhance template then `task-mgr enhance agents`, do not hand-edit inside markers): after this PR, pipe `{id, humanReviewOutcome}` to `update --stdin` then `complete <clarify-id>`.
- **Spawn-fixup PRD targeting:** `task-mgr add --stdin` MUST pass `--from-json tasks/agent-task-ops-pr2.json` (or `--depended-on-by REVIEW-001` / `CONTRACT-00x`) or the entry leaks.
- CONTRACT-LOG-001: `ui::*` for all product UX / CLI data / byte-locked operator contracts; `tracing` for internal diagnostics only. Overlay/refuse/sync-failure copy is `ui::emit` / `ui::emit_err`.
- DB from a worktree cwd already lands in **main** `.task-mgr` (`tests/worktree_db_resolution.rs`). Do not change that.
- Tests that spawn the real binary with an init-family command MUST set `HOME` to a tempdir. verify-task-mgr already does this.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments.

**Overlay stdin/JSON** — `&str` → `serde_json::Value` object (string keys, camelCase). **Do not** `from_value::<PrdUserStory>(v)`:

```rust
let v: Value = serde_json::from_str(input)?;
let obj = v.as_object().ok_or(...)?;
```

**Overlay `id`** — JSON string → optional `prefix_id` → `tasks.id` TEXT (lookup only). Skip `prefix_id` when prefix empty. **Never** `obj.insert("id", …)` on the existing story:

```rust
let mut id = obj["id"].as_str()...;
if !ctx.prefix.is_empty() { id = prefix_id(&ctx.prefix, &id); }
```

**Reject keys** — top-level object keys only:

```rust
if obj.contains_key("status") || obj.contains_key("passes") { /* lifecycle_err */ }
for k in obj.keys() {
    if k != "id" && !WHITELIST.contains(k) { /* unknown_err naming all */ }
}
// then type/null table bound to PrdUserStory field types (## CONTRACT-002)
```

**Partial UPDATE** — struct of `Option`s → SQL `SET col = ?` only for `Some`. Never SET `status`, `archived_at`, `priority`, `id`. Always `updated_at = datetime('now')` when any **DB** column/table changes. JSON-only overlay: no `UPDATE tasks`.

**`dependsOn` present** — JSON array of strings → `task_relationships` rows `rel_type = 'dependsOn'` only. **Do not** call `delete_task_relationships`. Skip the delete when key **absent**:

```rust
tx.execute(
    "DELETE FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'",
    [id],
)?;
for dep in arr {
    insert_relationship(tx, id, prefix_id_if_needed(dep), "dependsOn")?;
}
```

**`dependsOn` JSON write** — same array, unprefixed strings. After merge: `strip_prefix_in_id_array(obj, "dependsOn", prefix)` — same helper as `append_user_story`.

**`touchesFiles` present** — JSON array of strings → `task_files`. `delete_task_files` + `insert_task_file` iff key present.

**JSON story patch** — `userStories[]` elements are `Value` objects. **Do not** `from_value::<PrdUserStory>(entry)`. After merge: `entry["id"]` == previous bytes:

```rust
for entry in arr {
    if id_matches(entry, overlay_id, prefix) {
        // merge whitelist keys except id; break
    }
}
```

**Extra keys on existing story** — string keys on the `Value` object. Merge **does not** remove keys not in the overlay (except canonicalising `difficulty` → `estimatedEffort` when that alias is being set). Overlay `difficulty` **or** `estimatedEffort` (not both — validator rejects) writes JSON `estimatedEffort` and **removes** leftover `difficulty`. Either key → DB `SET difficulty` (FEAT-004).

**`humanReviewOutcome`** — JSON object/`null` → `Option<Value>` on `PrdUserStory` / `AddTaskInput` only. **Never** a `tasks` column bind. JSON-only overlay: persist via `patch_user_story` or `invalid_state` — never `Ok` skip:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub human_review_outcome: Option<Value>
```

**Type/null bind (CONTRACT-002)** — overlay checks use these `PrdUserStory` field types (grep `parse.rs`; overlay is stricter than serde Option for `requiresHuman` / `maxRetries` / `title`):

| Overlay key | `PrdUserStory` field | Overlay accept | Overlay reject |
| --- | --- | --- | --- |
| `title` | `title: String` | non-empty string | null, empty, non-string |
| `description`/`notes`/`model`/`escalationNote`/`severity`/`sourceReview`/`estimatedEffort` | `Option<String>` (`difficulty` for effort) | string or `null` (clears) | wrong type |
| `humanReviewTimeout` | `human_review_timeout: Option<u32>` | unsigned integer or `null` (clears) | negative, float, string, bool, array, object |
| `claimsSharedInfra` | `claims_shared_infra: Option<bool>` | bool or `null` (clears) | string (`"true"`), number, array, object |
| `reviewScope` | `review_scope: Option<Value>` | JSON value or `null` (clears); do not `as_str()` | — |
| `dependsOn`/`touchesFiles`/`acceptanceCriteria`/`requiredTests` | `Vec<String>` | array of strings (`[]` clears) | `null`, non-array, non-string element |
| `requiresHuman` | `requires_human: Option<bool>` | JSON bool | `null`, non-bool |
| `maxRetries` | `max_retries: Option<i32>` | integer | `null` (column NOT NULL; do not default to 3), non-integer |
| `humanReviewOutcome` | `human_review_outcome: Option<Value>` | object or `null` (null removes JSON key) | array, string, number, bool |

**`ResolvedContext.prd_json_path`** — PR-1 write path from `resolve_context_with_roots` + `db_dir` roots (not bare `resolve_context`). Mixed: after DB commit, `patch_user_story(&ctx.prd_json_path, …)` only. JSON-only: same path, but `Err` if empty/`patch` fails. Mixed + empty path: pin 11 skip/warn. `ctx is None` → `sole_task_list_path` then **`choose_cli_write_path` when roots known**, else `cli_write_path` — no second `locate_prd_json`.

**Tmp name** — `.{basename}.{pid}-{n}-{nanos}.tmp`. `prd_json::unique_tmp_path`; same-dir rename.

**`invalid_state` command** — `&str` parameter. `"update"` on this command's path; `atomic_write(target, content, command)`; `preflight_from_json_path(path, command)`.

**HEAD extraction targets (do not cite memory; grep symbols — PR-1 is this tree):** `src/commands/init/import.rs` `update_task` / `delete_task_relationships` / `delete_task_files` / `insert_relationship`; `src/commands/init/parse.rs` `PrdUserStory`; `src/commands/add.rs` `AddTaskInput` / `into_prd_user_story` / **private** `preflight_from_json_path` (hardcodes `"add"`) / **private** `default_prd_roots` / inlined ≥2 refuse; `src/commands/prd_json.rs` `unique_tmp_path` / `append_user_story` / **private** `atomic_write` (hardcodes `"add"`) / **private** `strip_prefix_in_id_array`; `src/commands/context.rs` `resolve_context` **and** `resolve_context_with_roots` / `sole_task_list_path` / `choose_cli_write_path` / `cli_write_path` (`refuse_unpinned_write` and `preflight_from_json_path` are **absent** — extract them here); `src/cli/error_recovery.rs` `WRONG_SUBCOMMAND_HINTS`; `src/cli/commands.rs` `Commands::Add` `--from-json` (pin help) — **no** `Commands::Update`; `RunAction::Update` is run-session only. `src/commands/update.rs` is **absent**. `.claude/skills/verify-task-mgr/features/add-and-current-from-json.md` **is on HEAD**.

---

## Feature-Specific Checks

- Grep `src/commands/update.rs` for `update_task` and `delete_task_relationships` → zero hits.
- Grep `SET status` / `archived_at` / `priority` / `id` in `update.rs` **production** SQL → never those columns. Tests may seed `archived_at` via raw SQL — that is not a production SET (CODE-REVIEW-1 must not fail the seed).
- Overlay `difficulty` or `estimatedEffort`: JSON canonical `estimatedEffort` + leftover `difficulty` key removed (FEAT-002 unit); either key → `SET tasks.difficulty` (FEAT-004).
- FEAT-003: `pub mod update` in `commands/mod.rs`; validator only — no SQL/JSON writer.
- Grep `prd_json.rs` for `use crate::commands::add` / `use crate::commands::update` → zero hits.
- Grep `atomic_write` — takes `command: &str`; update path errors say `"update"`.
- Grep `preflight_from_json_path` — lives in `context.rs` with `command: &str`; update missing/directory errors say `"update"` (not leftover `"add"` from HEAD `add.rs`).
- Grep `update.rs` for `use crate::commands::add` → zero hits.
- Write-path: `default_prd_roots` → `resolve_context_with_roots`; `ctx is None` uses `choose_cli_write_path` when roots known. Do not call bare `resolve_context` as the write-path.
- Grep `apply_prefix` / `prefix_id` in `update.rs`: not called when prefix is empty.
- Grep `context.rs` refuse comments **and** `sole_task_list_path` rustdoc: **write-only**, not add-only.
- Grep `WRONG_SUBCOMMAND_HINTS`: no `update` row; `edit`/`change` name `update --stdin`; `set-status` unchanged.
- After FEAT-006: `task-mgr enhance agents` has been run; checkout fenced `CLAUDE.md` contains `update --stdin` and does not tell agents to `Edit` JSON for `humanReviewOutcome`. FEAT-008 sandbox `--dir` artifacts do **not** prove that file.
- `update --help` contains **pin**, not import, for `--from-json`.
- `PRAGMA table_info(tasks)` / migrations: no `human_review_outcome`.
- Directory `--from-json`: `is_file()` / `preflight_from_json_path` **before** overlay parse.
- Clap parse tests: `cargo test -p task-mgr cli::` (not `cargo test --test cli_tests` for `src/cli/tests.rs`).
- Later tasks grep symbols, not frozen `file:NNN` line numbers.
- ≥2-prefix sandbox: two prefixed `loop init`s, not `--no-prefix` twice.
- `--no-prefix` update **succeeds** (one `task_list`). JSON-only `invalid_state` is missing/empty write path — do not use `--no-prefix` as that case.
- FEAT-008 clones `features/add-and-current-from-json.md` (on HEAD); do not invent a recipe; do not list the clone source in `touchesFiles`.
- FEAT-007 follows `live_worktree_file_exists_writes_worktree_main_unchanged` + relative-`prd_files` pin; keep `add_from_worktree_root_lands_in_main_db` green (DB anchoring only).
- `task-mgr update --help` vs `task-mgr run update --help` stay distinct clap paths.

---

## Important Rules

- Work on **ONE story per iteration**
- **For high-effort tasks** (`estimatedEffort: "high"` or 10+ acceptance criteria): consider using `/ralph-loop` to iterate within the task until all acceptance criteria pass, e.g.:
  `/ralph-loop "Implement [TASK-ID]: [title]. Criteria: [list]. Output <promise>DONE</promise> when all pass." --max-iterations 10`
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md` section 8
