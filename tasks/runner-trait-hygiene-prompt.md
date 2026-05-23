# Claude Code Agent Instructions

You are an autonomous coding agent implementing **LlmRunner Trait Hygiene — Phase 1 (cleanup_session + FakeRunner)** for **task-mgr**.

## Problem Statement

The `LlmRunner` abstraction at `src/loop_engine/runner.rs:222` exposes a per-call opt-in flag `cleanup_title_artifact: bool` at `runner.rs:166` that controls a Claude Code 2.1.110 workaround for an ai-title artifact leak. The flag defaults to `false`. Of 8 production spawn call sites, 5 had opted in; 3 (the main coding-iteration sites at `engine.rs:656`, `engine.rs:2587`, and `prd_reconcile.rs:672`) had never been touched. Over months of automated loop runs, **~4,500 orphan `<uuid>.jsonl` ai-title metadata stubs accumulated across `~/.claude/projects/*/`**.

A /spike on 2026-05-19 traced the same bug class one provider deeper. The `grok` CLI has no `--no-session-persistence` equivalent and writes a *directory* of artifacts per session at `~/.grok/sessions/<percent-encoded-cwd>/<uuid>/`. `GrokRunner::spawn` at `runner.rs:489` currently destructures `cleanup_title_artifact: _` and silently ignores it. Every Grok-fallback iteration leaks a full session directory — strictly worse than Claude's single-file leak.

The fix is structural: cleanup belongs to the runner abstraction. Phase 1 adds `cleanup_session` to the `LlmRunner` trait, implements it on both `ClaudeRunner` and `GrokRunner`, wires `dispatch` to call it unconditionally post-spawn, removes the `cleanup_title_artifact: bool` field from `RunnerOpts` (and all 11 call sites), adds a `FakeRunner` test seam, and places `WORKAROUND(...)` comment markers so future upstream-fix removal is a one-grep operation. Phases 2–5 (capability discovery, error taxonomy, args builder, RunnerSession RAII) are explicitly out of scope.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate cleanup edge cases: NotFound silent, PermissionDenied banner, missing working_dir, parallel slots, mtime tiebreaker) → **PHASE 2 FOUNDATION** (cleanup_session is the first capability-aware trait method; the FakeRunner seam will be reused by Phases 2-5 of the hygiene roadmap; explicit-call shape soaks before Phase 5 RAII layers on top) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting, CLAUDE.md cleanup_session subsection).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `.unwrap()` in production cleanup paths). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that hand-build HashMaps as RunnerOpts mock state — use the real RunnerOpts struct so wrong field names fail at compile time
- Enumerate-and-sweep cleanup (read_dir + glob) — must delete ONLY the (session_id, cwd) tuple this spawn created, never the shared dir
- Deleting Grok's per-cwd ~/.grok/sessions/<encoded>/prompt_history.jsonl — that file accumulates across sessions by design
- Adding status, lifecycle, reconciliation, or <task-status> logic inside the dispatch post-spawn cleanup hook — task lifecycle belongs to the coherence-refactoring effort; this PRD's hook stays single-purpose
- Tidying adjacent code in engine.rs at the field-removal sites (lines 656, 2587) — the coherence Phase 1 carve owns those seams; this PRD only deletes the cleanup_title_artifact: true line
- Reimplementing dash-encoding for the Claude artifact path — reuse encoded_cwd_dir at claude.rs:51
- .unwrap() in any cleanup path — use match or ? on Result; errors propagate to dispatch which handles the banner
- Detached threads / tokio::spawn / async cleanup — sync only; detached threads don't survive parent CLI exit (learning [2674])
- Banner spam — every cleanup error must be gated by CLEANUP_WARN_ONCE (single AtomicBool::swap with Relaxed ordering)
- Generating the taskPrefix manually in the JSON — task-mgr init auto-generates it; setting it by hand can cause prefix collisions
- Refactoring other RunnerOpts fields (db_dir, signal_flag, timeout) under cover of this PRD — they encode genuine per-caller policy and are out of scope

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: Scoped tests for touched files pass with `cargo test`
- Rust: `cargo fmt --check` passes
- No new `.unwrap()` or `.expect()` in production code paths (existing `#[cfg(test)]` allow continues)
- All `eprintln!` warnings include enough context to identify the failed operation (provider, path, error kind)
- No detached threads, no `tokio::spawn`, no async cleanup — sync only
- Comments explain WHY (non-obvious constraint), never narrate WHAT — the `WORKAROUND(...)` markers carry the explanation

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/runner-trait-hygiene.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by MILESTONE-N` — priority auto-computed; DB + PRD JSON updated atomically                                                   |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

### Files you DO touch

| File                                       | Purpose                                                                |
| ------------------------------------------ | ---------------------------------------------------------------------- |
| `tasks/runner-trait-hygiene-prompt.md`     | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`               | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on, or a dependsOn task)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/runner-trait-hygiene.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   Output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes`. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `task-mgr next` listed a `dependsOn` task whose rationale you need (e.g. FEAT-006's dependency on ANALYSIS-001's Consumer Impact Table), grep that specific task's block.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for THIS task. **Do not** Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** The CLAUDE.md content that matters for THIS PRD is embedded in the **CLAUDE.md Excerpts** section below. If a task description cites a section name not shown here, grep for it (`grep -n -A 10 '<keyword>' CLAUDE.md`).

4. **Verify branch** — `git branch --show-current` should match `feat/runner-trait-hygiene`. Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section below.
   - Pick an approach. Survey alternatives only when `estimatedEffort: "high"` OR `modifiesBehavior: true` — one rejected alternative with a one-line reason is enough.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (Quality Checks § Per-iteration). Fix failures before committing.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`/`chore:` as appropriate).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON.

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---`.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` already picks: eligible tasks (`passes: false`, deps complete, not `requiresHuman`), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority. You don't pick — you claim what it returns.

