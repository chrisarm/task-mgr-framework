# PRD: Grok Fallback Runner for task-mgr Loop

**Type**: Feature (with Phase-1 Refactor sub-component)
**Priority**: P2 (Medium) — addresses dead-end failure modes; default disabled
**Author**: Claude Code
**Created**: 2026-05-17
**Status**: Draft (revised after architect review)

---

## 1. Overview

### Problem Statement

The task-mgr loop currently dead-ends when the Claude CLI fails on a task in
two scenarios the existing recovery system cannot escape:

1. **`PromptTooLong`**: after the 4-rung overflow ladder
   (`downgrade_effort → escalate_below_opus → to_1m_model → blocked`) has
   reached Opus[1M] at high effort. Task is marked `blocked` with no
   further options.
2. **`RuntimeError`** (generic unknown crash): after consecutive-failure
   model escalation has reached the Opus ceiling. Task continues to retry
   on Opus, accumulating cost without progress, until human intervention
   or `auto_block_task` fires at `max_retries`.

Grok 4 (256K context) and Grok 4 Fast (~2M context) are good at unsticking
both cases — different reasoner, materially different context budget. The
new xAI `grok` CLI is near flag-compatible with `claude` and auto-loads
`CLAUDE.md` + project skills natively (confirmed via `grok inspect`).

### Background

A tag-emission spike (this session, 2026-05-17) confirmed that `grok -p`
with `--permission-mode plan` emits all required loop control tags on first
try without any system-prompt override:

- `<promise>COMPLETE</promise>` ✅
- `<promise>BLOCKED</promise>` ✅
- `<task-status>ID:done</task-status>` ✅
- `<reorder>TASK-ID</reorder>` ✅

The loop's iteration pipeline parses these tags in
`src/loop_engine/detection.rs` and `src/loop_engine/iteration_pipeline.rs`.
Because grok loads the same `CLAUDE.md` + `.claude/skills/` content that
documents this protocol, the tag contract carries over without
modification.

Relevant prior learnings consulted:
- #2031 (4-rung overflow ladder contract)
- #2852 (wave + sequential share `handle_prompt_too_long`)
- #1856 (per-task model escalation via IterationContext overrides)
- #1989 / #2699 (existing trait-based `ClaudeMergeResolver` as prior art)
- #656 (non-loop spawn_claude callers use `PermissionMode::Scoped`)

### Intended outcome

When Claude fails terminally on a task AND
`.task-mgr/config.json` has `fallbackRunner.enabled: true`, the loop
automatically retries the same task on Grok exactly once, preserving all
loop semantics (tag protocol, working directory, file edits, permission
scoping). Disabled by default; opt-in via project config.

---

## 2. Goals

### Primary Goals

- [ ] `PromptTooLong` overflow ladder gains a 5th rung
      (`FallbackToProvider`) that promotes the task to Grok before the
      terminal `Blocked` rung.
- [ ] `RuntimeError` consecutive-failure escalation gains a fallback hook
      that promotes the task to Grok after the Opus ceiling is reached
      AND `tasks.consecutive_failures >= cfg.runtime_error_threshold`
      (default 2).
- [ ] All `spawn_claude` callers continue to work unchanged; the wrapper
      is preserved at its current signature. Phase 1 introduces a runner
      trait with byte-for-byte behavior parity (no observable changes).
- [ ] Default behavior is unchanged when `fallbackRunner` is absent or
      `enabled: false` in project config.
- [ ] Operator intent (explicit `model:` edit in PRD JSON, mid-loop)
      reliably clears any prior auto-recovery override on the next
      iteration.

### Success Metrics

- **Recovery rate**: synthetic regression test (overflow forced via
  artificially bloated prompt) recovers via Grok rather than terminating
  in `Blocked`. Logged in `overflow-events.jsonl` with
  `recovery.action == "fallback_to_provider"`.
- **Zero regression**: existing `cargo test --lib loop_engine` continues
  to pass. The 4-rung ladder behavior is byte-identical when
  `fallbackRunner` is unset.
- **Idempotency**: a task already running on Grok that overflows or
  RuntimeError-crashes proceeds directly to `Blocked` / `auto_block`
  (no infinite Grok-to-Grok promotion).
- **Operator escape valve**: a test asserts that editing a task's
  explicit `model:` field after a fallback fire causes the next
  iteration to honor the new model and clear the override (verified by
  watching `runner_overrides`/`model_overrides` get dropped).

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Single source of truth for the effective runner**: at every spawn
  site, compute `effective_runner` ONCE using this exact formula:

  ```rust
  let effective_runner = ctx.runner_overrides
      .get(&task_id)
      .copied()
      .unwrap_or_else(|| match provider_for_model(effective_model.as_deref()) {
          Provider::Grok => RunnerKind::Grok,
          Provider::Claude => RunnerKind::Claude,
      });
  ```

  Pass this single value to `spawn` AND to any downstream idempotency
  check. Do not re-derive provider routing in multiple places — it
  invites drift under refactor.

- **`effective_model` passed to `handle_prompt_too_long` MUST be the
  post-override value** (the same string passed to `spawn_claude` /
  `dispatch`). Today, `engine.rs:2532` passes
  `effective_model.as_deref()` — the post-override value. The PRD pins
  this contract: any refactor that hands `prompt_result.resolved_model`
  (pre-override) to the overflow handler would silently break the
  idempotency guard.

- **Override-clearing on explicit task-model change** (policy: explicit
  edit wins): every iteration, BEFORE selecting the runner, compare
  `prompt_result.resolved_model` (which already includes `task_model`
  precedence via `resolve_task_model`) against
  `ctx.overflow_original_model.get(&task_id)` (captured at first
  overflow). If they differ AND the difference came from an explicit
  `task_model` (not from a config-default change), drop
  `runner_overrides[task_id]` AND `model_overrides[task_id]` AND
  `effort_overrides[task_id]` AND `overflow_original_model[task_id]`
  AND `overflow_recovered`-remove(task_id). The next iteration then runs
  with the operator's new explicit choice as if no recovery had ever
  happened.

  Concrete detection: store `overflow_original_task_model:
  HashMap<String, Option<String>>` capturing the value of
  `task_row.model` (the DB column, NOT the resolved model) at first
  overflow. On each iteration, re-read `task_row.model`; if it differs
  from the stored value, treat that as "operator changed their mind"
  and clear all overrides for that task. Default `None` → operator
  hadn't pinned anything; default-to-recovery decisions still apply
  unless the operator pins a value for the first time.

