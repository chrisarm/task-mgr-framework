# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Agent task-ops UX PR-1 — remapper, context, add/current `--from-json`** for **task-mgr**.

## Problem Statement

Docs and loop prompts already tell agents to pin a destination with `task-mgr add --stdin --from-json tasks/<prd>.json`. Clap does not accept that flag. Cheatsheet CI **forbids** the string. From a linked worktree, `add` still writes the path stored in `prd_files` (usually the main checkout copy). `locate_prd_json` with no pin falls back to the first `task_list` row (`LIMIT 1`), so a ≥2-prefix DB silently appends PRD #1.

Real failures: **#4441** (worktree cwd, JSON sync "No such file or directory" on a bare filename), **#4237** (add wrote main JSON; loop mutates the worktree copy), **#2236** (omitted `--from-json` leaked `WIRE-FIX` / `CODE-FIX` into the wrong PRD).

**This list ships PR-1 only.** PR-2 is `task-mgr update` + `humanReviewOutcome`. PR-3 is export scoping + remaining docs. Do not implement those.

---

## PR-1 scope lock (read every iteration)

In scope: `git::worktree_root_at` / `worktree_root` / `remap_into_worktree`; `commands/context.rs` pin protocol + path identity (b)+(c); `commands/prd_json.rs` unique-tmp chokepoint; clap `--from-json` on **add** and **current** (pin, not import); CLI write-path (remap then `is_file()`); ≥2-prefix unpinned **add** refuse; JSON-sync failure copy; cheatsheet recipe; verify-task-mgr feature + drive.

**Out of scope (do not implement, do not spawn as "helpful" follow-ups):**

- `task-mgr update`, overlay whitelist, `humanReviewOutcome` as a DB column
- Export default/active-PRD, `--all`, `--force` overwrite
- Rewriting historical `tasks/*-prompt.md`, enhance/intents/`task_ops`/best-practices
- Claim-scoped short `<task-status>` ids; a `set-status` command; MCP wrappers
- `add --from-json` creating/registering a new PRD
- Changing DB anchoring (main checkout `.task-mgr` from a worktree stays)
- Sharing CLI `exists()` into loop startup; basename search inside the remapper
- Merging `update_prd_task_passes` into the add writer
- Making `get_project_root` → `git::worktree_root` a required story

Pins (do not rewrite; 1–4 / 11–16 / 19–21 are law for this PR; 5–10 / 17–18 must not be contradicted):

1. `--from-json` on add/update/current/export ships as “pin this already-registered effort”. `--depended-on-by` cannot pin a worktree-only file.
2. Unregistered `--from-json` path: Refuse (`loop init` first). Identity must treat relative `prd_files` + worktree remap as registered.
3. Ambiguous prefix (≥2 non-NULL, no env, no flag): Refuse the write. Zero prefixes / `--no-prefix` still allow DB insert — loop does *not* always set `TASK_MGR_ACTIVE_PREFIX` (`PrefixMode::Disabled`).
4. Worktree JSON: Pure remap, then CLI existence check. Loop remap stays unconditional. `--from-json PATH` always writes that PATH (never remapped away).
11. JSON sync is best-effort; DB commits first. Failure copy names `task-mgr current` and retry `--from-json`, never `export`.
12. One JSON write chokepoint: unique tmp + rename (pid + counter + nanos). Preserve unknown keys on patch. Do not deserialize an existing story to `PrdUserStory` and write it back.
13. `--from-json` never registers a PRD and never remaps the write target.
14. Live remap is path math, not discovery. No `exists()`, no basename search. Relative `prd_files` rows are joined to `source_root` before remap.
15. Loop remap stays unconditional. CLI existence checks are caller-side and must not be shared into startup.
16. Refuse-without-pin applies iff ≥2 registered non-NULL prefixes. Zero-prefix / `--no-prefix` is a different mode: DB insert OK; JSON sync only if exactly one `task_list` is registered.
19. Path identity (canonicalize + `source_root.join` + worktree remap) is one function, used by add / update / current / export overwrite-guard. Match (a) (JSON `taskPrefix` in `prd_metadata`) is a **separate prefix OR**, not that function.
20. Do not ship the multi-prefix refuse before clap has `--from-json`. PR-1 ships both together (this branch).
21. Out of scope list above.