Two runtime checks you DO own:

- If the returned task has `preflightChecks`, run them. If any fails: `task-mgr skip <TASK-ID> --reason "<preflight failure>"` and re-run `task-mgr next`.
- If the previous task had a `completionCheck`, run it before starting the new one. If it fails: `task-mgr fail <prev-task> --error "completionCheck failed"` and fix it first.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

For this PRD: **FEAT-003** (Claude unconditional `--session-id`) and **FEAT-006** (wire dispatch to call cleanup_session post-spawn, remove inline cleanup call) are the modifiesBehavior tasks. **ANALYSIS-001** is the prerequisite for FEAT-006.

1. ANALYSIS-001 must have `passes: true` and produce a Consumer Impact Table in the progress file before FEAT-006 runs.
2. FEAT-003 changes Claude behavior: ALL Claude iterations now emit `--session-id` and create an artifact, regardless of whether the caller used to set `cleanup_title_artifact: true`. The inline cleanup at runner.rs:422 still fires (transitional), so the net effect is leak-then-cleanup for every iteration.
3. FEAT-006 changes dispatch behavior: cleanup moves from runner.rs:422 (inline in ClaudeRunner::spawn) into dispatch (post-spawn for both providers). Read the Consumer Impact Table BEFORE implementing and confirm: the file-deletion outcome is identical (same FS operation, different call site).
4. Semantic distinction: dispatch's post-spawn cleanup hook is SINGLE-PURPOSE. Adding status/lifecycle/reconciliation logic crosses the coherence-refactoring effort's boundary — see the "Boundary with Coherence Refactoring" section of the PRD and the "Coherence-refactoring boundary" notes on each FEAT task.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **milestones** run the full gate and must leave the repo fully green.

### Per-iteration scoped gate

```bash
# Scope tests to touched files
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test --lib loop_engine                                            # for runner.rs / claude.rs unit tests
cargo test --lib loop_engine::runner 2>&1 | tee /tmp/t.txt | tail -3 && grep -E 'FAILED|error\[' /tmp/t.txt | head -10
```

For tests in `tests/`:
```bash
cargo test --test runner_cleanup           # new integration test (FEAT-009)
cargo test --test runner_trait_dispatch    # pre-existing dispatch tests
cargo test --test grok_runner_unit         # pre-existing Grok unit tests
cargo test --test grok_runner_integration  # pre-existing Grok integration tests
```

**Do NOT** run the entire workspace test suite (bare `cargo test`) during regular iterations — that's the milestone's job.

### Milestone gate (MILESTONE-1 / -2 / -FINAL)