- **Order of operations** in `handle_prompt_too_long` (per learning
  #2031) must remain: ctx update → DB UPDATE → stderr → dump → JSONL →
  rotate. The new rung inserts only at the action-selection step (step
  1); the durability ordering is unchanged.

- **Wave + sequential parity for PromptTooLong**: per learning #2852,
  both paths share `handle_prompt_too_long`. The new overflow rung
  works in both call sites
  (`src/loop_engine/engine.rs:561` wave, `engine.rs:2285` sequential)
  without path-specific branches.

- **Wave-mode wiring for RuntimeError fallback**: `run_slot_iteration`
  explicitly does NOT run crash-escalation (`engine.rs:481`
  "No crash-escalation, reorder, stale, or rate-limit-wait logic").
  Therefore the RuntimeError fallback hook MUST live in the post-wave
  aggregation step on the main thread (after `process_slot_result` runs
  the shared `iteration_pipeline`), not inside the slot worker. This
  preserves the slot-worker invariant that slot threads carry no
  shared-state mutation.

- **Provider isolation**: `escalate_model`, `escalate_below_opus`,
  `to_1m_model` are Claude-tier-ladder functions. They must return
  `None` on non-Claude inputs (e.g., `grok-4-fast`) by checking
  `provider_for_model(input) != Provider::Claude` at the top. Cross-
  provider transitions happen ONLY via the explicit
  `FallbackToProvider` rung.

- **Idempotency**: the single computed `effective_runner` value drives
  the guard. If `effective_runner == RunnerKind::Grok`, the fallback
  rung MUST be skipped in both `handle_prompt_too_long` and the
  RuntimeError hook, and the task MUST fall through to `Blocked`
  (overflow) or continue normal failure accounting (RuntimeError,
  ending in `auto_block_task` once `max_retries` is reached). No
  infinite loops.

- **Persistence semantics (restart behavior)**: per design decision,
  `runner_overrides`, `model_overrides`, `effort_overrides`,
  `overflow_recovered`, `overflow_original_model`, and
  `overflow_original_task_model` are all in-memory on `IterationContext`
  — matching the existing pattern. **A loop restart clears all override
  state.** A task that overflowed onto Grok will re-walk the Claude
  ladder from rung 1 on the first iteration after restart, then re-
  fallback if it overflows again — at most one wasted Claude iteration
  per restart per affected task. This is the intended v1 contract.

- **`tasks.model` DB column interaction**: `escalate_task_model_if_needed`
  (`engine.rs:4599-4602`) writes the escalated Claude model into the
  `tasks.model` DB column. If the RuntimeError fallback hook fires from
  this same path, it MUST also `UPDATE tasks SET model = ?
  WHERE id = ?1` with the configured Grok model — otherwise on the next
  iteration `resolve_task_model` reads `task_model = Some(OPUS_MODEL)`
  from the row (highest precedence) and the in-memory runner override
  is silently shadowed by `effective_model` resolving back to Opus.
  The overflow rung MUST do the same DB write when it sets the runner
  override.

- **Grok authentication failure short-circuit**: `GrokRunner` MUST sniff
  for well-known auth-failure stderr strings (`"not authenticated"`,
  `"please run grok login"`, `"grok login required"`, plus a fast-fail
  heuristic: non-zero exit within 3 seconds of spawn with one of these
  strings present). On match, return a distinct
  `TaskMgrError::GrokAuthFailure` variant; the overflow + RuntimeError
  handlers treat this error as "do NOT promote and do NOT count toward
  consecutive_failures", and emit a one-line stderr hint pointing the
  operator at `grok login`. This prevents an auth lapse from cascading
  into `auto_block_task` with a misleading "max retries exceeded"
  reason.

- **Config absence safety**: missing `fallbackRunner` key in JSON
  resolves to `None`. `None` resolves to "fallback disabled" — never to
  enabled with default values. Explicit
  `"fallbackRunner": null` also resolves to `None`.

- **`GrokRunner` binary resolution is config-independent**: `GrokRunner`
  resolves its binary as `$GROK_BINARY` → `fallbackRunner.cli_binary`
  (if config present) → `"grok"` (PATH lookup). An explicit
  `model: grok-4` task with NO `fallbackRunner` config still routes
  through `GrokRunner` and finds the binary on PATH. The startup binary
  existence check (FR-006) fires ONLY when `fallbackRunner.enabled ==
  true`; explicit task-model routing without config skips the startup
  check (the task author opted in directly).

### Performance Requirements

- Runner dispatch is on the hot path (every iteration). Static dispatch
  via `match` on `RunnerKind` (enum) — NOT `Box<dyn LlmRunner>`. The
  trait exists for code organization and testability, not for runtime
  polymorphism. Two known runners do not justify allocation per call.
- Short-circuit grok spawn when binary is missing: probe `which grok`
  once at runner initialization and fail config validation before the
  loop begins, not on the first fallback. Per learning #1992, prefer
  short-circuit before expensive subprocess.

### Style Requirements

- Follow existing codebase patterns. Specifically:
  - Per-task overrides on `IterationContext` mirror
    `model_overrides`/`effort_overrides` exactly (same `HashMap` shape,
    same insertion sites, same read sites).
  - `RecoveryAction` extension via new enum variant — do not introduce
    a sibling enum. Serde tagging contract in
    `src/loop_engine/overflow.rs:30` is preserved.
  - `ProjectConfig` extension via optional struct field (not boolean
    flag + sibling fields). `FallbackRunnerConfig` is its own struct.
  - No new `.unwrap()` on config paths; absent fields must resolve to
    sensible defaults via `Option`.
- Test seam: `GROK_BINARY` env var mirrors the existing `CLAUDE_BINARY`
  pattern (`src/loop_engine/claude.rs:290`). Test mutex generalizes
  from `CLAUDE_BINARY_MUTEX` to a shared `RUNNER_BINARY_MUTEX`.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|---|---|---|
| `fallbackRunner` key absent from `.task-mgr/config.json` | Most existing projects won't have it; must not regress | Resolves to `None`; loop behavior is byte-identical to today |
| `"fallbackRunner": null` (explicit null) | Some users serialize "disabled" as `null` | Same as absent: `None` |
| `fallbackRunner.enabled: true` but `grok` binary not on PATH | Misconfiguration is silent today | Fail loud at loop startup (`task-mgr loop start` exits with config error citing missing binary), NOT at first fallback fire |
| Task already on Grok overflows again | Cross-provider promotion already used its escape hatch | Skip rung 4 via `effective_runner == Grok` guard; fall through to `Blocked`; emit JSONL event with `recovery.action == "blocked"` and `runner: "grok"` |
| Task explicitly set `model: "grok-4-fast"` in PRD JSON | User opted in directly, not via failure-driven promotion | `provider_for_model` routes to `GrokRunner` from iteration 1; no fallback policy involvement; no startup binary check fires |
| Operator edits explicit `model: claude-haiku-4-5` AFTER fallback promoted to Grok | Operator escape valve must work | Next iteration: `task_row.model` differs from `overflow_original_task_model[task_id]` → drop all overrides → resolve fresh with new explicit model |
| Operator's explicit model change is identical to the override (no-op edit) | Detection must compare values, not timestamps | No state cleared; override remains in place |
| RuntimeError threshold reached but task is at Sonnet (not Opus) | Escalation ladder ascends Sonnet → Opus first | Fallback hook only fires AFTER `escalate_model` returns Opus AND `tasks.consecutive_failures >= threshold` |
| `tasks.consecutive_failures` resets via `task-mgr complete` mid-loop | Operator manually resolves a CLARIFY; counter zeroed | Counter zeroing is correct; fallback hook will not fire until the threshold re-accumulates (the task has effectively progressed) |
| Grok itself produces no `<promise>` tag (rare regression in grok versions) | Loop would mis-classify as runtime error | Same behavior as Claude doing the same — detection layer's existing "no promise → check exit code" path handles it. No special case for grok |
| Grok auth lapsed (`grok login` token expired) | Today's behavior: cascade into `auto_block_task` with misleading reason | `GrokRunner` sniffs auth-failure stderr → returns `TaskMgrError::GrokAuthFailure` → handlers skip promotion AND skip counting toward `consecutive_failures`; one-line stderr hint about `grok login` |
| Wave mode: two slots on different runners merge back | `git merge --no-edit` doesn't care about runner | Existing slot merge-back logic is runner-agnostic; no work required |
| `grok` binary version skew (older grok lacks `--permission-mode dontAsk`) | Flag mapping assumes recent grok | Document minimum grok version in config schema docs; runtime check is for binary existence only, not flag-surface parsing (parsing `--help` is brittle — drop it from the requirements per architect review) |
| Concurrent test runs setting `CLAUDE_BINARY` and `GROK_BINARY` | Test mutex pattern must serialize both | Shared `RUNNER_BINARY_MUTEX` covers both env vars |
| Per-provider session-artifact cleanup | Grok writes a session **directory**; Claude writes a single `<uuid>.jsonl` stub | Superseded by the runner-trait-hygiene PRD: cleanup is a trait method (`LlmRunner::cleanup_session`) that `dispatch` calls unconditionally post-spawn; the opt-in `cleanup_title_artifact: bool` field on `RunnerOpts` was removed. See `src/loop_engine/CLAUDE.md` § "Session artifact cleanup" and `runner.rs::LlmRunner::cleanup_session` rustdoc. |
| Model string contains both `claude` and `grok` substrings (e.g. hypothetical `claude-via-grok-proxy`) | Substring match would mis-route | `provider_for_model` does token-equality, not substring: lowercase + split on `-` + check `tokens.contains(&"grok")`. `groq-llama-70b` (Groq Inc., a real vendor) → tokens are `["groq", "llama", "70b"]`, none equals `"grok"` → routes to Claude (correct: groq is not xAI) |
| Task `model: "groq-llama-3"` (Groq Inc., not Grok) | Substring `.contains("grok")` would falsely route to xAI's grok | Token-equality check correctly classifies as `Provider::Claude` (default), and `model_tier` returns `Default` so the loop falls back to its default model resolution path |
| `escalate_task_model_if_needed` persists Opus to `tasks.model`, then RuntimeError hook fires | DB column drives `resolve_task_model` on next iteration; without a corresponding UPDATE, the runner override is silently shadowed | Fallback hook UPDATEs `tasks.model = '<grok-model>'` in the same transaction as setting `runner_overrides` |
| Overflow rung-4 fires; same task is then explicitly overridden by a `task-mgr` operator action mid-iteration | Race between in-memory override and DB write | Acceptable race: the in-memory override applies for the current iteration; DB UPDATE persists for restart-resilience; operator action takes effect after the current iteration completes (same as any mid-iteration DB edit) |
| Banner on a Grok task that overflows and gets blocked | Cosmetic: banner says `(overflow recovery from <claude-model>)` because `overflow_original_model` was captured at the first (Claude) overflow | Acceptable for v1; documented in `src/loop_engine/CLAUDE.md`. The JSONL event carries `runner: "grok"` so machine consumers see the truth |

---

## 3. User Stories

### US-001: Runner trait extraction (Phase 1, zero behavior change)

**As a** loop engine maintainer
**I want** `spawn_claude` to dispatch through a `LlmRunner` trait
**So that** future runner implementations can be added without touching
every call site

**Acceptance Criteria:**

- [ ] New module `src/loop_engine/runner.rs` defines:
  - `trait LlmRunner: Send + Sync` with `fn spawn(...)` method
  - `enum RunnerKind { Claude, Grok }`
  - `struct RunnerOpts<'_>` (renamed from `SpawnOpts`)
  - `struct RunnerResult` (renamed from `ClaudeResult`)
  - `struct ClaudeRunner` impl wrapping current `spawn_claude` body
  - `fn dispatch(kind: RunnerKind, prompt, mode, opts) -> Result<RunnerResult>`
- [ ] `src/loop_engine/claude.rs:270` `spawn_claude` becomes a wrapper:
  builds `RunnerKind::Claude`, calls `dispatch`. Public signature
  unchanged.
- [ ] `SpawnOpts` is preserved as a `type` alias for `RunnerOpts<'_>`
  and `ClaudeResult` as `type` alias for `RunnerResult` so all 10
  existing `spawn_claude` call sites continue to compile with NO source
  modifications.
- [ ] `src/loop_engine/mod.rs` re-exports the `runner` module.
- [ ] `cargo test --lib loop_engine` passes, including
  `claude::tests::spawn_claude_echo`-based tests (binary mocking via
  `CLAUDE_BINARY` env var continues to work).
- [ ] No behavioral telemetry difference: a regression-style test runs
  a small loop iteration through both the old and new code paths and
  asserts byte-identical stdout/stderr.

### US-002: GrokRunner implementation + provider identity (Phase 2)

**As a** loop engine maintainer
**I want** a working `GrokRunner` that takes the same `RunnerOpts` and
produces equivalent results
**So that** explicit `model: "grok-4-fast"` task entries route to grok
end-to-end

**Acceptance Criteria:**

- [ ] `src/loop_engine/runner.rs` adds `struct GrokRunner` impl with
  the flag mapping table from §6 Public Contracts.
- [ ] Binary resolution: `$GROK_BINARY` → `cli_binary` (if config
  present) → `"grok"` (PATH lookup). Works even when `fallbackRunner`
  config is absent.
- [ ] Per-provider session cleanup superseded by runner-trait-hygiene
  PRD: `LlmRunner::cleanup_session` (called by `dispatch` post-spawn)
  replaces the opt-in `cleanup_title_artifact` flag. See
  `src/loop_engine/CLAUDE.md` § "Session artifact cleanup".
- [ ] **Grok auth-failure sniff**: stderr string match against
  `["not authenticated", "please run grok login", "grok login required"]`
  PLUS fast-fail heuristic (non-zero exit within 3s of spawn with any
  such string present) → returns
  `TaskMgrError::GrokAuthFailure { hint: String }`. Documented in
  rustdoc.
- [ ] `src/loop_engine/model.rs` adds:
  - `enum Provider { Claude, Grok }`
  - `fn provider_for_model(Option<&str>) -> Provider` using
    **token-equality**: lowercase input, split on `-`, return
    `Grok` if any token equals `"grok"`, else `Claude`
  - Early guards on `escalate_model`, `escalate_below_opus`,
    `to_1m_model` returning `None` when `provider_for_model(input) !=
    Provider::Claude`
- [ ] New test `tests/grok_runner.rs` (gated `#[ignore]` for CI without
  grok installed) confirms the three control tags emit correctly from
  a real `grok` invocation against a **realistic-scale** prompt
  fixture (not just the spike's synthetic prompts — load CLAUDE.md +
  skills + a fake learnings section to approximate real iteration
  prompt size).
- [ ] Test mutex generalizes to `RUNNER_BINARY_MUTEX` covering both
  `CLAUDE_BINARY` and `GROK_BINARY`.
- [ ] Unit tests for `provider_for_model`:
  - `"grok-4-fast"`, `"grok-4"`, `"grok-code-fast-1"` → `Grok`
  - `"groq-llama-70b"`, `"groq-llama-3"` → `Claude` (defensive: Groq
    Inc. is not xAI)
  - `"claude-opus-4-7"`, `None`, `""`, `"unknown-model"` → `Claude`
  - Mixed-case (`"GROK-4"`, `"Grok-4-Fast"`) → `Grok`

### US-003: Fallback policy in overflow ladder (Phase 3a)

**As a** task-mgr operator
**I want** my loop to automatically retry overflowed tasks on Grok
**So that** I don't have to manually intervene every time Opus[1M] hits
the prompt limit

**Acceptance Criteria:**

- [ ] `RecoveryAction::FallbackToProvider { provider: String, model: String }`
  added between `To1mModel` and `Blocked` in
  `src/loop_engine/overflow.rs:31`. Serde tag is
  `"fallback_to_provider"` (snake_case via existing
  `rename_all = "snake_case"`).
- [ ] `RecoveryAction::user_message` formats the new variant (e.g.,
  "Prompt is too long for X at effort high, model
  claude-opus-4-7[1m] — falling back to grok-4-fast (Claude ladder
  exhausted)").
- [ ] `handle_prompt_too_long` selects the new rung only when ALL of:
  - `project_config.fallback_runner` is `Some(cfg)` AND `cfg.enabled`
  - **AND** `effective_runner` (computed via the single-source formula
    above) is `RunnerKind::Claude` (idempotency guard — pinned to
    `effective_runner` only, NOT an OR with `runner_overrides.get`)
- [ ] On rung 4 selection:
  - `ctx.runner_overrides.insert(task_id, RunnerKind::Grok)`
  - `ctx.model_overrides.insert(task_id, cfg.model.clone())`
  - `UPDATE tasks SET model = ?1, status = 'todo', started_at = NULL
     WHERE id = ?2` (the model UPDATE is critical — see
     `tasks.model` interaction in §2.5)
- [ ] `OverflowEvent` (`src/loop_engine/overflow.rs:85`) gains
  `runner: Option<String>` field with
  `skip_serializing_if = "Option::is_none"` for backward-compatible
  JSONL parsing.
- [ ] Iteration banner gains `(via grok)` suffix when the resolved
  `effective_runner == RunnerKind::Grok`.
- [ ] When `fallbackRunner.enabled == false` OR config absent: behavior
  is byte-identical to today (4-rung ladder ending in `Blocked`).
- [ ] Idempotency test: call `handle_prompt_too_long` on a task whose
  `effective_runner` resolves to Grok → returns `Blocked`, NOT
  `FallbackToProvider`.

### US-004: Fallback policy in RuntimeError escalation (Phase 3b)

**As a** task-mgr operator
**I want** terminally-crashing tasks to escape the Opus ceiling onto
Grok
**So that** the loop doesn't burn iterations forever on a model that's
consistently failing the same task

**Acceptance Criteria:**

- [ ] `engine.rs::escalate_task_model_if_needed` (the per-task
  consecutive-failure escalation function) gains a hook: after the
  existing escalation logic resolves to Opus AND the task's
  `tasks.consecutive_failures >= cfg.runtime_error_threshold`
  (default 2) AND `effective_runner == RunnerKind::Claude` AND
  `cfg.enabled`, promote:
  - `ctx.runner_overrides.insert(task_id, RunnerKind::Grok)`
  - `ctx.model_overrides.insert(task_id, cfg.model.clone())`
  - `UPDATE tasks SET model = ?1 WHERE id = ?2` (Grok model)
  - Counter does NOT reset on promotion — preserves the existing
    failure-accounting contract
- [ ] **Wave mode wiring**: the hook fires in the post-wave
  aggregation step on the main thread (after `process_slot_result`
  runs the shared `iteration_pipeline`), NOT inside the slot worker.
  This preserves the `run_slot_iteration` invariant that slot threads
  do not mutate shared state.
- [ ] When `fallbackRunner.enabled == false`: existing behavior
  preserved byte-identically.
- [ ] `TaskMgrError::GrokAuthFailure` from a prior Grok attempt does
  NOT increment `consecutive_failures` (so an auth lapse doesn't push
  a healthy task into `auto_block_task`).
- [ ] New test verifies the threshold-triggered promotion in a
  synthetic scenario (mocked failure stream with Opus at ceiling).

### US-005: Project config schema for fallback (Phase 3c — companion to 3a/3b)

**As a** task-mgr operator
**I want** to enable Grok fallback via project config without code
changes
**So that** I can flip it on per-project and let the loop pick it up

**Acceptance Criteria:**

- [ ] `src/loop_engine/project_config.rs:11` `ProjectConfig` gains
  `pub fallback_runner: Option<FallbackRunnerConfig>` (snake_case
  Rust, camelCase JSON via serde rename).
- [ ] New struct `FallbackRunnerConfig { enabled: bool, provider:
  String, model: String, cli_binary: Option<String>,
  runtime_error_threshold: u32 }` with `Default` impl producing
  `enabled: false`.
- [ ] Loader behavior:
  - Absent key → `None`
  - Explicit `"fallbackRunner": null` → `None`
  - Explicit object → parsed; missing optional fields use defaults
    (`provider: "grok"`, `model: "grok-4-fast"`,
    `runtime_error_threshold: 2`, `cli_binary: None`)
- [ ] **`GrokRunner` binary resolution is config-independent** (see
  US-002): `$GROK_BINARY` → `cli_binary` (if config present) →
  `"grok"`. Explicit task-model routing without config does NOT
  require `fallbackRunner` to be set.
- [ ] Loop startup binary-existence check fires ONLY when
  `enabled: true`. If `enabled == true` AND
  `which(cli_binary.as_deref().unwrap_or("grok"))` returns nothing,
  exit with a config error message naming the missing binary BEFORE
  the first iteration.
- [ ] Round-trip serde tests covering: present + enabled, present +
  disabled, absent, explicit-null, partial (missing optional fields).
- [ ] Version-check requirement DROPPED per architect review: parsing
  `grok --help` output is brittle. The minimum grok version is
  documented in user-facing docs only.

### US-006: Override invalidation on explicit task-model change

**As a** task-mgr operator
**I want** editing a task's explicit `model:` field mid-loop to take
effect immediately
**So that** I can override auto-recovery decisions without restarting
the loop

**Acceptance Criteria:**

- [ ] `IterationContext` gains
  `overflow_original_task_model: HashMap<String, Option<String>>`
  capturing the value of `tasks.model` (DB column, NOT
  `prompt_result.resolved_model`) at first overflow / first fallback
  promotion. Default insertion uses
  `.entry(task_id).or_insert_with(|| read_task_model_from_db())`.
- [ ] On each iteration BEFORE runner selection: if the task has an
  entry in `overflow_original_task_model` AND the current
  `tasks.model` differs from that captured value, drop:
  - `runner_overrides[task_id]`
  - `model_overrides[task_id]`
  - `effort_overrides[task_id]`
  - `overflow_recovered`-remove(task_id)
  - `overflow_original_model[task_id]`
  - `overflow_original_task_model[task_id]`
  And emit a one-line stderr message
  ("Operator changed task model for {task_id} —
  clearing auto-recovery overrides; resolving fresh.").
- [ ] A test confirms the escape valve: set up a task with a Grok
  override, simulate operator updating `tasks.model = 'claude-haiku-4-5'`
  via direct DB write, run one iteration, assert that the spawn used
  Claude with haiku and ALL override maps are empty for that task.
- [ ] A test confirms the no-op edit case: if operator "edits"
  `tasks.model` to the same value already there, no state is cleared.

---

## 4. Functional Requirements

### FR-001: Runner trait shape

The loop engine must define a single point of subprocess dispatch
abstracted over runner provider.

**Details:**
- `LlmRunner::spawn(prompt: &str, mode: &PermissionMode, opts: RunnerOpts<'_>) -> TaskMgrResult<RunnerResult>` is the only method.
- `RunnerOpts` is `SpawnOpts` renamed and moved; lifetime parameter unchanged.
- `RunnerResult` is `ClaudeResult` renamed; fields unchanged.
- Static dispatch: `fn dispatch(kind, ...)` matches on `RunnerKind` enum.

**Validation:**
- All 10 existing call sites compile against the new wrapper without source modification (preserved via type aliases).
- Test seam (`CLAUDE_BINARY`) continues to work.

### FR-002: Grok flag translation

The `GrokRunner` impl maps Claude flags to their grok equivalents per
the table in §6 Public Contracts. Differences are silent (e.g.,
`cleanup_title_artifact` ignored) rather than panicking.

**Validation:**
- Integration test (gated `#[ignore]`) runs a real grok command and asserts the spike-prompt outputs.

### FR-003: Provider routing without cross-provider escalation

`Provider::Claude` and `Provider::Grok` are distinct ladders. Promotion
within a ladder uses existing functions; promotion BETWEEN ladders uses
only the explicit `FallbackToProvider` action.

**Details:**
- `provider_for_model` algorithm: lowercase input, split on `-`,
  return `Provider::Grok` iff any token equals `"grok"` exactly.
  Otherwise `Provider::Claude`.
- `provider_for_model(None | "")` → `Claude` (default; preserves
  today's behavior).
- `escalate_model(Some("grok-4-fast"))` → `None`.
- `provider_for_model("groq-llama-70b")` → `Claude` (defensive
  against Groq Inc. model strings).

**Validation:**
- Unit tests as enumerated in US-002.

### FR-004: Fallback rung selection

The 5-rung ladder in `handle_prompt_too_long`:

1. `downgrade_effort` (existing)
2. `escalate_below_opus` (existing)
3. `to_1m_model` (existing)
4. **`fallback_to_provider`** (NEW) — gated on config + single-source
   idempotency guard (`effective_runner == Claude`)
5. `blocked` (existing, terminal)

**Validation:**
- Parameterized tests covering each rung's selection criteria, with
  fallback enabled vs disabled.
- Dedicated idempotency test: `effective_runner == Grok` → returns
  `Blocked`, NOT `FallbackToProvider`.

### FR-005: Runtime-error fallback hook

After per-task escalation has reached or remained at the Opus ceiling
AND `tasks.consecutive_failures >= cfg.runtime_error_threshold` AND
`effective_runner == Claude` AND fallback config enabled, set the
runner override + model override + DB `tasks.model`.

**Details:**
- Counter: `tasks.consecutive_failures` (DB-persisted, per-task,
  existing column used by `escalate_task_model_if_needed`).
- Threshold default: 2.
- Counter does NOT reset on tier change OR on fallback promotion —
  preserves existing failure-accounting contract.
- Hook location: post-wave aggregation step on main thread (wave) or
  the existing escalation site (sequential), NOT slot workers.

**Validation:**
- Synthetic test injecting consecutive failures with Opus already
  resolved; asserts override flip.

### FR-006: Startup binary-existence check (fallback enabled only)

When `fallbackRunner.enabled == true`:
- At loop startup, before the first iteration, resolve the binary path
  (`cli_binary` or `"grok"`) and call `which`-equivalent.
- If not found, exit with `task-mgr loop start` config error citing
  the missing binary path. Suggest installing `grok` or correcting
  `cli_binary` path.

When `fallbackRunner.enabled == false` OR config absent:
- No check. Explicit-task-model routing into Grok finds the binary at
  runtime via the same resolution chain or surfaces a clear error from
  `spawn` if absent.

### FR-007: Auth failure sniff and short-circuit

`GrokRunner` inspects stderr for known auth-failure substrings
immediately before returning a generic error:

**Details:**
- Substrings: `"not authenticated"`, `"please run grok login"`,
  `"grok login required"` (case-insensitive).
- Fast-fail heuristic: subprocess exited non-zero within 3 seconds of
  spawn AND stderr matches any substring.
- On match: return `TaskMgrError::GrokAuthFailure { hint: String }`.
- Overflow handler treats this as: do NOT count toward overflow
  state; emit one-line stderr hint; mark task `blocked` with reason
  `"grok auth failed"`.
- RuntimeError hook treats this as: do NOT increment
  `consecutive_failures`; do NOT re-attempt fallback (it would
  cascade); mark task `blocked` with same reason.

### FR-008: Override invalidation on explicit task-model change

Before every runner selection on every iteration:
- If `overflow_original_task_model.get(&task_id)` is `Some(prev)`:
  - Read current `tasks.model` from DB
  - If different from `prev`: clear all 6 override entries for this
    task; emit stderr notice; proceed with fresh resolution.

This is the operator escape valve.

---

## 5. Non-Goals (Out of Scope)

- **Migrating background `spawn_claude` callers** (curate enrich/dedup,
  learnings ingestion, milestone summary, prd_reconcile, watchdog)
  to direct `LlmRunner` use. Per learning #656 these use
  `PermissionMode::Scoped` and don't benefit from fallback. They keep
  calling the `spawn_claude` wrapper.
- **Per-task `fallback: bool` PRD JSON override.** Project-scoped
  config is enough for v1.
- **Additional providers** (OpenAI, Gemini, etc.). The trait makes them
  trivial to add. Each is its own integration.
- **Cost / fallback-rate dashboards.** The data lands in
  `overflow-events.jsonl`; surfacing it is a separate concern.
- **`grok --help` flag-surface parsing for version drift detection.**
  Brittle; documented minimum version in user docs is sufficient.
- **`ClaudeMergeResolver` migration** to use the new runner trait. It
  already has its own trait abstraction (per learnings #1989, #2699).
- **Pass-through of grok-specific flags** (`--best-of-n`, `--check`,
  `--no-plan`, `--rules`, `--sandbox`).
- **DB persistence of `runner_overrides`/`model_overrides`/etc.**
  (per design decision — restart resets all override state, matching
  existing in-memory pattern).
- **Per-provider `overflow_original_model` keying** so Grok-overflow
  banners show the Grok model. Cosmetic; v1 banner always shows the
  first (Claude) overflow's original model. JSONL events carry truth.

---

## 6. Technical Considerations

### Affected Components

| File:Line | Change |
|---|---|
| `src/loop_engine/runner.rs` (NEW) | Trait + enums + `ClaudeRunner` + `GrokRunner` + dispatch + auth-failure sniff |
| `src/loop_engine/claude.rs:270` | `spawn_claude` shrinks to wrapper over `dispatch(RunnerKind::Claude, ...)` |
| `src/loop_engine/claude.rs:1221` | Generalize `CLAUDE_BINARY_MUTEX` → `RUNNER_BINARY_MUTEX`; add `spawn_grok_echo` helper |
| `src/loop_engine/model.rs` | `Provider`, token-equality `provider_for_model`, escalation guards |
| `src/loop_engine/engine.rs:228-279` | Add `runner_overrides: HashMap<String, RunnerKind>` AND `overflow_original_task_model: HashMap<String, Option<String>>` fields on `IterationContext` |
| `src/loop_engine/engine.rs:561, 2285` | Route through runner dispatch at the two main iteration call sites; compute `effective_runner` via single-source formula; invoke override-invalidation check FIRST |
| `src/loop_engine/engine.rs:4581-4609` (`escalate_task_model_if_needed`) | RuntimeError fallback hook; UPDATE `tasks.model` to Grok when fallback fires |
| `src/loop_engine/engine.rs` post-wave aggregation site | RuntimeError fallback hook for wave mode (main thread, after `process_slot_result`) |
| `src/loop_engine/overflow.rs:31` | New `RecoveryAction::FallbackToProvider` variant |
| `src/loop_engine/overflow.rs:85` | `OverflowEvent.runner` optional field |
| `src/loop_engine/overflow.rs:330` | New rung 4 in `handle_prompt_too_long`; UPDATE `tasks.model` when rung fires; single-source idempotency guard |
| `src/loop_engine/project_config.rs:11` | `FallbackRunnerConfig` struct + ProjectConfig field + serde tests |
| `src/loop_engine/mod.rs` | Re-export `runner` module |
| `tests/grok_runner.rs` (NEW) | Real-grok integration tests (`#[ignore]`d for CI) with realistic-scale prompt fixture |
| `tests/grok_fallback_policy.rs` (NEW) | Synthetic-failure-stream tests for both rungs + idempotency + override invalidation |

### Dependencies

- External: `grok` CLI binary on PATH (or `cli_binary` override) for
  fallback paths. Documented minimum version: must support
  `--permission-mode dontAsk`, `--tools`, `--disallowed-tools`,
  `--output-format streaming-json`, `-p`, `--cwd`, `--effort`,
  `--model`. The version inspected in the spike supports all.
- Internal: none new.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| **Trait + enum static dispatch** (selected) | Zero allocation per call; easy to test (static); compiler verifies exhaustive match; pattern matches `ClaudeMergeResolver` prior art | Adds a new match arm per new provider | **Preferred** |
| `Box<dyn LlmRunner>` dynamic dispatch | Idiomatic Rust for plugin systems; cleanest extension | Heap allocation per call; harder to mock without trait objects; overkill for 2 known runners | Rejected — premature abstraction |
| Conditional flag building inside one `spawn_claude` function | Smallest diff; no new module | All runner knowledge tangled in one 700-line function; future providers compound the mess | Rejected — debt cost > extraction cost |

**Selected Approach**: Trait + enum static dispatch.

**Phase 2 Foundation Check**: Yes. ~1 day now saves ~1-2 weeks of
detangling later when OpenAI/Gemini support is asked for. 1:10+ ratio.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| **Tag-emission drift on real iteration prompts** (spike validated synthetic prompts; full loop prompts are 10-100x larger) | High — silent mis-classification | Medium | US-002's `#[ignore]`d test uses a realistic-scale prompt fixture, not just spike-scale; if drift observed, add `--rules` system-prompt override to enforce protocol |
| **Side-effect parity in agentic mode** (spike used `--permission-mode plan`; production needs real edits + commits) | High — could leave worktree inconsistent | Medium | Manual smoke test in US-005 verification with a real edit + commit task; if grok's commit format differs, the wrapper-commit logic may need a tweak |
| **Override-shadowing bug**: `tasks.model` DB column not updated → `resolve_task_model` re-resolves to Opus → in-memory `model_overrides` is silently shadowed | Critical (silent regression of fallback) | Low (architect caught it) | FR-004 + FR-005 explicitly require `UPDATE tasks SET model = ?` in the same transaction as the override insertion; dedicated test confirms post-restart of `effective_model` matches Grok |
| **Grok auth lapse cascades into `auto_block_task`** | Medium — wastes operator's debug time on a wrong root cause | Medium-Low (auth state is real but auth lapse is occasional) | FR-007 auth-failure sniff returns distinct error; handlers skip promotion and skip counter increment |

### Security Considerations

- `grok` CLI inherits the same permission-mode discipline as `claude`.
  No new attack surface beyond "an additional binary can edit files in
  the worktree."
- `GROK_BINARY` env var honors arbitrary paths — same caveat as
  `CLAUDE_BINARY`. Documented as a test-only seam.
- Startup binary check (FR-006) prevents silent reroute to a malicious
  binary when fallback is enabled.
- xAI API key / `grok login` state is the operator's concern.

### Public Contracts

#### New Interfaces

| Module/Symbol | Signature | Returns (success) | Returns (error) | Side Effects |
|---|---|---|---|---|
| `loop_engine::runner::LlmRunner::spawn` | `(&self, prompt: &str, mode: &PermissionMode, opts: RunnerOpts<'_>) -> TaskMgrResult<RunnerResult>` | `RunnerResult` | `TaskMgrError::IoError`, `TaskMgrError::GrokAuthFailure` (GrokRunner only) | Subprocess executes |
| `loop_engine::runner::RunnerKind` | `enum { Claude, Grok }` | n/a | n/a | n/a |
| `loop_engine::runner::dispatch` | `(kind: RunnerKind, prompt, mode, opts) -> TaskMgrResult<RunnerResult>` | as above | as above | as above |
| `loop_engine::model::Provider` | `enum { Claude, Grok }` | n/a | n/a | n/a |
| `loop_engine::model::provider_for_model` | `(model: Option<&str>) -> Provider` | `Grok` iff input lowercase-tokenized on `-` contains `"grok"` exactly; else `Claude` | n/a (pure) | none |
| `loop_engine::overflow::RecoveryAction::FallbackToProvider` | `{ provider: String, model: String }` | n/a | n/a | serialized as `{"action": "fallback_to_provider", ...}` |
| `loop_engine::project_config::FallbackRunnerConfig` | `{ enabled: bool, provider: String, model: String, cli_binary: Option<String>, runtime_error_threshold: u32 }` | n/a | n/a | loaded from `.task-mgr/config.json -> fallbackRunner` |
| `TaskMgrError::GrokAuthFailure` | `{ hint: String }` | n/a | n/a | distinct from `IoError` so handlers can short-circuit |

#### Modified Interfaces

| Module/Symbol | Current Signature | Proposed Signature | Breaking? | Migration |
|---|---|---|---|---|
| `loop_engine::claude::spawn_claude` | `(prompt: &str, mode: &PermissionMode, opts: SpawnOpts<'_>) -> TaskMgrResult<ClaudeResult>` | Same — wrapper around `dispatch(RunnerKind::Claude, ...)` | No (type aliases preserve signature) | None |
| `loop_engine::engine::IterationContext` | `model_overrides`, `effort_overrides`, etc. | Adds `runner_overrides: HashMap<String, RunnerKind>` AND `overflow_original_task_model: HashMap<String, Option<String>>` | No (additive fields) | None |
| `loop_engine::overflow::RecoveryAction` | 4 variants | 5 variants | No for serde-by-key consumers | New variant — downstream parsers must tolerate unknown `action` strings |
| `loop_engine::overflow::OverflowEvent` | Fields as today | Adds `runner: Option<String>` with `skip_serializing_if` | No (optional, omitted-when-None) | None |
| `loop_engine::project_config::ProjectConfig` | Fields as today | Adds `pub fallback_runner: Option<FallbackRunnerConfig>` | No (optional) | None |

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
|---|---|---|
| Config JSON → `ProjectConfig` → `FallbackRunnerConfig` field access | JSON: string (camelCase `"fallbackRunner"`); `ProjectConfig`: Rust field `fallback_runner: Option<FallbackRunnerConfig>` (snake_case, serde rename); `FallbackRunnerConfig`: typed struct fields | `let cfg = read_project_config(db_dir)?; if let Some(fr) = cfg.fallback_runner.as_ref() { if fr.enabled { /* use fr.model, fr.runtime_error_threshold, fr.cli_binary */ } }` |
| Per-task runner resolution (single source of truth) | `effective_model: Option<String>` (post-override) → `RunnerKind` via override lookup → `provider_for_model` fallback | `let effective_runner = ctx.runner_overrides.get(&task_id).copied().unwrap_or_else(\|\| match provider_for_model(effective_model.as_deref()) { Provider::Grok => RunnerKind::Grok, Provider::Claude => RunnerKind::Claude });` — compute ONCE per iteration, pass to spawn AND to overflow handler |
| Override-invalidation check | `tasks.model` (DB string column, nullable) vs `overflow_original_task_model: HashMap<String, Option<String>>` (in-memory snapshot) | `let current = read_task_model(conn, task_id)?; if let Some(prev) = ctx.overflow_original_task_model.get(task_id) { if &current != prev { clear_all_overrides(ctx, task_id); } }` |
| `OverflowEvent.runner` serialization | Rust: `Option<String>`; JSON: `"runner"` (snake_case, skip-if-none) | Reader: `let runner = event.get("runner").and_then(Value::as_str).unwrap_or("claude");` |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|---|---|---|---|
| `src/loop_engine/engine.rs:2236, 2253` | Reads `ctx.model_overrides`/`effort_overrides` for sequential iteration | OK — additive new fields | None needed |
| `src/loop_engine/engine.rs:561` (wave) | Reads same overrides for slot iteration | OK | None needed |
| `src/loop_engine/engine.rs:4599-4602` (`escalate_task_model_if_needed`) | Writes `tasks.model` on Opus escalation | NEEDS CARE — fallback hook must also UPDATE tasks.model to grok-model; otherwise resolve_task_model on next iteration shadows the override | Implemented in FR-005; dedicated test verifies |
| 8 background `spawn_claude` callers | Keep calling `spawn_claude` wrapper | OK | None needed |
| External JSONL consumers of `overflow-events.jsonl` | Parse `recovery.action` field | NEEDS REVIEW — new value `"fallback_to_provider"` may not be recognized | Document the new value in `src/loop_engine/CLAUDE.md`; existing parsers should tolerate unknown actions |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
|---|---|---|---|
| `escalate_model` (Claude-ladder ascent) | Per-task crash escalation | Returns `Some(next_tier)` for known Claude; `None` for unknown | Same; explicitly returns `None` for grok-* (provider guard) |
| `provider_for_model` (NEW) | Runner dispatch | n/a | Token-equality on `-` splits; `groq` ≠ `grok`; default `Claude` |
| `handle_prompt_too_long` rung selection | Overflow recovery | 4 rungs; first-match wins; ends at `Blocked` | 5 rungs; same first-match contract; rung 4 conditional on config + idempotency guard (single-source) |
| `escalate_task_model_if_needed` | Per-task crash escalation | Writes `tasks.model` to escalated Claude model | If fallback hook fires: writes `tasks.model` to Grok model in same logical operation |
| `IterationContext` initialization | Loop start / restart | All override maps default `HashMap::new()` | Same — restart drops state; design decision is in-memory only |

### Inversion Checklist

- [x] All `spawn_claude` callers identified (10 production, 1 test helper)
- [x] Routing decisions reviewed (`provider_for_model` guard placement; single-source `effective_runner`)
- [x] Tests that validate current ladder behavior identified
- [x] Different semantic contexts for same code documented
- [x] DB column `tasks.model` interaction with in-memory overrides traced
- [x] Operator escape valve (mid-loop edit) verified to clear overrides
- [x] Grok auth-lapse cascade mitigated via distinct error variant
- [x] Wave-mode vs sequential-mode wiring of RuntimeError hook documented (main thread only)

### Documentation

| Doc | Action | Description |
|---|---|---|
| `src/loop_engine/CLAUDE.md` "Overflow recovery and diagnostics" | Update | Add 5th rung; document `fallback_to_provider`; document idempotency guard formula; document operator escape valve |
| `CLAUDE.md` (project) | Update | Note `fallbackRunner` config block under "task-mgr workflow" |
| `src/loop_engine/runner.rs` rustdoc | Create | Document `LlmRunner` trait contract; rationale for static dispatch via enum; per-runner flag mapping table; auth-failure sniff for Grok |
| Per-file rustdoc on new symbols | Create | `Provider`, `provider_for_model` (with algorithm explanation), `FallbackRunnerConfig`, `RecoveryAction::FallbackToProvider`, `IterationContext::runner_overrides`, `IterationContext::overflow_original_task_model`, `TaskMgrError::GrokAuthFailure` |
| `tasks/grok-fallback-runner-prompt.md` (NEW) | Create | Generated by `/tasks` from this PRD |

---

## 7. Open Questions

All major policy questions resolved during architect review.
Remaining items, all v2/follow-up:

- [ ] **Should `fallbackRunner` also override `merge_resolver`** when
  the merge_resolver subprocess hits its own crashes? Deferred to a
  follow-up; merge_resolver already has its own trait.
- [ ] **Fallback success telemetry as automatic learnings**: should a
  successful Grok recovery emit a `task-mgr learn` automatically so
  future similar tasks preemptively route to Grok? Out of v1 scope.
- [ ] **Eager `--rules` system-prompt override** for `GrokRunner` as
  defense-in-depth against tag drift on large prompts? Decision can be
  made during US-002 integration testing — only add if drift observed.
- [ ] **Per-provider banner annotation** so Grok-side overflow events
  show the Grok original model. Currently v1 always shows the first
  (Claude) overflow. Cosmetic.

---

## Appendix

### Related Documents

- `/home/chris/.claude/plans/yes-check-the-tag-validated-hellman.md` —
  scratch plan from this session
- `src/loop_engine/CLAUDE.md` "Overflow recovery and diagnostics" —
  current 4-rung ladder
- Spike artifacts: `/tmp/grok-spike-out.txt`,
  `/tmp/grok-blocked-out.txt`, `/tmp/grok-reorder-out.txt`
- Architect review (this session) — surfaced 7 spec changes + 4
  edge cases now incorporated

### Glossary

- **Runner**: a subprocess executor for the agentic LLM CLI. Today
  only `ClaudeRunner` exists implicitly inside `spawn_claude`; this
  PRD formalizes the abstraction and adds `GrokRunner`.
- **Rung**: one step on the overflow recovery ladder in
  `handle_prompt_too_long`. The ladder walks rungs in order and
  selects the first whose preconditions are met.
- **Override**: a per-task entry in `IterationContext` that supersedes
  the default model / effort / runner selection for subsequent
  iterations of the same task. Set by recovery handlers, read at the
  runner-dispatch site. In-memory only — cleared on loop restart or
  on explicit operator task-model edit.
- **Effective runner**: the single value computed per iteration from
  `runner_overrides.get(task_id)` → `provider_for_model(effective_model)`
  → `Claude`. Used for both spawn dispatch and idempotency guards in
  failure handlers. SINGLE source of truth.
- **Operator escape valve**: editing `tasks.model` in the DB (via
  `task-mgr loop init --append --update-existing` or direct edit)
  causes the next iteration to detect the change vs.
  `overflow_original_task_model` and drop all override state.
- **Tag protocol**: the loop's contract with the LLM for emitting
  structured control signals in stdout (`<promise>`, `<task-status>`,
  `<key-decision>`, `<reorder>`).
- **Provider**: the upstream LLM platform (Claude via Anthropic /
  Bedrock / Vertex; Grok via xAI). Identified by token-equality match
  on the model ID's `-`-split tokens.
