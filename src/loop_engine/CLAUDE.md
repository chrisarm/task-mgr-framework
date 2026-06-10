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

## models + routing config and capability tiers (FR-001)

The single provider-first surface is the `models` + `routing` block (merged onto
`ModelsConfig::builtin_default`). Legacy `defaultModel` / `reviewModel` /
`primaryRunner` / `fallbackRunner` / `baselineTierRoutes` are hard-broken (detected
at preflight; migration via `task-mgr models init`).

`models` supplies:
- `primaryProvider`, `anchor` (default "standard")
- per-provider `enabled` + sparse `tiers` (kebab keys: cheapest|cost-efficient|standard|frontier → model or null) + per-difficulty `effort` tables + optional `fallback` (different enabled provider)

`routing` supplies role split (`taskClasses`), prefix forcing (`byIdPrefix`), and
spillover policy for quota blackouts.

See `src/loop_engine/model.rs` (CapabilityTier, anchored_tier, resolve_models_config,
resolve_execution_plan) and `project_config.rs` (validate + merge).

### Capability tiers + anchor window

Provider-neutral ordered ladder (`Cheapest < CostEfficient < Standard < Frontier`).
Anchor (default standard) + difficulty offset (low=-1, med=0, high=+1; from the
single `difficulty_rank` / `normalize_difficulty`) produces the starting tier,
clamped at ends (never wraps).

- `anchor=standard` (default): low → cost-efficient, med → standard, high → frontier
- Sparse per-provider ladders: `model_for` / escalate walk only defined rungs (gaps skipped; distance tie prefers cheaper).

Tier membership and reverse lookup are **config exact-match** (`tier_of` after stripping `[1m]` suffix). Substring matching is dead (fable contains no "opus").

### Routing precedence (highest → lowest)

Implemented in `resolve_execution_plan` (the single decision site; `ExecutionPlan`
carries final provider/tier/effort for prompt + runner dispatch):

1. Explicit task `"model"` (from DB / prd_default) — wins, bypasses all routing.
2. `routing.byIdPrefix` (dash-boundary token match on ID body post-prefix-strip). Optional forced tier; otherwise anchor window for difficulty. Beats the built-in review→frontier force (pinned by `precedence_matrix::byidprefix_route_beats_review_class_force`).
3. `routing.taskClasses` (implementation/planning/review keys). The review class (`is_frontier_class` after 8-hex prefix strip: CODE-REVIEW-*, MILESTONE-FINAL, REVIEW-*; REFACTOR-REVIEW-* excluded) forces the Frontier tier built-in at this rung — not redefinable via config, never spillover-eligible. `providerPreference` + `byDifficulty` can select provider; falls back to anchor window tier if no forceTier.
4. QUOTA_BLACKOUT reroute (default-path tasks only): if the would-be provider is blacked out *and* task is spillover-eligible (difficulty ≤ `spillover.maxDifficulty` or max unset), tier-preserving reroute to first enabled non-blacked provider. Beats plain anchor window.
5. ANCHOR_WINDOW (floor): `models.primaryProvider` + `anchored_tier(anchor, difficulty)`.

