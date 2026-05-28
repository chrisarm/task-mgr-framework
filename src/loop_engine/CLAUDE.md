# src/loop_engine — design notes

Cross-file narrative for the iterative loop subsystem. Module-level invariants
that touch multiple files; per-file/per-fn contracts live in rustdoc next to
the code. Several specific don't-do-this rules have been migrated to
`task-mgr learn` so they surface via `recall --for-task` — see
[Touchpoints](#touchpoints) for pointers.

## Auto-launch /review-loop after loop end

After a clean loop exit (all tasks complete), `task-mgr` can spawn an interactive
`claude "/review-loop tasks/<prd>.md"` session automatically. The user lands directly
in the review without a manual hand-off step.

**Default behavior**: fires when `autoReview: true` (default) AND `tasks_completed >= autoReviewMinTasks`
(default 3). Both live in `.task-mgr/config.json`. An empty config means both defaults apply.

**CLI overrides** (clap-enforced mutual exclusion):
- `--auto-review` — force on; treats the task-count threshold as 1
- `--no-auto-review` — force off unconditionally

**Batch mode**: ONE review fires at end-of-batch for the LAST successful PRD that met the
threshold — never per-PRD. Earlier PRDs in the batch are skipped even if they individually
qualified.

**Suppression cases** (prints a recovery hint, continues, exit code unchanged):
- Non-TTY stdout (CI, pipes) — hint: re-run interactively to get the review
- `tasks/<prd>.md` not found AND `tasks/prd-<stem>.md` not found — hint: name the markdown file to match
- Worktree path missing or cleaned up — hint: re-run `claude "/review-loop tasks/<prd>.md"` manually

**Process model**: `Command::status()` — blocking spawn, stdin/stdout/stderr inherit so the
review session is fully interactive. `ANTHROPIC_API_KEY` and other env vars inherit automatically.

**Module**: `src/loop_engine/auto_review.rs` — `Decision`, `resolve_decision`, `should_fire`,
`ReviewLauncher` trait, `maybe_fire`.

**Invariant**: auto-review failure NEVER changes the loop or batch exit code.

**Known footgun — paths with whitespace**: `ProcessLauncher::launch`
(`src/loop_engine/auto_review.rs:130`) interpolates the PRD path into a single
slash-command argv element: `format!("/review-loop {}", md.display())`. Claude
re-tokenizes the slash-command body on whitespace, so a PRD path containing
spaces (e.g. `tasks/My PRD.md`) splits into multiple tokens and the review
launch fails to find the file. Not a security issue (no shell, `Command::arg`
is safe), but project convention is space-free `tasks/<feature>.md` paths for
exactly this reason — keep it that way. If the Claude CLI grows a structured
args form, prefer that over in-band quoting.

`maybe_fire` enforces this convention with a launch-boundary guard: if the
resolved markdown path contains any `char::is_whitespace` character, the
launch is suppressed and a stderr hint tells the operator to rename the file
and re-run `/review-loop` manually. The guard sits AFTER `prd_md_path` (so it
sees the actual file we'd hand to Claude) and BEFORE `launcher.launch` (so
no fragmented argv ever reaches `claude`). It deliberately does not attempt
to quote or escape — quoting Claude's slash-command body is brittle, and
suppression with a clear hint is the simpler, more honest contract.

**Outer/inner split for test reachability**: `maybe_fire` is a thin
wrapper that performs the TTY pre-check and delegates to
`maybe_fire_inner` (`pub(crate)`), which contains every launch-decision
gate (decision, worktree existence, markdown path resolution, whitespace
guard, launcher dispatch). `cargo test` runs in a non-TTY env, so a unit
test that goes through the public `maybe_fire` would short-circuit at
the TTY gate before reaching any inner gate — meaning a test asserting
"this guard suppresses launch" via `CapturingLauncher` would pass even
if the guard were deleted. Tests for inner-side gates
(`maybe_fire_inner_*`) call the inner function directly to bypass the
TTY gate and exercise the real guard logic; a single
`maybe_fire_outer_suppresses_in_non_tty` test exercises the outer
wrapper to prove the TTY gate still fires. When adding a new
launch-boundary guard, add it inside `maybe_fire_inner` and test it via
the inner — never via the outer.

## primaryRunner config and routing

`primaryRunner` in `.task-mgr/config.json` routes specific task types or ID
prefixes to a non-default runner (Grok today) BEFORE the `difficulty=high →
Opus` escalation. All other tasks continue on the default Claude runner. This
is the mirror of `fallbackRunner` — instead of promoting a stuck Claude task to
Grok as a last resort, `primaryRunner` routes designated task classes to Grok
as the FIRST choice.

### Config block

```json
{
  "primaryRunner": {
    "claudeFallbackModel": "<a Claude model id — e.g. the SONNET_MODEL constant in src/loop_engine/model.rs>",
    "runtimeErrorThreshold": 2,
    "byTaskType": {
      "review":    { "provider": "grok", "model": "grok-build" },
      "milestone": { "provider": "grok", "model": "grok-build" }
    },
    "byIdPrefix": {
      "REVIEW-":    { "provider": "grok", "model": "grok-build" },
      "MILESTONE-": { "provider": "grok", "model": "grok-build" }
    }
  }
}
```

Field defaults: `claudeFallbackModel=null`, `runtimeErrorThreshold=2`,
`byTaskType={}`, `byIdPrefix={}`. Absent or `null` → `primary_runner = None`
in `ProjectConfig`; loop behavior is byte-identical to a pure-Claude run.

### Routing precedence (`resolve_task_model`)

```
explicit task model   (tasks.model DB column / model_overrides override)
  → primaryRunner match  (byTaskType wins over byIdPrefix when both match)
    → difficulty=high   (forces OPUS_MODEL)
      → prd default     (prd_metadata.default_model)
        → project default (.task-mgr/config.json defaultModel)
          → user default  ($XDG_CONFIG_HOME/task-mgr/config.json defaultModel)
            → None
```

Rung 2 (`primaryRunner`) is skipped entirely when `primary_runner = None` —
making the resolution chain byte-identical to the pre-primary-runner build.

**Match priority** inside `byTaskType` / `byIdPrefix` (both must be checked
for EVERY task):

1. `byTaskType` — exact, case-sensitive match on the semantic task type
   (e.g. `"review"`, `"milestone"`).
2. `byIdPrefix` — the task ID body (after stripping the 8-hex project prefix)
   starts with the map key, OR the body contains `"-<key>"`.

When both produce a match, `byTaskType` wins.

**SSoT**: `model::primary_runner_match` is the single implementation.
Do NOT re-implement the prefix-matching logic anywhere else.

### Symmetric Claude↔Grok fallback contract

`fallbackRunner` and `primaryRunner` form a symmetric pair:

| Direction | Config key | When it fires |
|---|---|---|
| Claude → Grok | `fallbackRunner.enabled=true` | Claude overflow-ladder exhausted (rung 4) OR consecutive RuntimeErrors ≥ `fallbackRunner.runtimeErrorThreshold` |
| Grok → Claude | `primaryRunner.claudeFallbackModel` set | Grok overflow-ladder exhausted (rung 4) OR consecutive RuntimeErrors ≥ `primaryRunner.runtimeErrorThreshold` |

Both paths share the same idempotency guard, and it is the SAME mechanism at
both sites: a single `ctx.runner_overrides.contains_key(task_id)` snapshot taken
BEFORE the promotion branch. If an override already exists (in EITHER direction),
the site bails to normal failure accounting (→ `auto_block_task`) instead of
promoting. A task can only cross the provider boundary ONCE per loop run
(in-memory override; clears on restart).

> ⚠️ Footgun: do NOT gate idempotency on a re-derivation like
> `provider_for_model(effective_model)` alone. Because a Grok→Claude promotion
> sets `runner_overrides[id]=Claude`, the next failure would otherwise enter the
> OPPOSITE (Claude→Grok) branch and flap providers every iteration (bounded only
> by `max_retries`, each flip spawning a real CLI subprocess). The RuntimeError
> escalation path shipped without this guard and ping-ponged; it now mirrors the
> overflow rung-4 `was_already_promoted` snapshot. When you add a THIRD
> cross-provider promotion site, replicate the `contains_key` guard there too.

`claudeFallbackModel` absent → no Grok→Claude fallback. The Grok task
dead-ends on `blocked` exactly as a Claude task without `fallbackRunner` does.

## Reaction framework (shared)

The loop engine has two execution paths — **sequential**
(`iteration.rs::run_iteration` driven by `orchestrator.rs::run_loop`) and
**parallel-wave** (`wave_scheduler.rs::run_wave_iteration` + `slot.rs`). Every
main-thread *reaction* that is NOT path-specific — the work the main thread does
before dispatching Claude and after Claude returns — lives in
`src/loop_engine/reactions/` and is called by BOTH paths. The wave path folds
its N slot results into one reaction; the sequential path folds its 1.

This module exists because the same reaction was historically implemented at one
path's call site and silently omitted or shaped differently in the other,
producing a recurring parity-divergence bug class (the latest: rate-limit waits
existed only in sequential, so wave mode never waited and false-aborted with "no
eligible tasks after 3 consecutive stale iterations", resetting in-flight work).

### The single-home contract (enforced at compile time)

Two mechanisms keep a reaction from being copy-pasted back into one path:

1. **`#[deprecated]` on the relocated leaf + `#![deny(deprecated)]` on the three
   engine files** (`iteration.rs:41`, `wave_scheduler.rs:47`, `slot.rs:32`). A
   direct call to a relocated leaf from any engine file fails `cargo build`; the
   only legitimate caller is the coordinator, which marks its single call site
   `#[allow(deprecated)]`. Re-inlining a relocated reaction is therefore a
   compile error, not a code-review judgment call.
2. **Exhaustive param-struct destructure (no `..`)** in every coordinator.
   Adding a field to a coordinator's param struct is a compile error until every
   coordinator body accounts for it — the parity divergence the framework exists
   to prevent becomes a compile-time concern.

### The converged coordinators (each called from BOTH paths)

| # | Coordinator | Module | Sequential call site | Wave call site | Relocated leaf (`#[deprecated]` shim) |
|---|---|---|---|---|---|
| #2 | `pre_spawn::resolve_task_execution` | `pre_spawn` | `iteration.rs:387` | `wave_scheduler.rs:1058` (per slot) | `recovery::{check_override_invalidation, check_crash_escalation}` |
| #3 | `account::account_usage_gate` | `account` | `iteration.rs:130` | `wave_scheduler.rs:249` (once/wave) | `usage::check_and_wait` |
| #5 | `post_output::handle_overflow` | `post_output` | `iteration.rs:755` | `slot.rs:535` (per slot) | `overflow::handle_prompt_too_long` |
| #6 | `account::react_to_outputs` | `account` | `iteration.rs:703` | `wave_scheduler.rs:1170` (once/wave) | `usage::{parse_reset_from_output, wait_for_usage_reset}` |
| #10 | `post_completion::react_to_completions` | `post_completion` | `orchestrator.rs:1207` | `wave_scheduler.rs:1482` | `orchestrator::trigger_human_reviews` |
| #13 | `account_iteration_budget` | `reactions` (mod) | `orchestrator.rs:1312` | `orchestrator.rs:1027` | (inline `iteration -= 1` / `saturating_sub`) |
| — | `account::react_to_transient` (FEAT-014) | `account` | `orchestrator.rs:1282` | `wave_scheduler.rs:1236` | (new; no pre-existing leaf) |

Account-global reactions (`account_usage_gate`, `react_to_outputs`,
`react_to_transient`) fire **exactly once per wave**, never once per
rate-limited slot — they reflect shared API-account state, not per-task state.
The per-task reactions (`resolve_task_execution`, `handle_overflow`) fold one
call per slot. Each coordinator pairs a production entry point with a hermetic
`_inner` core that takes the side-effecting step (wait / review) as an injected
seam, so `tests/reaction_parity.rs` can prove the sequential and wave shapes
compute identical results without OAuth, stdin, subprocesses, or real sleeps.

### Out of scope (NOT in the reactions framework)

Only two kinds of post-Claude work are deliberately left at the
`run_loop` / `run_wave_iteration` call sites:

- **pause-signal handling** — owns the signal-flag / `.stop` polling the
  per-iteration reactions do not carry.
- **slot merge resolution** (`worktree::merge_slot_branches_with_resolver`,
  `merge_resolver::ClaudeMergeResolver`) — requires the slot-0 merge worktree
  state owned by `run_wave_iteration`, not a per-iteration post-Claude concern
  (see "Slot merge-back conflict resolution").

Everything else that used to be "call-site inline glue" — wrapper-commit,
external-git reconciliation, human-review trigger, rate-limit / transient-backend
waits, the usage gate, the overflow ladder, the iteration-budget give-back — is
now a coordinator in the table above. `iteration_pipeline.rs`'s own "Out of
scope" note mirrors this split.

### Load-bearing invariants

- **`handle_overflow` ordering.** On a `PromptTooLong` outcome the overflow
  coordinator fires BEFORE the shared `iteration_pipeline::process_iteration_output`
  runs for that iteration/slot, in both paths. Recovery state (the `todo`/`blocked`
  DB reset + the ctx overrides) must be durable before the pipeline's
  crash-tracking write observes the outcome — otherwise the pipeline could
  account an overflowed-but-to-be-retried task as a terminal failure. Wave:
  `slot.rs::process_slot_result` calls `handle_overflow` then
  `process_iteration_output` a few lines later; sequential: `run_iteration`'s
  Step 8.5 runs before `run_loop` invokes the pipeline. Full ordering in
  "Overflow recovery and diagnostics".

- **`iteration_consumed == false` gives the loop-bound iteration back.** A
  `RateLimit` / `Reorder` / transient-backend `WaitedAndRetry` outcome routes
  through `account_iteration_budget` with `consumes_budget = false`, which does
  `*iteration = iteration.saturating_sub(1)` so a persistently rate-limited /
  unavailable run does not burn its `max_iterations` budget on waits (bounded
  termination then relies on the `.stop`/signal check, NOT the iteration
  ceiling). The wave path threads this as `WaveOutcome.iteration_consumed`
  (`orchestrator.rs:1030`); the sequential path computes `consumes_budget` from
  the outcome class (`orchestrator.rs:1306`). Both route through the ONE helper
  so the give-back rule cannot drift. A rate-limit retry wave additionally
  returns BEFORE merge-back with empty `failed_merges` and must NOT run the
  FEAT-002 reset/halt check (it would zero `consecutive_merge_fail_waves` and
  wipe the cascade-halt defense) — `orchestrator.rs:1041` `continue`s past it on
  `outcome.rate_limited_retry`.

- **Human review can fire on a partial wave.** `react_to_completions` runs once
  per wave at the post-merge-back step (after `apply_post_merge_reconcile`,
  before the terminal checks), so it can fire on a **partial wave** — one
  reaching the post-completion step with a sibling slot still `in_progress` or a
  sibling's ephemeral branch unmerged. Because the reaction is **input-driven**
  (it consumes the already-computed `completed_ids` set, never re-queries
  "everything completed since an epoch"), it reviews ONLY the completed
  `requires_human` ids and leaves every `in_progress` / unmerged sibling
  untouched. This is deliberate — a completed CLARIFY should unblock its
  dependents without waiting for the whole wave to drain. (The rate-limit /
  transient `WaitedAndRetry` reactions early-return BEFORE merge-back, so a wave
  that bails on a rate limit defers its completed tasks' reviews to a later wave;
  that is the retry path's existing contract, not a regression — the wave never
  reviewed at all pre-FEAT-010.) Detail in "Post-completion reactions
  (converged)".

- **The rate-limit reset filters on `status = 'in_progress'`.**
  `react_to_outputs` / `react_to_transient` reset the affected tasks via
  `TaskLifecycle::recover_in_progress_for_prefix`, whose `status = 'in_progress'`
  guard means a slot that already completed THIS wave (flipped to `done` by
  `process_slot_result`) is never clobbered.

## Overflow recovery and diagnostics

When the Claude CLI subprocess returns "Prompt is too long", the loop engine
walks a **five-rung recovery ladder** and writes a diagnostics bundle. Entry
point: `reactions::post_output::handle_overflow` in
`src/loop_engine/reactions/post_output.rs` (FEAT-005 relocated the body here;
the original `overflow::handle_prompt_too_long` leaf was a transition
`#[deprecated]` shim that FR-CLEANUP-001 then removed entirely — the only
home is the coordinator). The diagnostics primitives
(`sanitize_id_for_filename`, `dump_prompt`, `append_event_log`,
`rotate_dumps_keep_n`) and the wire types (`RecoveryAction`, `OverflowEvent`,
`DumpHeader`) stay in `src/loop_engine/overflow.rs` and are exercised
directly by the `tests/overflow_*.rs` equivalence-oracle suites.

**Both execution paths route through `handle_overflow`** on the `PromptTooLong`
crash outcome — sequential via Step 8.5 of `iteration.rs::run_iteration`
(`slot_index: None`), wave via `slot.rs::process_slot_result`
(`slot_index: Some(n)`). The three engine files
(`iteration.rs`/`slot.rs`/`wave_scheduler.rs`) carry `#![deny(deprecated)]`, so
a direct call to the old `handle_prompt_too_long` leaf is a compile error
(CONTRACT-001 single-home reaction lock).

**Ordering relative to `process_iteration_output`** (contractual, both paths):
`handle_overflow` fires BEFORE the shared post-Claude pipeline
(`iteration_pipeline::process_iteration_output`) runs for that iteration/slot.
In the wave path this is explicit — `process_slot_result` calls
`handle_overflow` and then `process_iteration_output` a few lines later; in the
sequential path `run_iteration`'s Step 8.5 runs before the pipeline is invoked
from the `run_loop` call site after `run_iteration` returns. The reason: the
overflow ladder must durably reset the task row (`todo` on rungs 1-4, `blocked`
on rung 5) and apply the ctx overrides BEFORE the pipeline's crash-tracking
write observes the outcome — otherwise the pipeline could account an
overflowed-but-to-be-retried task as a terminal failure.

**The ladder** (in order; first rung whose precondition is met wins):

| Rung | Action | Claude runner | Grok runner |
|---|---|---|---|
| 1 | Downgrade effort (`xhigh → high`) | ✓ | ✓ |
| 2 | Escalate model below Opus (`haiku → sonnet`, `sonnet → opus`) | ✓ | — |
| 3 | Escalate to 1M-context Opus (`opus → opus[1m]`) | ✓ | — |
| 4 | **FallbackToProvider** — cross-provider pivot | → Grok via `fallbackRunner` | → Claude via `primaryRunner.claudeFallbackModel` |
| 5 | Block (no further recovery) | ✓ | ✓ |

Rung 2 and 3 are Claude-only: Grok does not support the `--effort` flag or
the 1M-context variant in the same way, so Grok tasks skip straight from rung 1
to rung 4 when they hit a prompt-too-long ceiling.

**Rung 4 detail — `FallbackToProvider`**: fires only when:
- The effective runner is `RunnerKind::Claude` AND `fallback_runner` is
  `Some(cfg)` with `cfg.enabled = true` (Claude → Grok), **OR**
- The effective runner is `RunnerKind::Grok` AND `primary_runner` is
  `Some(pr)` with `pr.claude_fallback_model.is_some()` (Grok → Claude).

In both cases, the rung writes the target model to the `tasks.model` DB column
AND inserts matching entries into `ctx.runner_overrides` / `ctx.model_overrides`
atomically. Idempotency guard: a task already carrying a promotion override
(in either direction) skips this rung and falls through to rung 5. The DB
UPDATE AND the override-map inserts MUST run together — otherwise
`resolve_task_model` on the next iteration silently shadows the override.

Rungs 1–4 reset the task status to `todo` (and clear `started_at`) so the next
iteration retries with the override applied; rung 5 sets `blocked`. Behavior
is byte-identical to the pre-Grok 4-rung ladder when `fallbackRunner` is
absent or `enabled: false` — rung 4 is unreachable from the Claude direction
in that configuration, and the path collapses to rungs 1–3 → blocked.

**Operator escape valve — `check_override_invalidation`**: at the top of
every iteration (before `resolve_effective_runner`),
`recovery::check_override_invalidation` compares the current `tasks.model` DB
value against `ctx.overflow_original_task_model[task_id]` (the snapshot
captured at first overflow / RuntimeError fallback). When they diverge — i.e.
an operator edited `tasks.model` out-of-band — all six per-task auto-recovery
channels are cleared in one shot: `effort_overrides`, `model_overrides`,
`overflow_recovered`, `overflow_original_model`, `runner_overrides`,
`overflow_original_task_model`. A single stderr line announces the clear so
the operator sees the escape valve fired. Short-circuits for any task that
never triggered the ladder (the dominant case is free).

**Provider routing — `model::provider_for_model`**: classifies a model id as
`Provider::Claude` or `Provider::Grok` via **token equality on `-` splits of
the lowercased id**, returning `Provider::Grok` iff *some token is exactly*
`"grok"`. Substring matching (`.contains("grok")`) is explicitly prohibited
because it would mis-route Groq Inc. models (`groq-llama-3`, etc.) to the
xAI Grok runner. Every other input — `None`, the empty string, unknown
model ids, the Claude constants, and Groq family ids — falls through to
`Provider::Claude`. Total function: every `Option<&str>` produces some
`Provider`; never panics. This routine is the SINGLE source of truth used
by `resolve_effective_runner` (in `engine.rs`) for the spawn-site dispatch
discriminant — re-deriving the formula independently is explicitly
prohibited (PRD §2.5).

**Diagnostics bundle (best-effort; failures log via `eprintln!` and never
propagate)**:

- **Prompt dump**: written to
  `.task-mgr/overflow-dumps/<sanitized-task-id>-iter<n>-<unix-ts>.txt`. Contains
  metadata + per-section byte breakdown + dropped sections + the raw assembled
  prompt. Task IDs are sanitized via `overflow::sanitize_id_for_filename`
  (path-traversal defense; `..` collapsed before allowlist filtering).
- **JSONL event log**: appended one-line-per-event to
  `.task-mgr/overflow-events.jsonl`. Each line is a serialized
  `OverflowEvent` (`ts`, `task_id`, `run_id`, `iteration`, `model`, `effort`,
  `prompt_bytes`, `sections`, `dropped_sections`, `recovery`, `dump_path`).
  `sections` is an ordered JSON array of `[name, size]` pairs (NOT a map).
  `recovery` is a tagged object with discriminator field `action` and
  variant-specific siblings (e.g. `{"action": "escalate_model", "new_model": "..."}`).
- **Rotation**: keeps newest 3 dumps per task ID via
  `overflow::rotate_dumps_keep_n`. Each entry (unreadable dir entry, missing
  metadata, failed deletion) is logged and skipped independently so a single
  IO error never aborts the rest of the rotation pass.

**Banner annotation**: when a task is mid-recovery, the iteration banner emits
`(overflow recovery from <original-model>)` next to the model line. The banner
gates on `IterationContext::overflow_recovered` (a `HashSet<String>` of task
IDs that have hit the overflow handler at least once), NOT on `model_overrides`
— see learning #893: crash escalation and consecutive-failure escalation must
stay in their own channels. The original model is captured first-overflow only
via `IterationContext::overflow_original_model.entry().or_insert_with(...)`.

**Order of operations is contractual** (do not reorder):
ctx update → DB UPDATE → stderr → dump → JSONL → rotate. Recovery state must
be durable before any best-effort observability writes.

**Grok auth-failure detection** (`runner.rs::GROK_AUTH_FAILURE_SUBSTRINGS` +
`stderr_contains_auth_failure`): the auth-failure short-circuit relies on a
small set of case-insensitive substrings matched against captured stderr.
A missed match silently fails open — the task is counted toward
`consecutive_failures` and may be auto-blocked with a misleading "max
retries exceeded" reason rather than "grok auth failed". On every grok CLI
version bump, re-capture the unauthenticated stderr output via
`grok login --logout` (or by intentionally invalidating the token) and run
the binary once; extend the substring list in `runner.rs` if new phrasing
appears. Negative controls (`stderr_contains_auth_failure_w3_broader_phrasing`
in `runner.rs` unit tests) keep the list from drifting into false positives
on common error phrases like "file not found" or "rate limit exceeded".

**Transactional promotion ctx writes are deferred** (`recovery.rs::handle_task_failure`
+ `escalate_task_model_if_needed_inner` + `apply_pending_promotion`): the
RuntimeError fallback hook runs inside the same DB transaction that
increments `consecutive_failures` and (optionally) auto-blocks. If the ctx
mutations (`runner_overrides`, `model_overrides`,
`overflow_original_task_model`) happened inside the transaction body and
`tx.commit()` failed, the in-memory ctx would claim a promotion the DB
rolled back. The pattern is: inner helper performs DB writes only and
returns an `Option<PendingPromotion>`; the caller applies it via
`apply_pending_promotion` **only after `tx.commit()?` returns Ok**. Direct
callers (tests, sequential non-transactional paths) use the convenience
wrapper `escalate_task_model_if_needed` which applies immediately. Same
shape applies to any future "in-memory state mutation paired with DB
write inside a transaction" — split inner-helper / apply-pending /
defer-until-commit.

**Binary-resolution env var "" must fall through, and existence ≠
executable** (`runner.rs::resolve_grok_binary`
+ `project_config.rs::check_fallback_runner_binary`): both the runtime
resolver and the startup probe MUST treat an empty/whitespace
`GROK_BINARY` (or `CLAUDE_BINARY`) value as "unset" — `export VAR=""` is
a common shell footgun and a divergence between resolver and probe
surfaces as a confusing startup failure on a host where PATH lookup
would have succeeded. The startup probe additionally checks the
executable bit on Unix (`metadata.mode() & 0o111 != 0`) rather than just
`Path::exists()`; a non-executable file at the path produces a clearer
error up-front than a `std::io::Error` from spawn at first use. Any new
"binary path probe" code (additional providers, sidecar tools) should
honor both invariants — see `is_executable_path` in
`project_config.rs`.

**Single-source-of-truth drift sentinels are `assert!`, not
`debug_assert!`** (`slot.rs::process_slot_result` cross-check of
`slot_result.effective_runner` vs. `resolve_effective_runner(...)`
re-derivation): when a sentinel guards against a silent dispatch
mismatch (wrong-runner spawn, wrong-model resolution, wrong-binary
exec), the check belongs in release builds too. `debug_assert!` is
compiled out and the silent-mismatch consequence dwarfs the cost of a
single HashMap lookup. Reserve `debug_assert!` for invariants whose
violation is loud (panic in a downstream layer) or whose cost is
real (e.g., O(n) over a large collection). The drift sentinel is
cheap and the failure is silent — use `assert!`.

### Session artifact cleanup

Every `LlmRunner` impl provides `cleanup_session(session_id, cwd)`; `dispatch`
calls it unconditionally post-spawn when `RunnerResult.session_id.is_some()`.
`NotFound` is silent success — the artifact may never have been written or may
already be gone. Other errors emit one banner per process gated by
`CLEANUP_WARN_ONCE` (`AtomicBool::swap(true, Relaxed)`) and never modify the
spawn return value. Implementations derive the path deterministically from
`(session_id, cwd)` and remove ONLY that path — never enumerate-and-sweep,
never touch shared per-cwd dirs (e.g. Grok's `prompt_history.jsonl`).
`WORKAROUND(claude-code-2.1.110-session-stub)` and
`WORKAROUND(grok-cli-no-persistence-off)` markers tag the cleanup sites so
future upstream fixes are a one-grep removal.

## Iteration pipeline (shared)

Sequential (`run_iteration`) and parallel-wave (`run_slot_iteration` +
`process_slot_result`) execution paths share a single post-Claude pipeline:
`process_iteration_output` in `src/loop_engine/iteration_pipeline.rs`. The
module-level rustdoc lists the steps in order (progress logging,
`<key-decision>` extraction, `<task-status>` dispatch, completion ladder
including the `is_task_reported_already_complete` fallback, learning
extraction, bandit feedback, per-task crash tracking) and the two engine.rs
call sites (sequential at ~3204 in `run_loop`, wave at ~1166 in
`process_slot_result`).

**Why a shared pipeline**: before this unification, wave mode silently
skipped behaviors the sequential path treated as core — slot output was
never extracted for new learnings, bandit feedback never updated, and the
completion fallback didn't fire. The single-pipeline contract makes
parity-divergence a compile-time concern (any new step is added in one
place; both call sites pick it up).

**Prompt-builder companion**: `src/loop_engine/prompt/mod.rs` documents the
three-builder layout (`core` / `sequential` / `slot`) plus the main-thread
bundle rule — slot prompts must be built on the main thread before
`thread::spawn` because `rusqlite::Connection` is `!Send`. A compile-time
`Send` assertion on `SlotPromptBundle` enforces this. Both paths consume
sections through the shared assembler (`prompt/assembler.rs`); a
roster-completeness test in `tests/prompt_assembler_parity.rs` enforces that
every known section appears in the sequential roster (the hand-enforced wiring
rule has been retired).

**Out of scope for the pipeline** (kept at the call sites): rate-limit waits,
pause-signal handling, slot merge resolution (see "Slot merge-back conflict
resolution" below), and the post-merge-back slot completion reconcile (slot-0
`{pre_merge_head}..HEAD` scan via
`git_reconcile::reconcile_merged_slot_completions` — see the Touchpoints table
below). The remaining three post-Claude call-site reactions —
**wrapper-commit (#8)**, **external-git reconciliation (#9)**, and the
**human-review trigger (#10)** — were converged into a single
`reactions::post_completion::react_to_completions` coordinator both paths route
through (FEAT-010); see "Post-completion reactions (converged)" below.

## Post-completion reactions (converged)

`reactions::post_completion::react_to_completions` is the single home for the
three completion-driven reactions that fire after the shared pipeline has
flipped this iteration/wave's tasks to `done`:

| # | Reaction | Sequential | Wave |
|---|---|---|---|
| #8 | Wrapper-commit (commit on a task's behalf when Claude couldn't) | `wrapper_commit = true` | `wrapper_commit = false` (slot merge-back already carries the commit) |
| #9 | External-git completion shadow (`git_reconcile::reconcile_external_git_completions`) | ✓ | ✓ |
| #10 | Human-review trigger for `requires_human` completions | ✓ | ✓ **(behavior addition)** |

**Input-driven, not timestamp rediscovery.** The coordinator consumes the
**already-computed** `completed_ids` set — the ids the shared pipeline + the
post-merge slot reconcile (`apply_post_merge_reconcile` →
`reconcile_merged_slot_completions`) flipped to `done` this iteration/wave. It
does NOT re-query "everything completed since an epoch". This is what preserves
intra-wave ordering: the post-merge reconcile result feeds `completed_ids`
BEFORE the external-git shadow runs inside the coordinator, and human review
fires only for `requires_human` ids in that set (∪ any the external-git shadow
newly discovers). A `requires_human` task that completed out-of-band and is
absent from the set is never reviewed.

**Wave gains human review (intentional behavior addition).** Before FEAT-010 the
wave path had no human-review trigger at all; a `requires_human` (e.g. CLARIFY)
task a slot completed never spawned its review. It now does. `react_to_completions`
runs once per wave at the post-merge-back step (after `apply_post_merge_reconcile`,
before the terminal checks), so it can fire on a **partial wave** — one that
reaches the post-completion step with a sibling slot still `in_progress` (it
didn't complete this wave) or with a sibling's ephemeral branch unmerged (a
failed merge-back). Because the reaction is input-driven it reviews ONLY the
completed `requires_human` ids and leaves every `in_progress` / unmerged sibling
untouched; the interactive review session can block in that partial state while
sibling work is still outstanding. This is deliberate — a completed CLARIFY
should unblock its dependents without waiting for the whole wave to drain. (The
rate-limit / transient-backend `WaitedAndRetry` reactions early-return BEFORE
merge-back, so a wave that bails on a rate limit defers its completed tasks'
reviews to a later wave; that is the rate-limit retry path's existing contract,
not a regression — the wave never reviewed at all pre-FEAT-010.)

**Test seam (inner/outer split).** `react_to_completions` (production) builds the
real review action — `signals::handle_human_review` over stdin, then
`prd_reconcile::mutate_prd_from_feedback` on feedback (applied AFTER the inner
returns, so the inner's `&mut Connection` is free) — and delegates to
`react_to_completions_inner`, which takes the review action as an injected
`ReviewFn` seam (hermetic: no stdin, no subprocess; pinned by
`tests/reaction_parity.rs`). The param struct `PostCompletionParams` is
destructured exhaustively (no `..`) — the CONTRACT-001 single-home parity lock.

**Single-home lock.** The relocated leaf `orchestrator::trigger_human_reviews`
carries `#[deprecated]` (now a timestamp-query shim that delegates to
`react_to_completions`); the three engine files
(`iteration.rs`/`wave_scheduler.rs`/`slot.rs`) carry `#![deny(deprecated)]`, so
copy-pasting human review back into one path fails to compile.

## Drained-queue classification (sequential ↔ wave parity)

When no *schedulable* task can be selected, both execution paths decide the
loop-end verdict through ONE helper, `engine::classify_drained_queue`, so they
cannot drift on "what counts as complete vs stuck":

- **Clean drain** — only `done`/`irrelevant` remain → exit 0,
  `RunStatus::Completed`, reason "all tasks complete".
- **Stuck drain** — at least one `blocked` and/or `skipped` row remains with no
  schedulable work → exit 1, `RunStatus::Aborted`, reason names the counts +
  a `task-mgr review` hint. **`skipped` is treated as unfinished work, not a
  clean success** (deliberate product decision — neither path may claim
  completion while deferred work is outstanding).
- **Not drained** — any `todo`/`in_progress` row exists →
  `count_remaining_active_tasks != 0` → returns `None`; the caller keeps
  looping / recovering.

Call sites:
- **Wave**: `handle_no_eligible_tasks` (empty group) AND the all-complete exit
  at the bottom of `run_wave_iteration` (guarded by `agg.any_completed`).
- **Sequential**: the clean-complete check in `run_iteration`'s `build_prompt`
  `Ok(None)` arm, plus a drained-but-stuck short-circuit in `run_loop`'s
  `NoEligibleTasks` branch (exits immediately with the named reason instead of
  spinning to the 3-iteration stale-abort threshold).

**Empty-group ≠ stale.** Before counting an empty wave selection toward the
stale tracker, `handle_no_eligible_tasks` first runs the same auto-recovery the
sequential path does (`reconcile_passes_with_db` + `recover_in_progress_for_prefix`)
— a task a finished slot left stranded in `in_progress` is reset to `todo` and
retried next wave WITHOUT incrementing stale. Only a genuinely stuck queue
(nothing schedulable, nothing recoverable, no blocked/skipped terminal) drives
the stale counter. This closed a bug where a fully-completed PRD aborted with
exit 1 "no eligible tasks after 3 consecutive stale iterations".

**`archived_at IS NULL` is mandatory** in `count_remaining_active_tasks` and
`count_tasks_in_status` — archiving stamps `archived_at` on prefix-matched rows
regardless of status, so an archived row would otherwise mis-classify the drain
(locked by `archive.rs::test_archived_tasks_invisible_to_status_count_query`).

## Slot merge-back conflict resolution

When parallel-slot waves finish, `merge_slot_branches_with_resolver` (in
`src/loop_engine/worktree.rs`) runs `git merge --no-edit` from slot 0 for each ephemeral
slot branch. On a non-zero exit it lists the conflicted files and invokes a `MergeResolver`
(callback seam, `pub(crate) trait`); the engine wires `ClaudeMergeResolver` from
`src/loop_engine/merge_resolver.rs`, which spawns Claude in slot 0's already-conflicted
worktree (`PermissionMode::Auto`, `working_dir = slot0_path`, 600s timeout) with a prompt
that explicitly prohibits push, branch deletion, hard reset outside the merge, and history
rewrites. The resolver's `Resolved` claim is **never trusted**: the caller re-inspects
MERGE_HEAD and HEAD post-spawn and downgrades a lying resolver to `failed_slots` with a
forced `git reset --hard pre_merge_head`. `SlotFailureKind::ResolverAttempted` vs
`PreResolver` lets engine.rs pick the right warning text without string-sniffing.

Note: merge resolution is intentionally NOT part of the shared
`iteration_pipeline` (see "Iteration pipeline (shared)" above) — it requires
working-tree state owned by `run_wave_iteration`, not the per-slot
post-Claude processing block.

### Gitignored progress files (FEAT-001, slot-merge-preflight PRD)

The per-PRD progress file `tasks/progress-<prefix>.txt` is the most common
source of slot-0 dirtiness — slot 1 commits to it on every wave iteration —
and git's merge precondition aborts when slot 0 has uncommitted local
changes to a file the incoming merge would touch (`"Your local changes to
the following files would be overwritten by merge"`, non-zero exit with **no
conflict markers**). The `ClaudeMergeResolver` then correctly short-circuits
because there's nothing to act on, and the slot's commits get stranded.
`task-mgr init` writes/refreshes a managed marker-block in `.gitignore`
covering `tasks/progress-*.txt` and runs a one-time `git rm --cached`
migration so existing repos drop the tracked file from the index without
losing its on-disk content. See `src/commands/init/mod.rs::ensure_progress_gitignore`
and `untrack_progress_files`. **The `git rm --cached` (NOT bare `git rm`)
distinction is load-bearing** — bare `git rm` would delete the file on disk
and lose the operator's loop history.

### Stash-based preflight (FEAT-003 / FEAT-004, slot-merge-preflight PRD)

For residual non-progress dirtiness (log files, build artifacts the project
hasn't gitignored, stray test fixtures), `merge_slot_branches_with_resolver`
runs a stash-based preflight before every per-slot `git merge --no-edit`.
`prepare_slot0_for_merge` stashes everything dirty (tracked + untracked)
under a deterministic tag `task-mgr-slot-{slot}-{run_id}-{epoch_ms}`;
`cleanup_preparation` pops after the merge attempt — successful or not.
Pop conflicts are warned-and-continued (stash retained on stack for operator
inspection), and once `count_stashes_with_prefix` exceeds
`ProjectConfig.slot_stash_limit` (default 5) on the same slot, the slot is
demoted to `failed_slots(PreResolver)` and the FEAT-002 consecutive-merge-fail
halt threshold trips. **Cleanup is structurally guaranteed to run exactly
once per slot** — `run_slot_merge_attempt` was extracted as a helper so
every exit path (rev-parse failure, spawn failure, clean success, any
conflict-handling branch) goes through the same `cleanup_preparation` call.
No auto-commit — that would pollute base-branch history with `chore(progress)`
commits. Stash tags include `run_id` so concurrent loops don't poach each
other's stashes. See `src/loop_engine/worktree.rs::prepare_slot0_for_merge`
and `cleanup_preparation`. `merge_resolver.rs:278` annotates the
"no conflicts reported, refusing to spawn" diagnostic with a preflight
pointer so the next operator who hits a regression knows where to look.

### Reconcile auto-recovery (FEAT-005, slot-merge-preflight PRD)

`reconcile_stale_ephemeral_slots` now accepts an optional
`AutoRecoveryConfig` (model / effort / claude_timeout / signal_flag /
db_dir / run_id / stash_limit). When `Some`, the function attempts an
automatic merge-back of each `CleanUnmerged` stale ephemeral at loop
startup using the same preflight + `ClaudeMergeResolver` path live waves
take — `prepare_slot0_for_merge` → `git merge --no-edit` →
`ClaudeMergeResolver` on conflict → `cleanup_preparation` → `git worktree
remove` + `git branch -D` on success. `slot0_path` is `project_root`
because reconcile runs **before** `ensure_slot_worktrees` — slot 0 IS the
loop's main worktree at startup. Per-branch failures keep the branch in
`unmerged` and fall through to the existing `halt_threshold` abort, with
the message annotated `(auto-recovery attempted and failed for:
<branches>)` so the operator sees which branches the resolver attempted
vs. didn't. When `None`, behavior is byte-for-byte identical to the
pre-FEAT-005 abort path. **Out of scope: case-4 dirty stale worktrees**
still always abort regardless of `auto_recovery` — auto-recovery never
runs on a worktree that has uncommitted work, by design.
Test-injection seam: `reconcile_stale_ephemeral_slots_inner` (pub(crate))
accepts an explicit `&dyn MergeResolver` so unit tests exercise the
resolver-Failed branch without spawning Claude. Engine wiring lives in
`src/loop_engine/engine.rs` at the FEAT-005 reconcile call site (Step 9.5)
— it builds a one-off `AutoRecoveryConfig` from `project_default_model` /
`project_config.merge_resolver_effort` / `merge_resolver_timeout_secs` /
`slot_stash_limit` with a fresh `SignalFlag` and a synthetic
`"startup-reconcile"` run-id (real run-id allocation happens later in
Step 12 `run_cmd::begin`).

## Parallel-slot scheduling

Five layered defenses harden parallel-slot execution against the cascade
that produced the mw-datalake incident (a 2-slot loop whose slot-1
merge-back failed on iteration 1 with a recomputed-slot-path ENOENT,
silently kept launching new waves, and eventually diverged 22-vs-18
commits with un-merged `Cargo.lock` modifications on each side).

### 1. Slot path threading (cause-fix)

`merge_slot_branches_with_resolver` (`src/loop_engine/worktree.rs`) takes
`slot_paths: &[PathBuf]` and uses `slot_paths[0]` as slot 0's path, never
recomputing it via `compute_slot_worktree_path(project_root, branch, 0)`.
The recomputation diverges when the loop runs from inside the matching
worktree — `compute_slot_worktree_path` re-derives a path under
`{parent(project_root)}/{slot0_name}-worktrees/...` while the actual slot 0
worktree IS the project root. Engine threads the paths returned by
`ensure_slot_worktrees` through `WaveParams::slot_worktree_paths`.

`compute_slot_worktree_path` is still correct for slots 1+ inside
`merge_slot_branches_with_resolver` and for `cleanup_slot_worktrees` — only
the slot 0 lookup was wrong.

### 2. Consecutive-merge-fail halt threshold

`ProjectConfig::merge_fail_halt_threshold` (default `2`) caps consecutive
parallel-slot merge-back failure waves before the engine halts. Single
failures are recoverable (next wave gets a clean slate from the
resolver); two-in-a-row indicate a cascading state. The reset/halt
contract is implemented once in
`apply_merge_fail_reset_and_halt_check` (`src/loop_engine/wave_scheduler.rs`)
and called from the wave-loop boundary — sequential-loop and wave-loop
paths must not re-implement it.

Threshold semantics:
- `0` — never halt (legacy "log and continue" behavior, preserved
  bit-for-bit on the same forced-fail input)
- `1` — halt on any merge-back failure
- `2` (default) — halt after two consecutive merge-back failure waves

### 3. Implicit-overlap baseline + buildy heuristic

`select_parallel_group` in `src/commands/next/selection.rs` serializes
shared-infra contention through a single synthetic `__shared_infra__`
slot per wave. A candidate "claims" the synthetic slot when ANY of:

- (a) some `touchesFiles` entry's basename matches the union of
  `IMPLICIT_OVERLAP_FILES` (Cargo.lock, uv.lock, package-lock.json,
  go.sum, etc. — Rust/Python/JS/Go ecosystems out-of-the-box) ∪
  `ProjectConfig::implicit_overlap_files` ∪
  `PrdFile::implicit_overlap_files` (project + PRD lists EXTEND, do not
  replace, the baseline);
- (b) the task id matches `BUILDY_TASK_PREFIXES` (`FEAT`, `REFACTOR`,
  `REFACTOR-N`, `CODE-FIX`, `WIRE-FIX`, `IMPL-FIX` — superset of
  `SPAWNED_FIXUP_PREFIXES`) via the same token-aware
  `id_body_matches_prefix` matcher used by the soft-dep guard (no
  parallel matcher);
- (c) the task's `claims_shared_infra` field (Option<bool>, migration
  v19) is `Some(true)` — explicit override.

`Some(false)` overrides BOTH (a) and (b); `None` falls through to (a) ∨
(b). This deliberately changes the empty-`touchesFiles` parallelism
baseline — buildy-prefix tasks claim infra even with no listed files.

### 4. Cross-wave file affinity (un-merged ephemeral branches)

`select_parallel_group` accepts `ephemeral_overlay: &[(branch, files)]`
listing files claimed by un-merged ephemeral slot branches from prior
waves. A candidate is deferred when its `touchesFiles` overlap with any
ephemeral branch's claimed set — preventing the same file from being
modified on two divergent branches across waves.

Engine builds the overlay via `worktree::list_unmerged_branch_files`
(`git diff --name-only {base}...{ephemeral}`) for each `{branch}-slot-N`
ephemeral that hasn't merged back. Empty overlay → identical results to
the pre-FEAT-004 implementation (strict superset filter).

**Deadlock guard**: when the greedy pass yields an empty group AND every
candidate's only overlap was ephemeral, `ParallelGroupResult::ephemeral_block_diagnostics`
is populated with named blocking branches. Engine treats this as
equivalent to `failed_merges` non-empty so the FEAT-002 reset/halt
contract fires and the loop halts cleanly with named branches instead
of spinning until stale-iteration abort.

### 5. Stale ephemeral branch hygiene at startup

`reconcile_stale_ephemeral_slots` (`src/loop_engine/worktree.rs`) runs
once at loop startup BEFORE `ensure_slot_worktrees`. For each
`{branch}-slot-N` left over from a prior crash:
- Clean (worktree dir gone, no un-merged commits) → branch deleted, no
  abort.
- Un-merged commits exist AND `halt_threshold > 0` → abort startup
  (the operator must reconcile before the new loop can run).
- Dirty working tree (uncommitted changes) → abort regardless of
  `halt_threshold` (no automated cleanup of unsaved work).

Branch-name shape uses `ephemeral_slot_branch(branch, slot)` (slot 0 is
the loop's base branch; slots 1+ are `{branch}-slot-{N}`). Idempotent —
running twice produces identical state on the second pass.

**Slot-0 SAFETY GUARD (load-bearing)**: `classify_ephemeral_branch`
returns `Err` when the parsed slot suffix is `0`, and
`list_ephemeral_slot_branches` filters `slot > 0`. Production code never
creates a `{branch}-slot-0` ref (slot 0 reuses the base branch directly
in `ensure_slot_worktrees`), but a stray ref from a buggy past version,
manual operator action, or recovery artifact would otherwise classify
as `CleanMerged` with `worktree_path` pointing at the **loop's main
worktree** — `compute_slot_worktree_path(_, branch, 0)` short-circuits
to `compute_worktree_path(_, branch)`. The downstream
`delete_merged_ephemeral` would then `git worktree remove` the loop's
running worktree. Guard MUST hold; never broaden the glob without
adding the slot==0 rejection at the same boundary.

### 6. Run-level config caching (restart required for mid-loop edits)

`ProjectConfig` and the PRD-side `implicit_overlap_files` override are
loaded ONCE at `run_loop` startup and threaded through
`WaveIterationParams` (`prd_implicit_overlap_files`, `project_config`).
`run_wave_iteration`, `apply_merge_fail_reset_and_halt_check`, and the
merge-back resolver setup all read from the cached references — never
call `read_project_config` or `read_prd_implicit_overlap_files` from
inside a wave hot path.

**Mid-loop edits to `.task-mgr/config.json` or the PRD JSON do NOT take
effect** — operators must restart the loop to apply config changes.
Same restart-required semantics every other run-scoped knob already
has (`parallel_slots`, `default_model`, `merge_resolver_*`).
Documenting this here so the next "I changed config and nothing
happened" question has a quick answer.

### 7. Failed-merge accounting: `Vec<FailedMerge>`, not parallel arrays

`WaveOutcome.failed_merges: Vec<FailedMerge>` carries `(slot, task_id)`
as a single struct so the slot/task pairing is a type-level invariant.
The earlier shape (parallel `Vec<usize>` + `Vec<Option<String>>` held
lockstep by rustdoc) was correct but implicit; mismatched lengths would
have silently truncated under `zip`. Don't reintroduce parallel arrays
here, and apply the same shape preference for any future
"slot + per-slot data" aggregation.

**Synthetic-deadlock sentinel (`SYNTHETIC_DEADLOCK_SLOT = usize::MAX`)**:
`handle_ephemeral_deadlock` inserts one entry with this slot index when
every blocking ephemeral branch had a malformed suffix
(`synth_slots.is_empty() && !diagnostics.is_empty()`). Without it,
`failed_merges` would be empty, `apply_merge_fail_reset_and_halt_check`
would reset `consecutive_merge_fail_waves` to 0, and the deadlock
guard would silently spin until the stale-iteration tracker aborted —
defeating the FEAT-002 cascade halt. The diagnostic step special-cases
the sentinel to print `<malformed deadlock blocker>` instead of
synthesizing `{branch}-slot-18446744073709551615`.

General pattern: **any synthesis that translates "we observed a
problem" into "produce a failure record" must always emit at least
one record, even if the upstream parsers all rejected the input** —
otherwise downstream "is_empty" checks invert the safety guarantee.

## LLM runner dispatch

`dispatch` in `src/loop_engine/runner.rs` is the single spawn boundary. It
routes a `RunnerOpts` + `RunnerKind` pair to the matching backend
(`ClaudeRunner` or `GrokRunner`) via a static-dispatch `match` — no
`Box<dyn LlmRunner>` on the hot path.

### Capability surface

`RunnerCapability` (an exhaustive `pub(crate)` enum in `runner.rs`) is the
typed surface for expressing what a runner can and cannot do. Adding a new
capability-asymmetric feature MUST go through this enum — never a naked
`RunnerKind` identity check dressed as a capability test.

Key invariants:

- **`LlmRunner::supports` default returns `false`** (fail-closed). A new
  runner that forgets to override `supports` is treated as "supports
  nothing", so every capability-driven call against it is rejected at the
  dispatch boundary rather than silently no-op'ing runner flags.
- **Production runners use exhaustive matches** (no `_ =>` wildcard arm) in
  their `supports` impl. Adding a new `RunnerCapability` variant is a
  compile error in every production impl simultaneously — the runner owner
  must make a deliberate per-variant decision before the code can compile.
- **`dispatch` is fail-closed**: before the spawn `match`, `enforce_capabilities`
  walks the `CHECKS` registry table. For each `(RunnerCapability, field_check,
  field_name)` row, if the field is set to a non-default value AND the chosen
  runner's `supports(cap)` returns `false`, dispatch returns
  `TaskMgrError::UnsupportedRunnerCapability` immediately — no subprocess is
  launched. Field presence drives enforcement; value semantics are the
  backend's concern.
- **`CHECKS` is the single source of truth** mapping `RunnerOpts` fields to
  `RunnerCapability` variants. Every enforced capability has exactly one row.
  A completeness-guard test (`checks_table_covers_every_capability_variant`)
  asserts full coverage — a new variant without a matching row fails at
  unit-test time.

Current capabilities and their production support matrix:

| Capability | Claude | Grok |
|---|---|---|
| `Effort` | ✓ | ✓ |
| `StreamJson` | ✓ | ✓ |
| `Pty` | ✓ | ✗ |
| `DisallowedTools` | ✓ | ✓ |
| `TitleArtifactCleanup` | ✓ | ✗ |

`Pty` and `TitleArtifactCleanup` are the asymmetric capabilities today.
`Pty` maps to `use_pty` (Node.js line-buffering workaround, Claude-only).
`TitleArtifactCleanup` maps to `cleanup_title_artifact` (ai-title jsonl
session-leak workaround for Claude Code 2.1.110; Grok has no equivalent).

## Status mutations — use TaskLifecycle

All `tasks.status` writes inside `loop_engine/` go through `TaskLifecycle`
verbs. Do **not** add raw `UPDATE tasks SET status …` SQL here.

| Context | Verb | Constructor |
|---|---|---|
| Loop `<task-status>` tag dispatch | `apply()` | `TaskLifecycle::with_run(conn, run_id).with_prd_sync(path, prefix)` |
| Slot pre-claim (wave) | `try_claim()` | same connection, no run context needed |
| Stuck in-progress reset (stale sweep, slot release) | `recover_in_progress_for_prefix()` | `TaskLifecycle::with_run(conn, run_id)` |
| Consecutive-failure auto-block | `auto_block_after_failures()` | `TaskLifecycle::with_run(conn, run_id)` |
| Overflow rung reset / provider promote | `resurrect_for_iteration()` | `TaskLifecycle::with_run(conn, run_id)` |

For the full site→verb audit table and source-allowance matrix see
[`src/lifecycle/CLAUDE.md`](../lifecycle/CLAUDE.md).

## Touchpoints

| Concern | File | Symbol |
| --- | --- | --- |
| Status mutation SSoT | `src/lifecycle/mod.rs` | `TaskLifecycle`, six public verbs |
| Outer loop entry point | `src/loop_engine/orchestrator.rs` | `run_loop`, `on_run_completed` |
| Sequential iteration body | `src/loop_engine/iteration.rs` | `run_iteration` |
| Wave scheduling + merge-back | `src/loop_engine/wave_scheduler.rs` | `run_wave_iteration`, `run_parallel_wave` |
| Per-slot lifecycle + result | `src/loop_engine/slot.rs` | `run_slot_iteration`, `process_slot_result` |
| Per-task recovery cluster | `src/loop_engine/recovery.rs` | `check_crash_escalation`, `check_override_invalidation`, `handle_task_failure` |
| Slot path threading | `src/loop_engine/worktree.rs` | `merge_slot_branches_with_resolver` |
| Halt threshold contract | `src/loop_engine/wave_scheduler.rs` | `apply_merge_fail_reset_and_halt_check` |
| Failed-merge struct | `src/loop_engine/engine.rs` | `FailedMerge` |
| Deadlock sentinel | `src/loop_engine/wave_scheduler.rs` | `SYNTHETIC_DEADLOCK_SLOT` |
| Implicit overlap baseline | `src/commands/next/selection.rs` | `IMPLICIT_OVERLAP_FILES`, `BUILDY_TASK_PREFIXES` |
| Cross-wave overlay | `src/loop_engine/worktree.rs` + `src/commands/next/selection.rs` | `list_unmerged_branch_files`, `ephemeral_overlay` parameter |
| Startup hygiene + slot-0 guard | `src/loop_engine/worktree.rs` | `reconcile_stale_ephemeral_slots`, `classify_ephemeral_branch` |
| Run-level config caching | `src/loop_engine/engine.rs` | `WaveIterationParams::project_config`, `prd_implicit_overlap_files` |
| Overflow recovery ladder | `src/loop_engine/reactions/post_output.rs` + `src/loop_engine/overflow.rs` | `handle_overflow` (coordinator, owns the ladder), `handle_prompt_too_long` (`#[deprecated]` shim), `sanitize_id_for_filename`, `rotate_dumps_keep_n`, `RecoveryAction::FallbackToProvider` |
| LLM runner dispatch | `src/loop_engine/runner.rs` + `src/loop_engine/engine.rs` | `RunnerKind`, `dispatch`, `ClaudeRunner`, `GrokRunner`, `resolve_effective_runner` |
| Capability surface | `src/loop_engine/runner.rs` | `RunnerCapability`, `LlmRunner::supports`, `enforce_capabilities`, `CHECKS` |
| Provider routing | `src/loop_engine/model.rs` | `Provider`, `provider_for_model` |
| Operator escape valve | `src/loop_engine/recovery.rs` | `check_override_invalidation` |
| Overflow original model snapshot | `src/loop_engine/engine.rs` | `IterationContext::overflow_original_task_model` |
| Fallback runner config | `src/loop_engine/project_config.rs` | `FallbackRunnerConfig`, `check_fallback_runner_binary` |
| Primary runner config + routing | `src/loop_engine/project_config.rs` | `PrimaryRunnerConfig`, `RunnerSpec` |
| Primary runner model routing | `src/loop_engine/model.rs` | `primary_runner_match`, `resolve_task_model`, `ModelResolutionContext` |
| Auto-review launch boundary | `src/loop_engine/auto_review.rs` | `maybe_fire`, `maybe_fire_inner`, `ProcessLauncher` |
| Shared post-Claude pipeline | `src/loop_engine/iteration_pipeline.rs` | `process_iteration_output` |
| Merge resolver | `src/loop_engine/merge_resolver.rs` | `ClaudeMergeResolver`, `MergeResolver` trait |
| Stash preflight | `src/loop_engine/worktree.rs` | `prepare_slot0_for_merge`, `cleanup_preparation`, `run_slot_merge_attempt` |
| Post-merge slot reconcile | `src/loop_engine/git_reconcile.rs` | `reconcile_merged_slot_completions` |
| Post-completion reactions (#8/#9/#10) | `src/loop_engine/reactions/post_completion.rs` | `react_to_completions` (coordinator, both paths), `react_to_completions_inner` (hermetic core), `PostCompletionParams`, `ReviewFn`; relocated leaf `orchestrator::trigger_human_reviews` (`#[deprecated]` shim) |