Milestones run the **full, unscoped** suite and must finish green:

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
cargo run --bin gen-docs -- --check    # model constants doc sync
```

If ANY test fails (including pre-existing failures), the milestone fixes them. Default: attempt every failure. Escape hatch: >12 unrelated failures → spawn `FIX-xxx` task via `task-mgr add --stdin --depended-on-by <THIS-MILESTONE>` listing the failing tests, and `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production. Most common misses for THIS PRD:

- `cleanup_session` trait method defined but only the default no-op impl exists for one of the runners — both ClaudeRunner and GrokRunner MUST override.
- `RunnerResult.session_id` added but ClaudeRunner still uses the `.then(||)` gate — Claude now must generate UUID unconditionally (FEAT-003).
- `grok_encoded_session_dir` defined but `urlencoding` crate not in `Cargo.toml` → compilation fails.
- `cleanup_claude_session_artifact` promoted but the four import sites at `claude.rs:3020/3087/3121/3138` not updated → tests reference the old name.
- `dispatch` calls `cleanup_session` but doesn't gate on `result.session_id.is_some()` → panics or unintended cleanup when GrokRunner couldn't capture a uuid.
- `WORKAROUND(...)` markers missing on the cleanup sites → future upstream-fix removal becomes archaeology instead of a one-grep operation.
- `CLEANUP_WARN_ONCE` left at `claude.rs:739` with file-private visibility → `runner.rs` can't reach it; banner emission compiles fail.
- Inline cleanup call at `runner.rs:422` not removed in FEAT-006 → cleanup fires twice per Claude iteration (works but wasteful + violates the "dispatch owns cleanup" principle).
- `cleanup_title_artifact: true` left in any of the 11 sites after FEAT-007 → `grep` finds residual hits; PRD Success Metric fails.
- `tests/grok_runner_unit.rs::grok_runner_silently_ignores_cleanup_title_artifact` retained → asserts a contract that no longer exists.
- `test_cleanup_title_artifact_false_omits_session_id` at `claude.rs:2948` retained → fails because Claude now always injects `--session-id`.

---

## Review Tasks

Review-type tasks spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

| Review                  | Priority | Spawns (priority)                  | Before            | Focus                                                                                              |
| ----------------------- | -------- | ---------------------------------- | ----------------- | -------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 17       | `CODE-FIX` / `WIRE-FIX` (14-16)    | MILESTONE-1       | unwrap, error propagation, NotFound semantics, prompt_history.jsonl preservation, WORKAROUND markers, dispatch single-purpose |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)             | MILESTONE-FINAL   | DRY between ClaudeRunner/GrokRunner cleanup impls (restrained — Phase 2-5 will reshape anyway)     |