After route selection, finalize:
- model = `models.model_for(final_provider, final_tier)` (sparse clamp; null = omit -m flag)
- effort = `models.effort_for(final_provider, difficulty)` (from *final* provider's table)

Codex is **route-only**: only reachable via explicit `byIdPrefix` or `taskClasses` routes with `provider: "codex"`. Never inferred from any model string (token-equality `provider_for_model` never yields Codex).

### Blackout channel contract (FEAT-008)

`IterationContext::provider_blackouts` (a `BlackoutState`) is a SEPARATE, EPHEMERAL channel from `runner_overrides`:

- Blackouts: per-pass, self-expiring, never persisted (no DB/serde). Written only by account rate-limit reaction (`reactions::account`). Read by spawn resolver (`resolve_execution_plan` for QUOTA_BLACKOUT reroute), quota deferral, and excluded-id computation. Cleared on restart by design.
- `runner_overrides` (and `promote_once`): permanent (cross-provider at most once per run) for the overflow-ladder rung-4 fallback. (The RuntimeError cross-provider escape valve was removed in REFACTOR-006; the overflow rung-4 pivot is now the only `runner_overrides` writer via `promote_once`.) Cleared on restart.

**Invariants**:
- `promote_once` / every `runner_overrides` writer MUST NOT read or write `provider_blackouts`.
- The blackout reroute path MUST NOT write `runner_overrides` (that would permanently pin the task to the spillover provider for the remainder of the run, violating the escape-valve "once" contract).
- The two are derived independently per pass; collapsing them produces the exact failure mode guarded by `blackout_reroute_leaves_runner_overrides_untouched`.

Rung-4 provider fallback (tier-preserving) now uses `providers.<source>.fallback` from the `models` config (resolved into `ResolvedModelsConfig`), not the old `fallbackRunner` surface.

**Stream-C (grok child stderr capture, FEAT-006)**: Raw grok stderr (telemetry, HTML errors, etc.) is captured by a sniffer thread to `.task-mgr/logs/<prefix>-<run>-<slot>-iterN-grok-stderr.log` (path announced via `ui::emit`; never teed to console). The capture file is the permanent artifact for post-run inspection. Classifier-based extraction/surfacing of notable lines from these files into the operator or learnings flow is deferred to FEAT-014 (intentionally decoupled).

## Reaction framework (shared)

The loop engine has two execution paths — **sequential**
(`iteration.rs::run_iteration` driven by `orchestrator.rs::run_loop`) and
**parallel-wave** (`wave_scheduler.rs::run_wave_iteration` + `slot.rs`). Two
WS-3 carves split the largest of those modules along behavior-neutral seams:
`run_loop`'s linear startup phase (Steps 1–16) now lives in
`startup.rs::initialize_loop` (returns a `LoopInitContext` the orchestrator
destructures once), and `wave_scheduler`'s non-hot-path wave-decision trio
(`wave_preflight_check`, `handle_no_eligible_tasks`, `handle_ephemeral_deadlock`)
now lives in `wave_orchestration.rs`. Both moves are byte-for-byte relocations —
`run_loop` and `run_wave_iteration` stay the entry points and call across the
new module boundary. Every
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

| Rung | Action | Claude runner | Grok runner | Codex runner |
|---|---|---|---|---|
| 1 | Downgrade effort (`xhigh → high`) | ✓ | ✓ | — |
| 2 | `escalate_below_ceiling` — up one DEFINED tier on the SOURCE provider's ladder (`haiku → sonnet → opus → fable`); `None` at the ceiling | ✓ | — | — |
| 3 | `to_1m_model` — append `ONE_M_SUFFIX` at the ceiling (`fable → fable[1m]`, suffix-append, Claude-only) | ✓ | — | — |
| 4 | **FallbackToProvider** — tier-preserving cross-provider pivot to `providers.<source>.fallback` | → fallback provider | → fallback provider | → fallback provider |
| 5 | Block (no further recovery) | ✓ | ✓ | ✓ |

Rung 2 walks the SOURCE provider's capability ladder (config exact-match via
`model::escalate_below_ceiling`, no substring matching). A single-rung provider
(Grok, Codex) already sits at its ceiling, so rung 2 is a no-op for it and the
ladder advances. Rung 3 (`to_1m_model`) is Claude-only — 1M context is a
Claude-only capability; it suffix-appends `ONE_M_SUFFIX` to ANY Claude tier
(opus[1m], fable[1m]), gated on the source provider being Claude. Codex v1 has
no effort flag; with no `providers.codex.fallback` set it falls through to block.

**Rung 4 detail — `FallbackToProvider`** (FEAT-007, config-derived): the pivot
target is `providers.<source>.fallback` from the provider-first `models` config
(`ResolvedModelsConfig::fallback_provider`), and the target model is
**tier-preserving** — `model_for(target_provider, tier_of(source, model))`. A
provider gains a rung-4 pivot iff its `fallback` key is set (Codex included);
`validate_models_config` guarantees the fallback is a different, enabled
provider, so a self-pivot or disabled target can never reach the rung. The
legacy `fallbackRunner` / `primaryRunner.claudeFallbackModel` surfaces no longer
drive this rung.

