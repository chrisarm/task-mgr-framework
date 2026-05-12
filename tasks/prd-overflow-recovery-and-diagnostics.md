# PRD: Prompt-Overflow Recovery Escalation + Diagnostics

**Type**: Enhancement
**Priority**: P1 (High — silently blocks Sonnet-default loops on iteration 1)
**Author**: Claude Code
**Created**: 2026-05-04
**Status**: Draft

---

## 1. Overview

### Problem Statement

Two distinct gaps in the loop engine's `PromptTooLong` recovery path:

1. **Recovery gap.** A loop running at `Model: claude-sonnet-4-6, Effort: high` overflows on iteration 1 and is immediately blocked. The recovery code in `src/loop_engine/engine.rs:2044-2113` runs two checks — `downgrade_effort` (no-op: `high` is the floor) and `to_1m_model` (no-op: only Opus has a 1M variant) — then blocks the task with the misleading message `"all recovery exhausted (effort floor + 1M model)"`. Sonnet at `high` effort has zero recovery path today, even though `model::escalate_model` exists and would naturally escalate Sonnet → Opus.

2. **Diagnostics gap.** When overflow happens the user sees one stderr line and the task is blocked. There is no record of *what* the prompt looked like, *how big* it was, or *which section dominated*. The error message also conflates "1M was tried and failed" with "1M was never available for this model tier", sending users hunting in the wrong place.

### Background

`src/loop_engine/engine.rs` runs an autonomous loop that selects tasks, builds prompts via `prompt::build_prompt()` (capped at `TOTAL_PROMPT_BUDGET = 80_000` bytes), and spawns the Claude CLI to execute one iteration of work per task. Crashes are classified by `detection.rs::classify_crash`; `PromptTooLong` is one classification (engine.rs:2044).

The existing recovery seam is `IterationContext.model_overrides: HashMap<String, String>` and `IterationContext.effort_overrides: HashMap<String, &'static str>` (engine.rs:203, 208). When the next iteration runs, `effective_model` (engine.rs:1745-1771) reads from these overrides and they take precedence over the DB-resolved model. Learning #1856 documents this pattern; learning #1861 confirms `IterationContext` is per-slot and not thread-safe.

`prompt::build_prompt()` (prompt.rs:133) returns a `PromptResult` that already includes a `dropped_sections: Vec<String>` field for budget-trim diagnostics. Sections are assembled as named `String` variables (`task_section`, `learnings_section`, `source_section`, etc., prompt.rs:170-280) — capturing per-section byte counts is a parallel addition.

The architect review of the implementation plan flagged ten issues, all incorporated into this PRD: dedicated overflow-recovered marker (BLOCKER), task_id sanitization for filenames (BLOCKER), reordered operations for crash-resilience (MAJOR), prompt-bytes lifetime documentation (MAJOR), atomic JSONL writes (MAJOR), explicit-Opus-skips-rung-2 test case (MAJOR), JSONL growth TODO (MINOR), `Option<String>` formatting in messages (MINOR), `Serialize` derive on event struct (NIT).

---

## 2. Goals

### Primary Goals

- [ ] Add a third recovery rung to the `PromptTooLong` branch: when effort is at floor and model is below Opus tier, escalate model (Sonnet→Opus) instead of blocking the task.
- [ ] Capture the failing prompt to disk (`.task-mgr/overflow-dumps/`) with per-section byte breakdown so users can see what dominated.
- [ ] Append a structured JSONL event per overflow to `.task-mgr/overflow-events.jsonl` for later inspection.
- [ ] Replace the misleading "effort floor + 1M model" message with three distinct, accurate phrasings (one per recovery rung).
- [ ] Annotate the iteration banner when a task is mid-recovery so users immediately see the degraded state.

### Success Metrics

- A task started at Sonnet+high that hits `PromptTooLong` does NOT block; instead the next iteration runs at Opus+high.
- A task started at Sonnet+xhigh that hits `PromptTooLong` four times across iterations walks the full ladder: Sonnet+high → Opus+high → Opus[1M]+high → blocked.
- After any overflow event, `ls .task-mgr/overflow-dumps/` shows a dump file for the affected task and `tail -1 .task-mgr/overflow-events.jsonl` parses as JSON with `recovery.action` matching the action just taken.
- Dumps directory contains at most 3 files per `task_id` (oldest rotated out by mtime).

---

## 2.5. Quality Dimensions

### Correctness Requirements