Use the **rust-python-code-reviewer** agent. Document findings in the progress file. Spawning follow-ups:

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-1: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 14,
  "touchesFiles": ["src/loop_engine/runner.rs"]
}' | task-mgr add --stdin --depended-on-by MILESTONE-1
```

If no issues, emit `<task-status><REVIEW-ID>:done</task-status>` with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight — future iterations tail this.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block.

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw). **Do not Read those files directly.** Use:

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries

Record learnings with `task-mgr learn` (don't append directly to the files).

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

If verified: `<promise>COMPLETE</promise>`

### Blocked Condition

If blocked: document in progress file, create CLARIFY-xxx task with priority 0, commit, output `<promise>BLOCKED</promise>`.

---

## Milestones

Milestones are **full-gate checkpoints**: they prove the trunk is green before the next phase begins. NOT sweeping rewrites — stale tasks self-correct when their agent picks them up.

1. Check all `dependsOn` tasks have `passes: true`. If any don't, milestone can't run yet.
2. **Run the full quality gate** (Quality Checks § Milestone gate — complete test suite). This is the ONE place in the loop where the entire suite runs.
3. **Leave the repo green** — for every failure (including pre-existing): trivial → fix in milestone commit; non-trivial → spawn `FIX-xxx` via `task-mgr add --depended-on-by <THIS-MILESTONE>`.
4. Mark milestone done only when full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here.

- **[2847]** Use deterministic UUID for safe cleanup in shared directories — the (session_id, cwd) tuple is the target; never enumerate-and-sweep. THIS PRD's foundation.
- **[1614]** Best-effort cleanup must not fail the parent operation — banner-and-continue pattern. Cleanup outcome never modifies the spawn return value.
- **[1626]** Opt-in cleanup flag (`cleanup_title_artifact: bool`) threaded through `spawn_claude` signature. THIS PRD SUPERSEDES this learning — the corrected pattern is "cleanup as a trait method, called unconditionally by dispatch."
- **[1617]** Do NOT reuse `cleanup_ghost_sessions` for `~/.claude/projects/` cleanup — different directory (`~/.claude/sessions/` for ghost-sessions), different bug class, different cleanup mechanism. Preserved invariant.
- **[2674]** Detached threads don't survive parent CLI exit; sync cleanup only. No `tokio::spawn`, no detached `std::thread`. Preserved invariant.
- **[2891]** Extract common subprocess scaffolding when adding the second agent implementation — applies to spawn scaffolding, NOT necessarily cleanup. Cleanup impls are tiny (1 line for Claude, ~10 for Grok); DRY pressure is low. REFACTOR-REVIEW-FINAL should be restrained.
- **[2956]** `RunnerKind` enum dispatch keeps allocation-free; no `Box<dyn LlmRunner>` on the hot path. The no-op default `cleanup_session` impl does NOT introduce dynamic dispatch overhead — resolved statically per impl.
- **[2939]** Multi-step visibility widening for internal refactoring — applies to promoting `cleanup_title_artifact_sync` from `claude.rs` (file-private) to `runner.rs` (module-private). Also applies to `CLEANUP_WARN_ONCE` (lift to `pub(crate)` or move).
- **[2919]** Integration test mirrors unit test shape for consistency — `tests/runner_cleanup.rs` mirrors `tests/runner_trait_dispatch.rs`.
- **[2736]** Rename refactor: update all sites and verify completeness via grep. `cleanup_title_artifact_sync` → `cleanup_claude_session_artifact` plus removal of the `cleanup_title_artifact` field across 11 call sites.
- **[1581]** Adding a new `spawn_claude` param requires updating many mechanical sites — this PRD is the inverse (removal); same discipline applies. Use cargo check as the enforcement loop.
- **[795]** `replace_all` misses instances with different indentation levels — DO NOT use blind `replace_all` for the field-removal sweep (FEAT-007). Edit each site individually after grep enumerates them.
- **[810]** Grep for struct field locations across all construction sites — applies to the `RunnerResult.session_id` addition (FEAT-003) and the `cleanup_title_artifact` removal (FEAT-007).
- **[2061]** Comprehensive struct field refactoring with zero regressions — pattern for FEAT-003 (`RunnerResult.session_id: Option<Uuid>`).
- **[658]** Adding a field to a struct with single construction site is low-risk — `RunnerResult` has 2 prod sites + 1 test alias, still low-risk.
- **[1610]** Collapse `Option<T>` if/else into `bool::then` for clarity — INVERSE applies in FEAT-003: we're removing the `cleanup_title_artifact.then(|| { ... })` gate because the value should be unconditional.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets are extracted from `src/loop_engine/CLAUDE.md` for the subsystems this PRD touches. Do NOT Read the full file.

**LLM runner dispatch** (the surface this PRD modifies):

- Touchpoints row: `LLM runner dispatch | src/loop_engine/runner.rs + src/loop_engine/engine.rs | RunnerKind, dispatch, ClaudeRunner, GrokRunner, resolve_effective_runner`
- `RunnerKind` is a static-dispatch enum — no `Box<dyn LlmRunner>` on the hot path; the no-op default `cleanup_session` impl does NOT introduce dynamic dispatch.
- `dispatch` at `runner.rs:877` is the single routing point — `ClaudeRunner.spawn` vs `GrokRunner.spawn`. THIS PRD extends `dispatch` with a post-spawn `cleanup_session` call when `result.session_id.is_some()`.
- Provider routing uses `model::provider_for_model` (token equality on `-` splits of the lowercased model id). NOT relevant to cleanup but worth knowing the dispatch context.

**Auto-launch /review-loop after loop end** — not directly modified by this PRD; worth knowing the loop-completion flow exists.

**Parallel-slot scheduling** (relevant for FEAT-004's concurrency reasoning):

- `IterationContext` is NOT thread-safe; slot workers may only read context fields snapshotted into their own state. THIS PRD's cleanup_session impls do not touch IterationContext; they run inside dispatch (called from each iteration's main thread, sequential OR slot worker).
- Distinct worktree cwds → distinct encoded session dirs → no cross-slot interference. The PRD §6 Risk #1 mitigation calls for an explicit test of this property; TEST-001 (or FEAT-009 test 5) exercises it.

**Touchpoints to know about (not modified, just referenced):**

- `Provider routing | src/loop_engine/model.rs | Provider, provider_for_model` — outside scope.
- `Operator escape valve | src/loop_engine/engine.rs | check_override_invalidation, IterationContext::overflow_original_task_model` — outside scope.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures in this PRD. Use these exactly — do NOT guess key types from variable names.

### 1. Session id from spawn → cleanup (Claude)

| Layer | Type | Location |
|---|---|---|
| Generated | `Uuid` (typed, `uuid::Uuid::new_v4()`) | `ClaudeRunner::spawn` (post-FEAT-003: unconditional) |
| Carried | `RunnerResult.session_id: Option<Uuid>` | `runner.rs:102` (post-FEAT-003) |
| Read by | `dispatch` reads as `Option<Uuid>` | `runner.rs:877` (post-FEAT-006) |
| Passed to | `cleanup_session(uuid, &cwd)` | trait method, both impls |

Copy-pasteable access at the dispatch site:
```rust
if let Ok(ref result) = spawn_result
    && let Some(sid) = result.session_id
{
    let cwd: PathBuf = opts.working_dir.map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    if let Err(e) = runner.cleanup_session(sid, &cwd)
        && !CLEANUP_WARN_ONCE.swap(true, Ordering::Relaxed)
    {
        eprintln!("[cleanup warn] {}: {} ({})", provider_tag(kind), e, cwd.display());
    }
}
```

### 2. Session id from spawn → cleanup (Grok)

| Layer | Type | Location |
|---|---|---|
| Pre-spawn snapshot | `HashSet<String>` of entry names | `GrokRunner::spawn` (FEAT-004) |
| Post-wait snapshot | `HashSet<String>` | same |
| Diff result | `Vec<Uuid>` (via `Uuid::parse_str`, skip non-uuid entries) | same |
| Resolved | `Option<Uuid>` (None / pick by mtime if multiple) | `RunnerResult.session_id` |

Copy-pasteable capture:
```rust
let grok_dir = grok_encoded_session_dir(&cwd, &home);
let before: HashSet<String> = std::fs::read_dir(&grok_dir)
    .map(|d| d.filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect())
    .unwrap_or_default();