`RunnerKind::Codex` derives NO fallback from model strings or from
`primaryRunner`. Its only overflow rung-4 pivot is the explicit, config-derived
`providers.codex.fallback` (FEAT-007) — set it and Codex pivots tier-preservingly
like any other provider; leave it unset and Codex overflow falls through to
block. The RuntimeError retry path (separate from overflow) receives the
executed runner explicitly via `IterationResult.effective_runner` /
`SlotResult.effective_runner` and uses it ONLY to short-circuit Codex out of the
Claude tier ladder (a Codex task must not normalize a NULL model to the Sonnet
baseline and escalate onto Opus). It performs NO cross-provider promotion:
REFACTOR-006 deleted the Claude→Grok / Grok→Claude / Codex→Claude RuntimeError
promotion arms (they were unreachable once preflight hard-rejected
`primaryRunner` / `fallbackRunner`). A repeatedly-RuntimeError-crashing task now
climbs the same-provider Claude tier ladder (Claude tasks only) and then
auto-blocks; cross-provider rescue exists only on the overflow rung-4 pivot.

In both cases, the rung writes the target model to the `tasks.model` DB column
AND inserts matching entries into `ctx.runner_overrides` / `ctx.model_overrides`
atomically. Idempotency guard: a task already carrying a promotion override
(in either direction) skips this rung and falls through to rung 5. The DB
UPDATE AND the override-map inserts MUST run together — otherwise
model resolution on the next iteration silently shadows the override.

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

**Provider routing — `model::provider_for_model` +
`EffectiveRunnerInput.provider_hint`**: `provider_for_model` classifies a
model id as `Provider::Claude` or `Provider::Grok` via **token equality on `-`
splits of the lowercased id**, returning `Provider::Grok` iff *some token is
exactly* `"grok"`. It never returns `Provider::Codex`; Codex is routed only by
explicit provider intent from the resolved `ExecutionPlan.provider`, carried to
the spawn site via `EffectiveRunnerInput.provider_hint`. Substring matching
(`.contains("grok")`) is explicitly prohibited because it would mis-route Groq
Inc. models (`groq-llama-3`, etc.) to the xAI Grok runner. Every other input —
`None`, the empty string, unknown model ids, Codex-looking model ids, the
Claude constants, and Groq family ids — falls through to `Provider::Claude`.
Total function: every `Option<&str>` produces some `Provider`; never panics.
`resolve_effective_runner` (in `engine.rs`) is the SINGLE source of truth for
the spawn-site dispatch discriminant: `runner_overrides` first, then
`provider_hint`, then `provider_for_model(model)`. Re-deriving the formula
independently is explicitly prohibited.

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
  variant-specific siblings (e.g. `{"action": "escalate_tier", "new_model": "..."}`).
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
consecutive-failure escalation runs inside the same DB transaction that
increments `consecutive_failures` and (optionally) auto-blocks. The pattern is:
inner helper performs DB writes only and returns an `Option<PendingPromotion>`;
the caller applies it via `apply_pending_promotion` **only after `tx.commit()?`
returns Ok** — so a rolled-back DB can never leave the in-memory ctx claiming a
promotion. Direct callers (tests, sequential non-transactional paths) use the
convenience wrapper `escalate_task_model_if_needed` which applies immediately.
Same shape applies to any future "in-memory state mutation paired with DB write
inside a transaction" — split inner-helper / apply-pending / defer-until-commit.

REFACTOR-006 deleted the RuntimeError cross-provider promotion arms, so
`escalate_task_model_if_needed_inner` no longer produces a `PendingPromotion`
(it always returns `None` for that slot; only the same-provider Claude tier
escalation remains). The deferred-apply skeleton is retained intact because
`apply_pending_promotion` is the sole non-test reader of [`PendingPromotion`]'s
fields, and [`PendingPromotion`] is still constructed by `promote_once` for the
LIVE overflow rung-4 pivot (`reactions::post_output`). `promote_once` /
deferred-apply / the override-invalidation escape valve are shared with that
live pivot and stay untouched.

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
(`ClaudeRunner`, `GrokRunner`, or `CodexRunner`) via a static-dispatch `match` — no
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