Architect tightenings (encode on the matching FEAT, not a new phase): empty `ctx.prefix` skips `apply_prefix` **and** `prefix_id` on `--depended-on-by`; `ctx is None` && exactly one `task_list` uses remap-then-`is_file()` on that row; drop `LIMIT 1` as a prefix-miss fallback — count==1 lookup only on the None path.

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

- Implementing PR-2 (`task-mgr update`, humanReviewOutcome persistence, error_recovery hint swap, CLARIFY docs) or PR-3 (export scoping, task_ops prompt, enhance/intents remaining alignment, best-practices copy)
- `add --from-json` creating or registering a new PRD, or calling `init` / `register_prd_files`
- `exists()`, dest `canonicalize`, basename search, or directory walk inside `remap_into_worktree`
- Sharing CLI existence policy into loop startup Step 8.5 (pin 15)
- Dropping startup's canonicalize-`source_root` before the helper
- Shipping the ≥2-prefix refuse without clap `--from-json` on add (pin 20)
- Erroring inside `resolve_context` for 2+ prefixes (breaks `current` probe)
- `if ctx.is_none() { refuse }` (breaks `--no-prefix` / 0-prefix insert)
- `apply_prefix("")` or `prefix_id("", id)` on a NULL-prefix `--from-json` pin (inserted id `-FEAT-001`)
- Second `locate_prd_json` write after DB commit (display vs write split, learning #4237)
- Remapping `--from-json PATH` away from the canonical flag path
- Keeping `locate_prd_json` `LIMIT 1` as a prefix-miss fallback (writes PRD #1)
- JSON-sync failure copy naming `task-mgr export`
- Keeping `.{name}.task-mgr-add.tmp` (learning #1562)
- Deserializing existing `userStories` through `PrdUserStory` on append (strips unknown keys; pins 12 / 17)
- Changing DB anchoring (main checkout `.task-mgr` from a worktree stays)
- Rewriting historical `tasks/*-prompt.md`; MCP task wrappers; a `set-status` command; claim-scoped short `<task-status>` ids; putting `priority` on the update whitelist
- Making `get_project_root` → `git::worktree_root` a required story
- Merging `update_prd_task_passes` into the add writer (share `unique_tmp_path` only)
- Inventing a second verification harness; treating compile/unit tests as proof for add/current `--from-json` clap, pin, or refuse
- Driving `loop run` / `batch run` through verify-task-mgr; claiming worktree live-path cases via that harness
- Identity tests seeding a bare basename unless that is what `register_prd_files` stored
- Manual edits to `tasks/*.json` for status (use task-mgr CLI / task-status tags)
- unwrap() on filesystem or SQLite in new modules unless a prior invariant makes it unreachable
- tracing for operator-facing pin/refuse/sync-failure copy (use `ui::emit` / `ui::emit_err`)
- Tests that only assert 'no crash' or check type without verifying content

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy -- -D warnings` (per-iteration). `--all-targets` is REVIEW-001 only
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() on filesystem or SQLite in new production paths
- Operator pin/refuse/sync-failure copy uses `ui::emit` / `ui::emit_err` (CONTRACT-LOG-001), never tracing
- Do not implement PR-2 or PR-3 stories
- Existing `--no-prefix` add unit/integration tests stay green
- Existing `tests/worktree_db_resolution.rs` DB-anchoring tests stay green
- User-facing add/current `--from-json` clap, pin, and refuse proof is FEAT-009 + REVIEW-001 via `.claude/skills/verify-task-mgr/SKILL.md` (compile/unit tests alone are not proof for those stories). FEAT-004 / FEAT-005 / FEAT-006 keep rust tests only — no skill drive

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Project Verification Skills, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

Do **not** edit `tasks/prd-agent-task-ops-pr1.md`, `tasks/prd-goal-agent-task-ops-ux-ledger.md`, or later PR-2/PR-3 files.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/agent-task-ops-pr1.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Inspect this iteration's task          | `task-mgr show <TASK-ID>` using the task ID from `## Current Task`                                                                                                                 |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/agent-task-ops-pr1.json` — if the flag is not yet on the binary (pre-FEAT-004), omit `--from-json` and rely on `--depended-on-by REVIEW-001` + env pin |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task (rare — e.g., cross-PRD `requires[]`), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/agent-task-ops-pr1.json
jq '.globalAcceptanceCriteria' tasks/agent-task-ops-pr1.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/agent-task-ops-pr1-prompt.md` | This prompt file (read-only)                                               |
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
3. **Semantic distinctions**: loop remap vs CLI exists; `init --from-json` (import) vs add/current `--from-json` (pin); `resolve_context` None (probe) vs add-only refuse; skip `apply_prefix` when prefix is empty. Do not shoehorn these into one helper.

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
cargo test -p task-mgr git::                         # remapper
cargo test -p task-mgr commands::add                 # add / context move
cargo test -p task-mgr commands::current
cargo test -p task-mgr commands::prd_json
cargo test -p task-mgr commands::cheatsheet
cargo test --test add_integration
cargo test --test worktree_db_resolution
cargo test --test cheatsheet_drift
cargo test -p task-mgr cli::                         # src/cli/tests.rs parse tests (not cargo test --test cli_tests)
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
2. For the remaining unrelated failures: spawn a single `FIX-xxx` or `CLARIFY-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/agent-task-ops-pr1.json` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID so a human can route ownership.

Below the ~12-failure threshold, just fix them. Each failure you punt is a tax on every future milestone, so the bar to punt is deliberately high.

If a **Project Verification Skills** section is present, this gate also includes that skill's mapped-feature drive (see that section).

---

## Project Verification Skills

This repo ships a project-level verification skill. Language-level gates (fmt, type-check, lint, scoped tests) are **necessary but not sufficient** for user-facing changes the skill covers. Follow the skill literally — do not invent a second harness, and do not paste the skill body into the progress log.

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** `add-and-current-from-json` (FEAT-009 writes the feature file then drives it; REVIEW-001 re-drives). FEAT-004 / FEAT-005 / FEAT-006 keep rust tests only — no skill drive. FEAT-004b write-path is **not** this skill (sandboxes are not worktrees; live write path is FEAT-005 rust tests). Worktree live-path is **not** this skill (FEAT-005 rust tests). ≥2-prefix refuse in the sandbox requires two `loop init`s **without** `--no-prefix` (or two files with distinct `taskPrefix`); do not use two `--no-prefix` copies of sample_prd.

**Per-iteration:** if this task is FEAT-009, REVIEW-001, or a FIX / WIRE-FIX spawned from them, Read that SKILL.md (and `features/add-and-current-from-json.md` if listed) and drive that recipe after the scoped language gate. Capture evidence where the skill says. A green compile/test run is not proof. Do **not** drive the skill on FEAT-004 / FEAT-005 / FEAT-006 / FEAT-004b.

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
- `remap_into_worktree` defined but startup still inlines `strip_prefix` without calling it
- `prd_json::unique_tmp_path` defined but `prd_reconcile` still has a private copy
- `Commands::Add` / `Current` clap fields added but `main.rs` dispatch ignores them
- `FromJsonFlag` wired in context but add still calls `resolve_context(conn)` with no path

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
}' | task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/agent-task-ops-pr1.json
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
- GOOD: "`remap_into_worktree` must not `exists()`; CLI writers check `is_file()` after the helper returns"
- BAD: "When implementing the remapper we discovered after much discussion that calling exists inside the helper would cause startup to stop remapping a missing dest which is bad because the loop copies files later."

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
   - Non-trivial failures → spawn a `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/agent-task-ops-pr1.json` with the failure's `verifyCommand`. The loop picks it up; the milestone re-runs when the FIX passes.
   - If the failure reveals that a remaining task in this PRD is stale or needs splitting, spawn the correction now. This is the ONLY time milestones touch the task graph — and only in response to a concrete test failure, not a speculative sweep.
4. **Batch sibling PRDs**: skip unless the full suite revealed cross-PRD breakage.
5. Mark the milestone `<task-status>REVIEW-001:done</task-status>` only when the full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

- **[4441]** `task-mgr add` from a worktree cwd with `TASK_MGR_DIR` → main `.task-mgr` silently fails JSON sync on a bare filename.
- **[4237]** add wrote the **main** JSON; loop `prd_path` is the **worktree** copy; `update_prd_task_passes` then "Task in PRD not found". Display vs write must be the same path (`ctx.prd_json_path`).
- **[2236]** omitted `--from-json` leaks `WIRE-FIX` / `CODE-FIX` into the wrong PRD JSON — refuse ≥2 unpinned writes; ship the flag.
- **[3240]** path resolution from a worktree cwd is not "the worktree JSON" today.
- **[1562]** `.{filename}.task-mgr-add.tmp` is the old name; replace with pid-counter-nanos.
- **[3440]** / **[1561]** JSON sync is best-effort after DB commit; do not roll back the row.
- **[2667]** same-directory tmp + rename (cross-filesystem `/tmp` rename is not atomic).
- **[2303]** / **[2798]** loop re-imports the worktree copy; writing only main is silently overwritten.
- **[2903]** / **[4876]** `TASK_MGR_ACTIVE_PREFIX` leaks into subprocess tests — EnvIsolation + `env_remove` required (`tests/worktree_db_resolution.rs` already does this).
- **[5443]** isolating `HOME` for verify-task-mgr breaks rustup unless `RUSTUP_HOME` / `CARGO_HOME` are kept; use the in-tree helper, do not invent a sandbox.
- **[3126]** pre-flight validation before opening a write transaction (`--from-json` missing/directory/unregistered).
- **[1252]** PRD JSON key is `userStories`, not `tasks`.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md` to pull just that block.

- Feature worktrees: `$HOME/projects/task-mgr-worktrees/<branch-name>/`; parallel-slot worktrees: `…/<branch-name>-slot-<N>/` (slot 0 reuses the feature worktree).
- **Spawn-fixup PRD targeting:** `task-mgr add --stdin` MUST disambiguate the destination PRD or the entry leaks into whatever JSON the CLI defaults to. Two forms: (a) `--from-json tasks/<correct-prd>.json` — explicit path; (b) `--depended-on-by CONTRACT-001` (or the PRD's final milestone). Symptom of getting it wrong: orphan `passes: false` placeholders show up in an unrelated PRD during merge-back. **This PRD is the clap implementation of (a).**
- **Never edit `tasks/*.json` directly.** Use CLI subcommands plus `<task-status>`.
- CONTRACT-LOG-001: `ui::*` for all product UX / CLI data / byte-locked operator contracts; `tracing` for internal diagnostics only. Pin/refuse/sync-failure copy is `ui::emit` / `ui::emit_err`.
- DB from a worktree cwd already lands in **main** `.task-mgr` (`db::path::resolve_db_dir` + `tests/worktree_db_resolution.rs`). Do not change that.
- Tests that spawn the real binary with an init-family command MUST set `HOME` to a tempdir or they'll stage into the developer's real `~/.claude/commands/`. verify-task-mgr already does this.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments.

**`prd_files.file_path`** — SQLite `TEXT` → `PathBuf` (relative **or** absolute; `register_prd_files` uses `strip_prefix(tasks_dir).unwrap_or(json_path)`):

```rust
let registered = PathBuf::from(row);
let resolved = if registered.is_absolute() {
    registered.clone()
} else {
    source_root.join(&registered)
};
```

**PRD JSON `taskPrefix`** — `PrdFile.task_prefix: Option<String>` (`camelCase` `taskPrefix`) → `prd_metadata.task_prefix`. Read off `serde_json::Value`; do **not** deserialize the whole file to `PrdUserStory`:

```rust
let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
let prefix = v.get("taskPrefix").and_then(|x| x.as_str());
```

**`--from-json PATH`** — clap `Option<PathBuf>` → canonical `PathBuf`. Registered if (a) **or** pin-19 identity (b)/(c). `ResolvedContext.prd_json_path = canon` (never remapped away):

```rust
let canon = fs::canonicalize(path).map_err(...)?;
```

**Pin 19 identity (b)+(c) only** — join relative `registered` to `source_root`, then `canonicalize(flag) == canonicalize(resolved)` **or** `canonicalize(flag) == remap_into_worktree(registered, source_root, worktree_root)`. Match (a) is **not** this function (`prefixes.contains(&json_task_prefix)`). A stray copy with the same prefix is registered via (a); pin 4 then writes **that** PATH.

**Remap** — path math only (helper does **not** `canonicalize` dest):

```
remap_into_worktree(registered, source_root, worktree_root) -> PathBuf
  resolved = if registered.is_absolute() { registered }
             else { source_root.join(registered) }
  if let Ok(rel) = resolved.strip_prefix(source_root)
    -> worktree_root.join(rel)
  else -> resolved
```

Startup Step 8.5 **keeps** canonicalize-`source_root` then calls the helper.

**CLI write target** — remap then `is_file()` **caller-side**; stored on `ctx.prd_json_path`:

```rust
let target = remap_into_worktree(...);
let write = if target.is_file() {
    target
} else if resolved.is_file() {
    resolved
} else {
    /* skip / PathBuf::new() */
};
// After DB commit:
append_user_story(&ctx.prd_json_path, …) // only — no second locate_prd_json
```

`ctx is None` → JSON sync iff exactly one `task_list` row (remap-then-`is_file()` on that row).

**`ResolvedContext`** — `ctx.prefix` (`String`; empty when the matched `prd_metadata.task_prefix` is NULL); `ctx.source` (`FromJsonFlag` displays `from-json`); `ctx.prd_json_path` = write path. Empty prefix ⇒ skip `apply_prefix` **and** `prefix_id` on `--depended-on-by`.

**JSON `userStories`** — `serde_json::Value` object; array under string key `"userStories"`:

```rust
root_obj["userStories"].as_array_mut();
// push a to_value(new_story) object
// Do NOT map existing entries through PrdUserStory
```

**Tmp name** — `.{basename}.{pid}-{n}-{nanos}.tmp` next to target. Shared `unique_tmp_path`; same-directory rename.

**HEAD extraction targets (do not cite memory):** `src/commands/add.rs` `ResolutionSource` / `resolve_context` / `locate_prd_json` / `append_task_to_prd_json` / `atomic_write`; `src/loop_engine/startup.rs` Step 8.5 ~663–705; `src/loop_engine/prd_reconcile.rs:29-43` `unique_tmp_path`; `src/git/mod.rs` has `main_repo_root_at` only — **no** `worktree_root` yet; `src/cli/commands.rs` Add has no `--from-json`; Current is a unit variant.

---

## Feature-Specific Checks

- Grep `src/loop_engine/startup.rs` Step 8.5: no `exists()` on the remap path; `canonicalize` of `source_root` still present before the helper.
- Grep `locate_prd_json` / `apply_prefix` in `src/commands/add.rs` (and `context.rs` after the move) — do not trust frozen authoring line numbers (`add.rs:345` etc.). After `tx.commit()`: no `locate_prd_json` on the write path; append uses `ctx.prd_json_path`.
- Grep `apply_prefix` / `prefix_id`: not called when `ctx.prefix` is empty.
- Grep `task-mgr export` in `src/commands/add.rs` → zero hits.
- Grep `task-mgr-add.tmp` in production code → zero hits.
- Grep `LIMIT 1` on unscoped `prd_files … file_type = 'task_list'` fallback → gone.
- `tests/cheatsheet_drift.rs` still forbids `set-status`, `recall --top-k`, `learnings show`; no longer forbids `add --from-json`.
- `add --help` / `current --help` contain **pin**, not import, for the new flag.
- Identity tests seed `prd_files` as `tasks/foo.json` or absolute, never a bare basename unless init stored that.

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