// ... spawn child, wait ...
let after: HashSet<String> = std::fs::read_dir(&grok_dir)
    .map(|d| d.filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect())
    .unwrap_or_default();
let new_ids: Vec<Uuid> = after.difference(&before)
    .filter_map(|s| Uuid::parse_str(s).ok())
    .collect();
let session_id: Option<Uuid> = match new_ids.len() {
    0 => None,
    1 => Some(new_ids[0]),
    _ => pick_newest_by_mtime(&grok_dir, &new_ids),  // mtime tiebreaker
};
```

### 3. cwd from opts → cleanup target path

| Layer | Type | Default |
|---|---|---|
| Source | `opts.working_dir: Option<&Path>` | provided by caller |
| Fallback | `std::env::current_dir() -> io::Result<PathBuf>` | parent cwd if `None` |
| Final | `PathBuf` passed to `cleanup_session(&self, sid, cwd: &Path)` | — |

This fallback mirrors `cleanup_title_artifact_sync`'s behavior at `claude.rs:1154` pre-promotion. Behavior must be bit-identical.

### 4. cwd → encoded artifact path (per provider)

**Claude:**
```
<home>/.claude/projects/<dash-encoded-cwd>/<uuid>.jsonl
```
- `encoded_cwd_dir(cwd, home)` lives at `claude.rs:51` — REUSE; do NOT re-implement.
- Encoding: `/` → `-`. Example: `/home/chris/repo` → `-home-chris-repo`.

**Grok:**
```
<home>/.grok/sessions/<percent-encoded-cwd>/<uuid>/
```
- `grok_encoded_session_dir(cwd, home)` lives in `runner.rs` (NEW; FEAT-002).
- Encoding: `urlencoding::encode` of the absolute cwd. Example: `/home/chris/repo` → `%2Fhome%2Fchris%2Frepo`.
- **NEVER touch `<home>/.grok/sessions/<percent-encoded-cwd>/prompt_history.jsonl`** — that file accumulates across sessions by design (PRD §2.5 Correctness Requirement).

### 5. HOME env var resolution

| Layer | Type | On absence |
|---|---|---|
| `std::env::var("HOME")` | `Result<String, VarError>` | helper returns `Ok(())` (best-effort skip) |
| `PathBuf::from(home)` | `PathBuf` | passed to encoder helpers |

Mirroring `cleanup_title_artifact_sync`'s behavior at `claude.rs:757-760` pre-promotion: if HOME is unset or empty, the helper short-circuits with `Ok(())`. This is "best-effort skip", NOT an error.

### 6. CLEANUP_WARN_ONCE rate-limit

| Layer | Type | Semantics |
|---|---|---|
| Static | `AtomicBool` | initialized `false` |
| Gate | `swap(true, Ordering::Relaxed)` | returns OLD value — only the first caller sees `false` and prints |
| Reset | NEVER in production | `#[cfg(test)]` helper acceptable for test isolation |