| Capability | Claude | Grok | Codex |
|---|---|---|---|
| `Effort` | ✓ | ✓ | ✗ |
| `StreamJson` | ✓ | ✓ | ✓ |
| `Pty` | ✓ | ✗ | ✗ |
| `DisallowedTools` | ✓ | ✓ | ✗ |
| `TitleArtifactCleanup` | ✓ | ✗ | ✗ |

`Pty`, `DisallowedTools`, `Effort`, and `TitleArtifactCleanup` are asymmetric
today. `Pty` maps to `use_pty` (Node.js line-buffering workaround,
Claude-only). `TitleArtifactCleanup` maps to `cleanup_title_artifact`
(ai-title jsonl session-leak workaround for Claude Code 2.1.110; Grok/Codex
have no equivalent). Codex v1 accepts prompt-over-stdin with JSONL output only:
`codex -a never exec --json --sandbox workspace-write --ephemeral
--skip-git-repo-check --cd <cwd> [-m model] -` for scoped/auto modes, and the
dangerous sandbox-bypass flag only for dangerous mode. Do not add Codex flags
by checking `RunnerKind` at call sites; add a capability and let dispatch reject
unsupported `RunnerOpts`.

## Codex provider integration

Codex (OpenAI `codex` CLI) is the third runner alongside Claude and Grok. The
merged Codex path layers four load-bearing defenses on top of the V2 base:

### Selection — explicit provider intent only

Codex is reachable EXCLUSIVELY through `primaryRunner` entries with
`provider: "codex"` (the `model` field may be empty). `provider_for_model`
classifies model strings into `Provider::Claude` or `Provider::Grok` and NEVER
returns `Provider::Codex`; no `gpt-*` / `o*` / `codex-mini` / "openai"
substring is ever interpreted as Codex intent. The strict parser
(`parse_config_provider`) rejects misspellings (`"openai"`, `"codex-cli"`,
`"groq"`) at config-load time so a typo hard-fails preflight instead of
silently routing to Claude. `resolve_effective_runner` consumes
`EffectiveRunnerInput { model, provider_hint }` as an explicit struct (no
`Option<&str>` shorthand in production) so the dispatch site cannot drop a
validated `provider_hint` (the `From<Option<&str>>` impl is `#[cfg(test)]`-gated
to keep the production drift surface narrow). Scanner test:
`tests/no_bare_option_resolve_effective_runner.rs`.

### Stream — confirmed JSONL schema (codex-cli 0.135.0)

`CodexStreamFormat` parses top-level events `thread.started` /
`turn.started` / `item.started` / `item.completed` / `turn.completed`, plus
errors as `{type:"error", message}` or `{type:"turn.failed", error.message}`.
Per-item kind is `item.type` (NOT `item_type`). A single `command_execution`
emits two events — `item.started` carries `ToolUse`, `item.completed` carries
`ToolResult`; never duplicate either. `agent_message` arrives in one
`item.completed` block via `text`. Source: live capture against
codex-cli 0.135.0.

### Auth-failure detection — structured `[Error: ]` lines only

`codex_conversation_indicates_auth_failure` walks the assistant transcript
line-by-line and only matches auth markers against bodies that
`trim_start().strip_prefix("[Error: ")` returns. A naive full-text substring
scan over the transcript would false-positive on `agent_message` text or tool
output containing strings like "HTTP 401". The `[Error: ` prefix is produced
centrally by the stream accumulator from any `StreamEvent::Error` (top-level
`type:"error"` or `turn.failed`), so structured detection is reliable.

`CodexAuthFailure` is excluded from `handle_task_failure` at BOTH callers —
`orchestrator.rs` (sequential) and `wave_scheduler.rs` (wave) — so auth lapses
never push healthy tasks toward `auto_block_after_failures`. A sentinel test
(`test_codex_auth_failure_excluded_at_callers`) reads both files and asserts
the `CrashType::CodexAuthFailure` pattern is still listed; deleting the
exclusion fails at unit-test time.