- The new rung-2 (`escalate_below_opus`) MUST NOT change `effort` — only the model. The `xhigh→high→<floor>` invariant from `model.rs:42-48` ("`high` is the floor for overflow") remains intact.
- The banner annotation MUST gate on a dedicated `ctx.overflow_recovered: HashSet<String>` populated only by the `PromptTooLong` branch — never inferred from `model_overrides`. Other paths may write to `model_overrides` in the future (learning #893: "separate crash escalation from retry escalation"), and inference would create a false-positive trap.
- Dump filename MUST sanitize `task_id` via an allowlist (`[A-Za-z0-9._-]`, others → `-`) before formatting — task IDs come from PRD authors and could in principle contain `/`, `..`, NUL, spaces. Mirrors `worktree.rs::sanitize_branch_name`.
- Order of operations within the `PromptTooLong` arm MUST be: (1) update `ctx`, (2) DB UPDATE, (3) stderr message, (4) write dump (best-effort), (5) append JSONL (best-effort), (6) rotate (best-effort). A kill mid-sequence preserves recovery; observability is best-effort.
- JSONL appends MUST use `OpenOptions::new().append(true).create(true)` and a single `write_all` per event. On Linux, `O_APPEND` writes ≤ `PIPE_BUF` (4096B) are atomic; the single `write_all` keeps lines contiguous within the process.
- `effective_model.as_deref().unwrap_or("(default)")` MUST be used in all new stderr messages — `effective_model` is `Option<String>` (learning #209: `None` on early exits and uncomputed paths).

### Performance Requirements

- Best effort. Overflow recovery happens at most once per iteration, and observability writes are bounded (one ~1KB JSONL line, one ≤80KB dump). No hot path.
- Dump rotation is O(N log N) on the number of dumps for one task (cap = 3 most of the time), so trivially bounded.
- Adding `section_sizes: Vec<(&'static str, usize)>` to `PromptResult` is one push per named section in `build_prompt` — O(sections) ≈ 14 entries, ~336 bytes per `PromptResult`.

### Style Requirements

- Follow existing escalation patterns (learning #851). `escalate_below_opus` returns `&'static str` to match the style of `to_1m_model` and `downgrade_effort` (`model.rs:198-203, 233-239`).
- New module `src/loop_engine/overflow.rs` is the single home for `OverflowEvent`, `dump_prompt`, `append_event_log`, `rotate_dumps_keep_n`, `format_breakdown`, and `sanitize_id_for_filename` — keeps the `PromptTooLong` arm in `engine.rs` slim.
- `OverflowEvent` derives `serde::Serialize` with `#[serde(rename_all = "snake_case")]` so the JSONL schema cannot drift from the struct definition.
- No `.unwrap()` on filesystem ops; use `match` with `eprintln!` warnings on failure (observability is best-effort, not load-bearing).
- Field additions to `PromptResult` and `IterationContext` follow the incremental-addition pattern (learning #522). Update all construction sites including test fixtures (`prompt.rs:847`).

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Task with `model = "claude-opus-4-7"` set in DB (explicit Opus) | `escalate_below_opus` returns `None`; rung 2 must skip | Falls through to rung 3 (`to_1m_model`) on first overflow; JSONL records `recovery.action = "to_1m_model"` |
| Task with `model = "claude-haiku-4-5-20251001"` (Haiku) | `escalate_below_opus(Haiku)` returns Sonnet, not Opus | Rung 2 sets override to Sonnet; subsequent overflow at Sonnet then escalates to Opus on the *next* overflow |
| `effective_model = None` (early-exit path or no model resolved, learning #209) | Format string `"model {m}"` would print `"None"` | Use `.as_deref().unwrap_or("(default)")` matching rung-1 style at engine.rs:2053 |
| Task ID containing `/` or `..` (synthetic / future PRD edge) | Path traversal escape from `.task-mgr/overflow-dumps/` | `sanitize_id_for_filename` allowlists `[A-Za-z0-9._-]`; characters outside become `-` |
| Loop killed between ctx-update (step 1) and DB UPDATE (step 2) | ctx is in-memory; lost on restart anyway | No inconsistency: status is still `in_progress`, next loop run rediscovers the overflow on first attempt |
| Loop killed between DB UPDATE (step 2) and dump write (step 4) | Recovery durable in DB; observability missing for this event | Acceptable — observability is best-effort, recovery is what matters |
| Already at Opus[1M], effort=high, overflow | All four recovery rungs decline | Block task with accurate message: `"no recovery available (already at Opus[1M] with effort=high)"` |
| Two slots overflow on same task simultaneously | Not possible: `next::next()` claims tasks (status=in_progress, run_id=current); same task is in at most one slot per run (learning #1861) | N/A — best-effort rotation is sufficient |
| Dump file write fails (disk full, permission error) | Observability degrades, recovery must not | Log warning to stderr; do NOT propagate the error or fail the iteration |
| `prompt_result.prompt` lifetime in the `PromptTooLong` arm | Required: dump reads from this string | Verified live: `prompt_result` is in scope through the entire arm at engine.rs:2044 (passed to `spawn_claude` at engine.rs:1806) |

---

## 3. User Stories

### US-001: Loop completes Sonnet-default tasks despite overflow

**As a** user running a task-mgr loop with `claude-sonnet-4-6` as the default model
**I want** the loop to automatically escalate to Opus on `PromptTooLong` instead of blocking the task
**So that** my loop makes forward progress without manual intervention

**Acceptance Criteria:**

- [ ] First `PromptTooLong` event on a Sonnet+high task escalates the next iteration's model to Opus while keeping effort at `high`.
- [ ] The next iteration's banner shows the new model and an annotation indicating overflow recovery from Sonnet.
- [ ] If the Opus iteration also overflows, the iteration after that escalates to Opus[1M] (existing rung 3, unchanged behavior).
- [ ] If the Opus[1M] iteration also overflows, the task is blocked with an accurate message naming Opus[1M] + effort=high.

### US-002: User can diagnose what made the prompt too long

**As a** user investigating why a task overflowed
**I want** the failing prompt and a per-section byte breakdown saved to disk
**So that** I can see which section dominated (file context, learnings, base prompt, etc.) without re-running the loop

**Acceptance Criteria:**

- [ ] On every `PromptTooLong` event, a dump file is written to `.task-mgr/overflow-dumps/<sanitized-task-id>-iter<N>-<unix-ts>.txt`.
- [ ] The dump's header lists total assembled bytes plus per-section byte counts (`task`, `base_prompt`, `learnings`, `source`, etc.) and any sections dropped for budget.
- [ ] The dump's body is the assembled prompt verbatim.
- [ ] The dump includes a NOTE explaining that Claude additionally loads CLAUDE.md / skills / agents on top, so an assembled-bytes value well below 200K implicates the auto-load layer (start with `ls .claude/`).
- [ ] At most 3 dumps per `task_id` are retained (oldest rotated out by mtime).

### US-003: User can see overflow history and recovery actions across runs

**As a** user maintaining a long-running loop
**I want** a structured event log of all overflow events with recovery actions
**So that** I can grep / `jq` it to see how often a task overflows, what was tried, and where dumps live

**Acceptance Criteria:**

- [ ] Every `PromptTooLong` event appends one JSON line to `.task-mgr/overflow-events.jsonl`.
- [ ] Each event contains: `ts`, `task_id`, `run_id`, `iteration`, `model`, `effort`, `prompt_bytes`, `sections`, `dropped_sections`, `recovery.action` (one of `downgrade_effort` | `escalate_model` | `to_1m_model` | `blocked`), `recovery.new_model`, `recovery.new_effort`, `dump_path`.
- [ ] `cat .task-mgr/overflow-events.jsonl | jq '.recovery.action'` works without errors.

---

## 4. Functional Requirements

### FR-001: Three-rung recovery in the `PromptTooLong` branch

The recovery branch in `engine.rs:2044-2113` evaluates rungs in this order:

1. `model::downgrade_effort(effort)` — `xhigh → high`. Existing.
2. `model::escalate_below_opus(effective_model.as_deref())` *(new)* — if model tier < Opus, return next-tier model.
3. `model::to_1m_model(effective_model.as_deref())` — Opus → Opus[1M]. Existing.
4. Block task — only when all three return `None`.

**Details:**

- `escalate_below_opus` is a new `pub fn` in `model.rs` returning `Option<&'static str>`:
  ```rust
  pub fn escalate_below_opus(model: Option<&str>) -> Option<&'static str> {
      match model_tier(model) {
          ModelTier::Haiku => Some(SONNET_MODEL),
          ModelTier::Sonnet => Some(OPUS_MODEL),
          _ => None,  // Opus, 1M, Default, None
      }
  }
  ```
- Rung 2 inserts into `ctx.model_overrides[task_id]` exactly like rung 3 does today; effort is NOT touched.
- Rung 2 also inserts into `ctx.overflow_recovered` (HashSet, new) and `ctx.overflow_original_model` (HashMap, new) for the banner annotation.

**Validation:**

- Unit tests on `escalate_below_opus` covering all six tier inputs (Haiku, Sonnet, Opus, 1M Opus, Default, None).
- Integration test (`tests/overflow_recovery.rs`) walks Sonnet+xhigh → Sonnet+high → Opus+high → Opus[1M]+high → blocked across four overflow events on one synthetic task.

### FR-002: Per-section byte breakdown in `PromptResult`

Add `pub section_sizes: Vec<(&'static str, usize)>` to `PromptResult` (prompt.rs:51), parallel to `dropped_sections`. Populate inside `build_prompt` as each named `*_section: String` is finalized.

**Details:**

- Sections to track: `task`, `task_ops`, `completion`, `escalation`, `reorder_instr`, `base_prompt`, `learnings`, `source`, `dependencies`, `synergy`, `siblings`, `steering`, `session_guidance`, `reorder_hint`.
- Static string slices for section names — avoids per-iteration allocations.
- Update all `PromptResult` construction sites including `prompt.rs:847` (test fixture / early-return path).

**Validation:**

- Unit test in `prompt.rs` asserting `section_sizes.iter().map(|(_, n)| n).sum::<usize>() <= TOTAL_PROMPT_BUDGET` for a representative invocation.
- Sum of section bytes plus inter-section delimiters equals `prompt.len()` ± delimiter bytes (within 100 bytes tolerance).

### FR-003: Overflow dump to disk

When `PromptTooLong` is detected, write a dump to `.task-mgr/overflow-dumps/<sanitized-task-id>-iter<N>-<unix-ts>.txt`.

**Details:**

- Sanitization via `overflow::sanitize_id_for_filename(&task_id)` (allowlist `[A-Za-z0-9._-]`, replace others with `-`).
- Source bytes: `prompt_result.prompt` (verified live in scope at engine.rs:2044).
- Header includes: task_id, iteration, model, effort, ISO-8601 timestamp, total bytes, per-section breakdown, dropped sections, the auto-load NOTE.
- Body: the full assembled prompt verbatim, separated from the header by `---\n`.
- Created lazily — directory created on first overflow per run.
- Failure is non-fatal: `eprintln!("warning: overflow dump write failed: {}", e)` and continue.

**Validation:**

- Integration test creates the directory, writes a dump, asserts file exists, parses the header, asserts body matches the source prompt byte-for-byte.
- Sanitization unit tests: input `"FOO/BAR..baz"` → output `"FOO-BAR--baz"`; input `""` → output `"_"` (placeholder for empty); input `"abc-123_X.Y"` → unchanged.

### FR-004: JSONL event log

Append one JSON line per overflow to `.task-mgr/overflow-events.jsonl`.

**Details:**

- `OverflowEvent` struct with `#[derive(Serialize)]` and `#[serde(rename_all = "snake_case")]`.
- Fields: `ts`, `task_id`, `run_id`, `iteration`, `model`, `effort`, `prompt_bytes`, `sections: Vec<(&'static str, usize)>`, `dropped_sections: Vec<String>`, `recovery: RecoveryAction`, `dump_path: String`.
- `RecoveryAction` is a tagged enum with variants `DowngradeEffort { new_effort: String }`, `EscalateModel { new_model: String }`, `To1mModel { new_model: String }`, `Blocked`.
- File opened with `OpenOptions::new().append(true).create(true)`. Single `write_all` of `serde_json::to_vec(&event).unwrap()` followed by a `\n`.
- Failure non-fatal (same `eprintln!` warning pattern as FR-003).

**Validation:**

- Integration test asserts each line is parseable as `OverflowEvent` via `serde_json::from_str`.
- Asserts `recovery.action` matches the rung that fired.

### FR-005: Dump rotation (keep N=3 newest)

After writing a dump, list `<sanitized-task-id>-iter*-*.txt` in `.task-mgr/overflow-dumps/`, sort by mtime descending, and delete entries from index 3 onward.

**Details:**

- Best-effort: failure to list / delete logs a warning and continues.
- Rotation runs after each dump, not on a schedule.
- Per-task scope: rotation only touches files matching the current task's sanitized ID prefix.

**Validation:**

- Integration test triggers ≥4 overflows on one task, asserts directory contains exactly 3 files for that task afterwards.

### FR-006: Iteration banner annotation

In `engine.rs` near line 1793 where the iteration banner is built, if `ctx.overflow_recovered.contains(&task_id)`, append a parenthetical to the model field:

```
═══ Iteration 2/16 ═══ Task: ca83bc7f-TEST-001 ═══ Model: claude-opus-4-7 (overflow recovery from claude-sonnet-4-6) ═══ Effort: high ═══ Elapsed: 0s ═══
```

**Details:**

- `ctx.overflow_original_model: HashMap<String, String>` stores the pre-recovery model captured at first overflow per task. Read here for the annotation text.
- In-memory only; lost on loop restart (same scope as `model_overrides` today).

**Validation:**

- `engine.rs` test module asserts `format_iteration_banner(...)` includes the annotation when `ctx.overflow_recovered` contains the task.
- Negative test: when `ctx.model_overrides` has an entry for the task BUT `ctx.overflow_recovered` does NOT (simulating a future non-overflow writer to `model_overrides`), the banner does NOT include the annotation.

### FR-007: Corrected stderr messages

Replace the misleading "effort floor + 1M model exhausted" string with four distinct phrasings:

- Rung 1: `Prompt is too long for {task} at effort {e} — downgrading effort to {next}` *(unchanged)*
- Rung 2: `Prompt is too long for {task} at effort {e}, model {m} — escalating model to {next} (effort floor reached)` *(new)*
- Rung 3: `Prompt is too long for {task} at effort {e}, model {m} — escalating to 1M-context variant {m1m} (already at Opus)` *(reworded)*
- Blocked: `Prompt is too long for {task} at effort {e}, model {m} — no recovery available (already at Opus[1M] with effort=high)` *(reworded, accurate)*

`{m}` resolves via `effective_model.as_deref().unwrap_or("(default)")`.

**Validation:**

- Integration test captures stderr per rung and asserts the expected substring is present.

---

## 5. Non-Goals (Out of Scope)

- **Persisting `model_overrides` / `effort_overrides` / `overflow_recovered` to the DB.** Loop restart loses recovery state today; this PRD does not change that. Reason: requires migration; orthogonal to the recovery gap. Tracked separately.
- **`task-mgr overflow <task-id>` CLI subcommand** for inspecting past overflow events. Reason: deferred until users want a curated UX over `jq` on the JSONL.
- **Heuristic suggestions in the error message** ("files dominate, consider splitting"). Reason: requires telemetry over multiple incidents to be useful; section breakdown in the dump is the foundation.
- **Exposing `section_sizes` in non-overflow code paths.** Reason: only consumed by the overflow dump; no other current need.
- **Rotation / capping of `overflow-events.jsonl`.** Reason: lines are tiny (~1KB); growth is bounded by overflow frequency. TODO comment noted in the source for a future `--rotate` flag.
- **Adding a `SONNET_MODEL_1M` constant** to give Sonnet its own 1M variant. Reason: only Opus has a 1M context variant currently exposed by Anthropic; if Sonnet 4.6 gains one later, that's a separate model.rs change.

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/model.rs` — Add `escalate_below_opus(model)` helper.
- `src/loop_engine/engine.rs` — Three-rung recovery in `PromptTooLong` branch (lines 2044-2113); banner annotation at iteration header (line 1793); add `overflow_recovered: HashSet<String>` and `overflow_original_model: HashMap<String, String>` to `IterationContext` (line 203 area).
- `src/loop_engine/prompt.rs` — Add `pub section_sizes: Vec<(&'static str, usize)>` to `PromptResult`; populate inline as each section is assembled. Update all construction sites (including prompt.rs:847).
- `src/loop_engine/overflow.rs` *(new)* — Module hosting `OverflowEvent`, `RecoveryAction`, `dump_prompt`, `append_event_log`, `rotate_dumps_keep_n`, `format_breakdown`, `sanitize_id_for_filename`.
- `src/loop_engine/mod.rs` — Register the new module.
- `tests/overflow_recovery.rs` *(new)* — Integration test for the four-rung ladder + explicit-Opus skip-rung-2 case + sanitization.

### Dependencies

- `serde` and `serde_json` (already in `Cargo.toml`) for `OverflowEvent` serialization.
- `chrono` (already in `Cargo.toml`) for ISO-8601 timestamps in the dump header.
- No new external crates. No DB migration.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| **A. Three-rung escalation as designed (Sonnet→Opus, then Opus→Opus[1M])** | Smallest behavioral change. Reuses `model_overrides` seam exactly as rung 3 already does (learning #1856). Preserves `high` floor invariant. Single unit-test addition for `escalate_below_opus`. | Recovery is in-memory only — loop restart loses state. Already true for rung 3. | **Preferred** |
| B. Extend the effort ladder (`high → medium → low`) | No model-tier change needed; same model gets retried at lower effort. | Reverses the deliberate "high is the floor" decision (`model.rs:42-48`). `max` was retired for the same overflow reason; adding a `medium` rung would re-introduce the failure mode at a different tier. | Rejected |
| C. Block immediately + emit hint to user | Minimal code change. | User-hostile: requires manual restart with a different model flag for every overflow. Defeats the loop's autonomy. | Rejected |

**Selected Approach**: A. Reuses the existing seam, adds one helper function, four lines of branching in the `PromptTooLong` arm.

**Phase 2 Foundation Check**: The diagnostics layer (dumps + JSONL + `section_sizes` field) is the foundation for any future overflow-tooling: `task-mgr overflow` subcommand, heuristic suggestions, fleet-wide telemetry. Building it now (estimated 1-2 days) avoids three follow-ons each rebuilding the same plumbing — clear 1:>3 effort ratio. Section sizes specifically unlock targeted truncation strategies later (drop the heaviest trimmable section first) without another `PromptResult` schema change.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Banner annotation false-positives if a future code path writes to `model_overrides` for non-overflow reasons | Med (user confusion, lying telemetry) | Med (crash escalation lives in the same area; learning #893 calls out keeping the two paths separate) | Dedicated `overflow_recovered: HashSet<String>` marker, populated only by the `PromptTooLong` arm. Banner reads from this set, never inferred from `model_overrides`. Negative test asserts the gating. |
| Path traversal via unsanitized `task_id` in dump filename (synthetic / future PRD edge) | High (write outside `.task-mgr/`) | Low (current PRDs use clean IDs) | `sanitize_id_for_filename` allowlists `[A-Za-z0-9._-]`; mirrors `worktree.rs::sanitize_branch_name` precedent. Sanitization unit-tested. |
| Loop killed mid-recovery leaves task in inconsistent state | High (task wedged `in_progress`, never retried) | Low (process kill is rare during a 1ms DB UPDATE) | Order operations: `ctx` update → DB UPDATE → stderr → dump → JSONL → rotate. Worst-case kill between dump and JSONL leaves recovery durable but observability incomplete — acceptable. |

### Security Considerations

- Filename sanitization (`sanitize_id_for_filename`) prevents path-traversal escape from `.task-mgr/overflow-dumps/`.
- JSONL writes use `OpenOptions::append(true)` not truncate — never overwrites prior history.
- No secrets in dumps: prompts may contain task descriptions and file contents from the project, which were already sent to Claude. No new exposure surface.
- Dumps are user-readable only (`0644` is fine; no special permissions). Same trust boundary as `.task-mgr/tasks.db`.

### Public Contracts

#### New Interfaces

| Module/Function | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `model::escalate_below_opus` | `fn(model: Option<&str>) -> Option<&'static str>` | `Some(SONNET_MODEL)` for Haiku, `Some(OPUS_MODEL)` for Sonnet | `None` for Opus / 1M / Default / None | None |
| `overflow::sanitize_id_for_filename` | `fn(id: &str) -> String` | Allowlisted ASCII-safe filename component (empty input → `"_"`) | (infallible) | None |
| `overflow::dump_prompt` | `fn(dir: &Path, task_id: &str, iter: u32, header: &DumpHeader, prompt: &str) -> io::Result<PathBuf>` | `Ok(path_written)` | `Err(io::Error)` (caller logs warning) | Creates dir if missing; writes file |
| `overflow::append_event_log` | `fn(dir: &Path, event: &OverflowEvent) -> io::Result<()>` | `Ok(())` | `Err(io::Error)` (caller logs warning) | Appends one JSON line to `.task-mgr/overflow-events.jsonl` |
| `overflow::rotate_dumps_keep_n` | `fn(dir: &Path, sanitized_task_id: &str, keep: usize) -> io::Result<()>` | `Ok(())` | `Err(io::Error)` (caller logs warning) | Deletes oldest dumps for a task beyond `keep` |
| `OverflowEvent` (struct, derive `Serialize`, `Debug`) | `pub struct { ts, task_id, run_id, iteration, model, effort, prompt_bytes, sections, dropped_sections, recovery, dump_path }` | n/a | n/a | n/a |
| `RecoveryAction` (enum, derive `Serialize`, `Debug`, `#[serde(tag = "action", rename_all = "snake_case")]`) | `DowngradeEffort { new_effort } \| EscalateModel { new_model } \| To1mModel { new_model } \| Blocked` | n/a | n/a | n/a |

#### Modified Interfaces

| Item | Current | Proposed | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `PromptResult` | (no `section_sizes` field) | `pub section_sizes: Vec<(&'static str, usize)>` added | No (struct extension; only constructed in-crate) | Update construction sites: `prompt.rs:389` (main path), `prompt.rs:847` (early-return path), test fixtures. Default to `Vec::new()` where empty is appropriate. |
| `IterationContext` | (no `overflow_recovered` / `overflow_original_model` fields) | Two new fields added: `pub overflow_recovered: HashSet<String>`, `pub overflow_original_model: HashMap<String, String>` | No (struct extension; constructed only in `IterationContext::new()`) | Update `IterationContext::new()` to initialize both as empty. |

### Data Flow Contracts

The new path crosses `engine.rs` ↔ `prompt.rs` ↔ `overflow.rs`. Key types at each level:

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| Iteration body → prompt assembly → section sizes | `PromptResult` (struct with named field) → `Vec<(&'static str, usize)>` (positional tuple, NOT a map) | `prompt_result.section_sizes.iter().find(\|(name, _)\| *name == "learnings").map(\|(_, n)\| *n).unwrap_or(0)` — linear find, not hashmap lookup, since order matters for the dump header. |
| Recovery decision → ctx mutation → next iteration | `ctx.model_overrides: HashMap<String, String>` (key: task_id String, value: model String) | `ctx.model_overrides.insert(task_id.clone(), OPUS_MODEL.to_string());` then on next iteration: `ctx.model_overrides.get(&task_id).cloned()` (returns `Option<String>`). |
| Recovery decision → banner gating → rendering | `ctx.overflow_recovered: HashSet<String>` (key: task_id) + `ctx.overflow_original_model: HashMap<String, String>` (key: task_id, value: original model) | `if ctx.overflow_recovered.contains(&task_id) { ctx.overflow_original_model.get(&task_id).map(\|orig\| format!(" (overflow recovery from {})", orig)).unwrap_or_default() } else { String::new() }` |
| Overflow event → JSONL append | `OverflowEvent` (typed struct with serde) → `.jsonl` file (line-delimited JSON; string keys after serialization) | Producer side: `let line = serde_json::to_vec(&event)?; line.push(b'\n'); file.write_all(&line)?;` Consumer side (out of scope for this PRD): `for line in BufReader::new(File::open(".task-mgr/overflow-events.jsonl")?).lines() { let event: OverflowEvent = serde_json::from_str(&line?)?; ... }` |

The notable type transition is `section_sizes: Vec<(&'static str, usize)>` (positional, ordered) on the producer side becoming a `[[name, n], ...]` JSON array on the JSONL consumer side. Order is preserved across the boundary; it carries diagnostic meaning (sections appear in assembly order).

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `src/loop_engine/engine.rs:2044-2113` | The `PromptTooLong` arm itself | OK — this is the change site | Comprehensive test coverage in `tests/overflow_recovery.rs` |
| `src/loop_engine/engine.rs:1761` | Reads `ctx.model_overrides.get(&task_id)` | OK — rung 2 inserts into the same map; reader is unchanged | Existing rung-3 already exercises this seam |
| `src/loop_engine/engine.rs:1778` | Reads `ctx.effort_overrides.get(&task_id)` | OK — rung 1 inserts into this map; rungs 2/3 do not | Existing rung-1 already exercises this seam |
| `src/loop_engine/engine.rs:1793` | Iteration banner formatting | NEEDS UPDATE — append annotation when recovery is active | Negative test asserts annotation does NOT appear when only `model_overrides` (not `overflow_recovered`) has the task |
| `src/loop_engine/engine.rs:4276` (`escalate_task_model_if_needed`) | Crash escalation writes to DB `tasks.model`, NOT `model_overrides` | OK — separate channel | Documented separation aligns with learning #893 |
| `src/loop_engine/engine.rs:4225` (`check_crash_escalation`) | Returns `Option<String>` flowing into `effective_model`, NOT `model_overrides` | OK — separate channel | Per architect review and learning #893 |
| `src/loop_engine/prompt.rs:51` (`PromptResult` definition) | Struct extended with new field | OK — additive | All construction sites updated |
| `src/loop_engine/prompt.rs:847` (test/early-return PromptResult) | Initialize new field as `Vec::new()` | OK — additive | Test fixtures updated alongside |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| `ctx.model_overrides.insert(...)` from `PromptTooLong` arm rung 3 (`to_1m_model`) | Overflow recovery — Opus → Opus[1M] | Inserts Opus[1M]; iteration N+1 uses it | Unchanged. Plus: now also inserts task_id into `ctx.overflow_recovered` and original model into `ctx.overflow_original_model`. |
| `ctx.model_overrides.insert(...)` from `PromptTooLong` arm rung 2 *(new)* | Overflow recovery — Sonnet → Opus or Haiku → Sonnet | n/a | Inserts escalated model; effort UNCHANGED. Inserts task_id into `ctx.overflow_recovered`. |
| Crash-tracker model escalation (`escalate_task_model_if_needed`, engine.rs:4276) | Repeated crashes on the same task across iterations — DB-level escalation | Writes to `tasks.model` column in DB, NOT `model_overrides` | Unchanged. Stays in its own lane (learning #893). |
| `check_crash_escalation` (engine.rs:4225) flowing into `effective_model` | Same-task consecutive-crash escalation per iteration | Returns `Option<String>` checked at engine.rs:1761 BEFORE `model_overrides` | Unchanged. The `model_overrides` check at engine.rs:1761 still wins (overflow recovery is higher priority, by design). |

### Inversion Checklist

- [x] All callers identified and checked? — `model_overrides` and `effort_overrides` consumers traced through engine.rs:1761/1778; banner builder at engine.rs:1793 identified as the only annotation-aware reader.
- [x] Routing/branching decisions that depend on output reviewed? — Three-rung order verified: rung 1 / rung 2 / rung 3 all set their own override types and don't shadow each other; block-task arm only fires on triple-None.
- [x] Tests that validate current behavior identified? — Existing tests for `downgrade_effort` (model.rs:941+), `to_1m_model` (model.rs:1097+) remain valid; new tests cover `escalate_below_opus` and the integration ladder.
- [x] Different semantic contexts for same code discovered and documented? — Semantic Distinctions table above separates four overlapping channels: rung-3, rung-2, crash-tracker DB escalation, and per-iteration crash escalation.

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `CLAUDE.md` (project root) | Update | Add a section under "Loop CLI Cheat Sheet" or a new "Overflow Recovery" subsection naming the three-rung ladder, the dump path, and the JSONL event log path. |
| `src/loop_engine/model.rs` (rustdoc on `escalate_below_opus`) | Create | Document the helper alongside `downgrade_effort` and `to_1m_model`, including the "high effort floor preserved" invariant. |
| `src/loop_engine/overflow.rs` (module-level rustdoc) | Create | Module description: dump format, JSONL schema, sanitization rules, rotation policy. |
| `src/loop_engine/engine.rs` (rustdoc on the `PromptTooLong` arm) | Update | Document the four-state recovery ladder and the corrected message phrasings. |
| `docs/system-design-overview.md` | N/A | No architectural change — same recovery seam, same `IterationContext` shape with two additive fields. |

---

## 7. Open Questions

- [x] ~~Should `OverflowEvent.run_id` use the loop's `run_id` field directly, or a separate "overflow_session_id"?~~ **RESOLVED 2026-05-04**: reuse loop's `run_id`. Lets users correlate overflow events with the broader run via `jq 'select(.run_id == "...")'`. `ts` already provides temporal ordering.
- [x] ~~When `effective_model` is `None` AND overflow happens, should the dump still write?~~ **RESOLVED 2026-05-04**: yes, always write. Header shows `model: (default)` consistently with the new stderr messages. The dump bytes are the most-needed evidence; skipping would hide it.
- [x] ~~Should the dump include the resolved `effort` value or the sent value?~~ **RESOLVED 2026-05-04**: sent value. The dump answers "what overflowed?" The JSONL `recovery.new_effort` separately answers "what's tried next?" Each artifact has one purpose.
- [x] **RESOLVED 2026-05-04 — Override persistence**: `model_overrides[task_id]` (and the new `overflow_recovered`/`overflow_original_model` entries) persist for the loop run's lifetime, matching existing rung-3 semantics. Once a task escalates, it stays escalated. Avoids re-overflow churn at the cost of permanently inflating that task's tier for the rest of the run.
- [x] **RESOLVED 2026-05-04 — `overflow_original_model` capture**: only on the first overflow per task. Implementation MUST use `ctx.overflow_original_model.entry(task_id.clone()).or_insert(model.clone())` — banner annotation stays stable as `(overflow recovery from claude-sonnet-4-6)` even after the task walks Sonnet → Opus → Opus[1M].
- [x] **RESOLVED 2026-05-04 — Block-task observability**: rung 4 (block) writes the dump AND appends the JSONL event. `recovery.action == "blocked"`. A blocked task's dump is the highest-value diagnostic.
- [x] **RESOLVED 2026-05-04 — Test simulation of `PromptTooLong`**: tests inject a synthetic `IterationOutcome::Crash(CrashType::PromptTooLong)` directly into a recovery-branch helper (extracted from the `PromptTooLong` arm in `engine.rs`). Bypasses Claude spawn. Implementation must keep the recovery logic factored into a testable function — likely `pub(crate) fn handle_prompt_too_long(ctx, conn, task_id, effort, effective_model, prompt_result, iteration, run_id) -> RecoveryAction` — so the integration test can call it directly without orchestrating an entire iteration.

---

## Appendix

### Related Documents

- Plan file: `/home/chris/.claude/plans/dreamy-plotting-hearth.md`
- Architect review: incorporated into this PRD's Risks, Quality Dimensions, and Edge Cases.
- Prior PRD (similar shape): `tasks/prd-recall-scores-and-supersession.md`

### Relevant Learnings

- **#1856** — Per-task model escalation after effort exhaustion on `PromptTooLong`. Documents the existing rung-3 pattern; this PRD extends it.
- **#1861** — `IterationContext` is per-slot and not thread-safe. Justifies the "no parallel-slot rotation contention" simplification.
- **#893** — Separate crash escalation from retry escalation. Justifies the dedicated `overflow_recovered` HashSet.
- **#851** — Follow existing escalation patterns for consistency. Drives the `escalate_below_opus` shape (matches `to_1m_model` / `downgrade_effort`).
- **#854** — FEAT-004 model escalation implemented in single pass. Precedent for adding a model-escalation primitive without scope creep.
- **#522** — Incremental field addition to `IterationResult`. Pattern precedent for adding `section_sizes` to `PromptResult`.
- **#209** — `IterationResult.effective_model` is `None` for early exits. Drives the `unwrap_or("(default)")` choice in the new messages.
- **#165** — `resolve_task_model` multi-level fallback chain. Confirms the model-resolution order; recovery overrides sit above this chain.

### Glossary

- **Effort floor**: `high` is the lowest effort the system will downgrade to on `PromptTooLong`. Below `high` is treated as "no effort downgrade available" because `max` was retired for the same overflow reason and lower tiers were never observed to help.
- **Model tier**: `Default < Haiku < Sonnet < Opus`. Defined in `model.rs::ModelTier`. The `[1m]` 1M-context variant of Opus is also `ModelTier::Opus`.
- **Recovery rung**: One step in the four-state `PromptTooLong` recovery ladder. Rungs 1–3 take action; rung 4 blocks the task.
- **Dump**: A `.txt` file under `.task-mgr/overflow-dumps/` containing the assembled prompt that overflowed plus a header with byte counts and per-section breakdown.
- **Auto-load layer**: Content Claude implicitly loads on top of the explicit prompt — `CLAUDE.md` files, skills, agents, tool descriptions. Not visible to `task-mgr`; the dump's NOTE flags this as the likely culprit when assembled bytes are well below the model's window.