Pattern is at `claude.rs:775` today. THIS PRD lifts visibility to `pub(crate)` (or moves to `runner.rs` — implementer's call) and uses it from both the Claude and Grok cleanup banner sites.

---

## WORKAROUND() Marker Convention

Two greppable markers MUST exist post-Phase-1 so future upstream-fix removal is a one-grep operation:

```
WORKAROUND(claude-code-2.1.110-session-stub):  (on the Claude cleanup site)
WORKAROUND(grok-cli-no-persistence-off):       (on the Grok cleanup site)
```

After workaround removal (when Anthropic or xAI ships the upstream fix), `grep -rn "WORKAROUND(...)" src/` returns exactly the lines that need to be deleted. The default no-op `cleanup_session` impl handles the "no artifact to clean" case automatically.

PRD Success Metric: `grep -rn "WORKAROUND(claude-code-2.1.110-session-stub)\|WORKAROUND(grok-cli-no-persistence-off)" src/` → 2 hits.

---

## Boundary with Coherence Refactoring Effort

This PRD runs in parallel with the broader Coherence Refactoring design at `docs/designs/coherence-refactoring.md`. The two efforts share the engine spawn + post-processing window. Hard rules for THIS PRD:

- **`cleanup_session` hook stays single-purpose.** No status, lifecycle, reconciliation, or `<task-status>` logic is added inside `dispatch` post-spawn. Task lifecycle is the coherence effort's owned layer.
- **Engine field-removal edits leave clean seams.** At `engine.rs:656` and `:2587`, drop only the `cleanup_title_artifact: true` line; do NOT also tidy up adjacent `SpawnOpts`/`RunnerOpts` construction or extract helpers. The coherence Phase 1 will carve `engine.rs` along seams it has already mapped.
- **`runner.rs` stays a peer of the future `TaskLifecycle` module, not a parent.** No status, reconciliation, or `run_tasks` bookkeeping seeps into the runner trait surface.
- **`<task-status>` side-band tag contract and per-task partial-failure tolerance are preserved bit-identically.**

If during implementation a tension surfaces between these rules and a task acceptance criterion, raise it via Open Question rather than resolving unilaterally — both efforts have stakeholders.

---

## Important Rules

- Work on **ONE story per iteration**.
- **Commit frequently** after each passing story.
- **Keep CI green** — never commit failing code (scoped tests for the iteration; full suite at milestones).
- **Read before writing** — always read files first. The PRD §6 line numbers are accurate as of 2026-05-19; verify in ANALYSIS-001 before deletes.
- **Minimal changes** — only implement what's required by the task's acceptance criteria.
- **Reuse `encoded_cwd_dir` at `claude.rs:51`** — do NOT re-implement Claude's dash-encoding.
- **NEVER delete Grok's `prompt_history.jsonl`** — it accumulates across sessions by design.
- **`WORKAROUND(...)` markers must exist** post-Phase 1 so upstream-fix removal is one grep.
- **Sync cleanup only** — no detached threads, no `tokio::spawn`, no async cleanup (learning [2674]).
- **Single bisectable commit for FEAT-007** — the field-removal sweep across 10 files lands together.
- **Dispatch hook is single-purpose** (coherence-refactoring boundary) — no lifecycle/status/reconciliation logic inside the post-spawn cleanup block.