### Codex→Claude RuntimeError fallback — REMOVED (REFACTOR-006)

The opt-in `primaryRunner.<route>.runtimeErrorFallback` promotion (Codex→Claude
on repeated RuntimeError crashes, via `maybe_provider_runtime_fallback`) was
DELETED. Post-hard-break, `primaryRunner` / `fallbackRunner` are rejected at
preflight (`LEGACY_MODEL_KEYS`), so `ProjectConfig.primary_runner` /
`.fallback_runner` are always `None` at loop time and the promotion could never
fire in production. The migrate-vs-delete decision resolved to DELETE rather
than re-home it on the provider-first surface (migrating would re-accrete a
config surface the FR-001 redesign exists to eliminate; the PRD migrated only
the overflow rung-4 pivot to `providers.<source>.fallback`).

Current Codex RuntimeError behavior: a Codex task short-circuits OUT of the
same-provider Claude tier escalation (so its NULL/`gpt-*` model is never
rewritten to Opus) and proceeds to normal failure accounting → `auto_block`.
The `tests/codex_runner_overrides_invariant.rs` never-insert-Codex invariant
and the `codex_recovery.rs` "Codex never promotes" contract tests still pin
this. Cross-provider rescue for Codex exists ONLY via the explicit overflow
rung-4 `providers.codex.fallback` (FEAT-007), not on the RuntimeError path.

### Protected-state guard — post-hoc detect-and-revert

Codex runs with confined write access (`--cd <cwd>` plus its own scratch),
and an adversarial or buggy Codex prompt could still mutate orchestrator
state (`tasks/*.json`, `*-prompt.md`, `.last-branch`, the SQLite trio) via
`/tmp` symlinks or sandbox-escape attempts. The defense is POST-HOC: capture
a snapshot pre-spawn (read-only, `immutable=1` for SQLite — never opens
read-write, never replays the WAL), run Codex, then verify-and-restore. The
guard lives in `src/loop_engine/protected_state.rs` and is wired at three
call sites:

| Path | Call site | Snapshot ctor |
|---|---|---|
| Sequential | `iteration.rs:493 / :550` | `Snapshot::take(.., RunnerKind)` (no-op for non-Codex) |
| Per-slot (wave) | `slot.rs:201 / :256` | `Snapshot::take(.., RunnerKind)` (no-op for non-Codex) |
| Wave-level (synthetic FailedMerge on corruption) | `wave_scheduler.rs:1198 / :1212` | `Snapshot::take_unconditional` (already gated by `codex_wave`) |

`runner_requires_state_guard()` is the SSoT for the gate
(`RunnerKind::Codex => true`; Claude/Grok return false and the snapshot is
`None`, byte-identical to the pre-guard build). `VerifyOutcome` is tri-state:
`Clean` → no-op; `Reverted` → ui::emit one line per restored/removed path;
`FatalSqliteCorruption` → per-task sites fail the iteration via
`apply_verify_outcome`, wave sites synthesize a `FailedMerge` so
`apply_merge_fail_reset_and_halt_check` preserves the cascade-halt streak.

**Intentionally detect, not prevent.** The TOCTOU window between
`Snapshot::take` and `Snapshot::verify_and_restore` is acknowledged in the
module rustdoc — a concurrent reader between capture and revert sees the
mutated state momentarily. Defending the orchestrator AFTER the Codex
process exits is the load-bearing guarantee; prevention would require a
heavier sandbox (separate uid, bind mounts) that v1 explicitly punts on.
`walk_protected` fails OPEN on `EACCES` (a permission-denied subtree
contributes nothing to the snapshot) so a hostile chmod cannot mask a
mutation by hiding the directory — restoration just won't touch what it
couldn't see.

### Binary probe — route-gated, BOTH entry points

`preflight_validate_and_probe` runs from `main.rs::loop run` AND
`batch.rs::run_batch` before any PRD work. It composes
`validate_runner_routing_config` (strict provider parse, model presence,
fallback-runner shape) with `check_codex_runner_binary` (PATH lookup of
`CODEX_BINARY` / bare `codex`, executable-bit check on Unix). The probe is
ROUTE-GATED: a pure-Claude config (no Codex route anywhere in
`primaryRunner`) returns Ok before the env var or PATH is touched, so
operators without `codex` installed never see a probe failure. Batch failure
of preflight short-circuits the entire batch with `succeeded:0, failed:1,
results:[]` — identical to the existing `collect_prd_files` Err shape.

### Prohibited outcomes (compile-time + test-time)

- `Provider::Codex` returned from `provider_for_model` (test-asserted; the
  function signature returns Claude/Grok only)
- `RunnerKind::Codex` inserted into `IterationContext::runner_overrides`
  (sentinel test `tests/codex_runner_overrides_invariant.rs`)
- Full-text substring scan over the assistant transcript for auth markers
  (must walk lines and match only `[Error: ` prefixes)
- Byte-restoring a live SQLite WAL/SHM trio (integrity-check only; fatal on
  corruption; never overwrite)
- Inferring Codex from a model string (`gpt-*` / `o*` / `codex-mini` etc.)
- Stamping a per-task `model` on `REVIEW-` / `REFACTOR-` / fixup tasks —
  routing is config-owned via `primaryRunner.byIdPrefix`

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
| Linear startup phase (WS-3.2 carve) | `src/loop_engine/startup.rs` | `initialize_loop`, `LoopInitContext` |
| Sequential iteration body | `src/loop_engine/iteration.rs` | `run_iteration` |
| Wave scheduling + merge-back | `src/loop_engine/wave_scheduler.rs` | `run_wave_iteration`, `run_parallel_wave` |
| Wave decision helpers (WS-3.3 carve) | `src/loop_engine/wave_orchestration.rs` | `wave_preflight_check`, `handle_no_eligible_tasks`, `handle_ephemeral_deadlock` |
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
| LLM runner dispatch | `src/loop_engine/runner.rs` + `src/loop_engine/engine.rs` | `RunnerKind`, `dispatch`, `ClaudeRunner`, `GrokRunner`, `CodexRunner`, `resolve_effective_runner` |
| Capability surface | `src/loop_engine/runner.rs` | `RunnerCapability`, `LlmRunner::supports`, `enforce_capabilities`, `CHECKS` |
| Provider routing | `src/loop_engine/model.rs` | `Provider`, `ExecutionPlan`, `provider_for_model`, `resolve_execution_plan` |
| Operator escape valve | `src/loop_engine/recovery.rs` | `check_override_invalidation` |
| Overflow original model snapshot | `src/loop_engine/engine.rs` | `IterationContext::overflow_original_task_model` |
| Spawn-side model/provider routing | `src/loop_engine/model.rs` | `resolve_execution_plan`, `ExecutionPlan`, `PlanContext`, `ResolvedModelsConfig` |
| Cross-provider promotion primitive (overflow rung-4) | `src/loop_engine/recovery.rs` | `promote_once`, `PendingPromotion`, `apply_pending_promotion` |
| Auto-review launch boundary | `src/loop_engine/auto_review.rs` | `maybe_fire`, `maybe_fire_inner`, `ProcessLauncher` |
| Shared post-Claude pipeline | `src/loop_engine/iteration_pipeline.rs` | `process_iteration_output` |
| Merge resolver | `src/loop_engine/merge_resolver.rs` | `ClaudeMergeResolver`, `MergeResolver` trait |
| Stash preflight | `src/loop_engine/worktree.rs` | `prepare_slot0_for_merge`, `cleanup_preparation`, `run_slot_merge_attempt` |
| Post-merge slot reconcile | `src/loop_engine/git_reconcile.rs` | `reconcile_merged_slot_completions` |
| Post-completion reactions (#8/#9/#10) | `src/loop_engine/reactions/post_completion.rs` | `react_to_completions` (coordinator, both paths), `react_to_completions_inner` (hermetic core), `PostCompletionParams`, `ReviewFn`; relocated leaf `orchestrator::trigger_human_reviews` (`#[deprecated]` shim) |
