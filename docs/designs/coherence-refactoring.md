# Coherence Refactoring: Reducing Accreted Complexity

## Overview

task-mgr works extremely well, but its internal structure has become difficult to reason about and extend. The system was originally a clean Rust replacement for a bash loop script. Over time, major capabilities were added (parallel slot execution with worktree merge-back, rich learnings system with embeddings + bandit + supersession, overflow recovery, human review, batch mode, curate, doctor, auto-review, etc.) by extending existing paths and layering defensive mechanisms rather than evolving the core abstractions.

The symptoms are well-known to maintainers:

- `src/loop_engine/engine.rs` is ~9.6k lines and acts as the integration hub for almost everything.
- `src/loop_engine/worktree.rs` (~6.3k) contains many layers of hardening (synthetic slots, ephemeral overlays, stash preflights, slot-0 guards, reconcile paths) added after cascade failures.
- Direct `UPDATE tasks SET status` writes are scattered across **~15 sites in 13+ files** — not the "seven command modules" first reported. `TaskStatus::can_transition_to` in `src/models/task.rs` is documented as the single source of truth but is currently consulted by **only 2 of those ~15 sites** (`complete.rs`, `fail/transition.rs`). The remaining sites (`skip.rs`, `irrelevant.rs`, `unblock.rs`, `reset.rs`, `review.rs`, `doctor/fixes.rs`, `init/mod.rs`, `next/mod.rs`, `prd_reconcile.rs`, `overflow.rs`, and roughly a dozen UPDATE sites inside `engine.rs` itself) write status with raw SQL. The "SSoT" is aspirational, not enforced. The full audit is in §"Status-Write Site Audit" below. The refactoring case is *stronger* than the original framing suggested.
- Prompt construction is split across `prompt/core.rs` (shared helpers) plus two compositions in `prompt/sequential.rs` and `prompt/slot.rs`, with a manual "any new section MUST also be wired through `slot`" contract documented in `prompt/mod.rs`. Less "three parallel builders" than "two compositions over a shared kit with a hand-enforced wiring rule," but the hazard is the same.
- The learnings retrieval pipeline requires threading the same supersession filter, retirement logic, and UCB scoring across multiple backends plus post-filtering in Rust. (Confirmed: a single `SUPERSESSION_SUBQUERY` const in `retrieval/mod.rs` is used by 4 SQL backends; the vector backend uses a parallel `HashSet::contains` post-filter.)
- `main.rs` and `cli/commands.rs` contain substantial deprecated-shim dispatch logic that will "remain supported forever."
- Every new feature tends to produce "also update X, Y, and Z" rules that are documented in subsystem `CLAUDE.md` files because they are too numerous and subtle to keep in heads.

The root cause is **evolutionary accretion without periodic architectural refactoring**. We have many correct local patches and excellent defensive layers, but the number of places that must be touched (and the number of cross-cutting invariants that must be maintained) has grown faster than the number of crisp, composable abstractions.

## Goals

1. **Fewer patch surfaces**: Adding a new behavior or recovery path should touch a small number of obvious extension points rather than multiple command modules + engine call sites + prompt sections + retrieval backends.
2. **Clear ownership**: One module (or small cluster) owns the task lifecycle (valid transitions + all side effects: run tracking, decay columns, PRD reconciliation, progress logging).
3. **Obvious extension points**: Prompt sections and recall strategies should be registered or composed in one place with compile-time or test-time enforcement that both sequential and wave paths (and all retrieval backends) stay in sync.
4. **Preserved safety**: All current invariants around decay, merge-back cascades, overflow recovery, supersession filtering, permanent user-facing shims, and dual DB+PRD writes must remain intact (or be demonstrably improved).
5. **Incremental, verifiable progress**: The work must be broken into PRD-sized units that can be planned, executed by loops or humans, reviewed, and landed without long-lived feature branches or mass breakage.
6. **Improved testability and shrinkability**: The 9k-line engine and duplicated transition logic are the primary obstacles to both.

## Status-Write Site Audit

The Phase 1 PRD's acceptance criteria depend on an honest count of every site that writes `tasks.status` directly. The categories below are deliberately separated because they have different semantics — a "thin caller" abstraction that works for category A may be wrong for D.

| Category | Sites | Files | Semantics |
|---|---|---|---|
| **A. User-facing command** (calls `can_transition_to` or is a leaf verb) | 7 | `commands/complete.rs`, `commands/fail/transition.rs`, `commands/skip.rs`, `commands/irrelevant.rs`, `commands/unblock.rs`, `commands/reset.rs`, `commands/review.rs` | Operator intent → DB write + side effects (run_tasks, PRD JSON, audit log) |
| **B. Pre-claim race-safe transition** | 1 | `commands/next/mod.rs` (`todo → in_progress` with status-guarded WHERE) | Conditional update; must remain atomic |
| **C. Loop-side recovery** (bulk and per-task) | ~12 sites | `loop_engine/engine.rs` (claim guards, auto-block, mid-iteration `in_progress → todo` bulk resets across an entire prefix, ~12 distinct UPDATE sites), `loop_engine/overflow.rs` (`in_progress → blocked`, fallback promotion, todo recovery) | Recovery primitives — not user transitions; some are prefix-scoped bulk operations |
| **D. Reconcile / PRD-driven** | 2+ | `loop_engine/prd_reconcile.rs` (`todo → done`, `* → irrelevant` based on PRD JSON), `commands/doctor/fixes.rs` (stale-state repair) | Source of truth is *not* operator intent; driven by an external artifact or invariant check |
| **E. Bootstrap** | 1 | `commands/init/mod.rs` (initial `done` writes during ingest) | Only runs at PRD import; out of scope for normal lifecycle |

This audit becomes the Phase 1 PRD's literal checklist — every row must either route through `TaskLifecycle` or have a written rationale for staying out. See §"TaskLifecycle Scope Decision" for the resolution policy.

## Proposed Refactorings

These six areas were identified during the 2026 coherence review. They are not equally sized or equally urgent.

### 1. TaskLifecycle Service / Aggregate (Highest ROI)

Introduce a single service (proposed names: `TaskLifecycle`, `TaskService`, or `TaskAggregate` in a new `domain/` or `commands/lifecycle/` module) that is the *only* place allowed to perform status transitions.

Responsibilities:
- Own the full transition matrix (reusing or moving `can_transition_to` logic).
- Execute the exact SQL updates (including `blocked_at_iteration`, `skipped_at_iteration`, error_count, notes, timestamps).
- Manage `run_tasks` rows.
- Invoke PRD JSON reconciliation via a narrow seam (currently spread across `prd_reconcile.rs`, `git_reconcile.rs`, and individual commands).
- Emit the necessary progress/audit side effects.

All existing command functions (`complete`, `fail_single_task`, `skip`, `irrelevant`, `unblock`, `reset_*`) and the loop engine's `<task-status>` tag path (`apply_status_updates` + `iteration_pipeline`) become thin callers of this service.

**Why this is foundational**: It collapses ~15 status-write sites into a small named surface. Future changes (new terminal state, new decay rule, new audit requirement) touch one place. The existing `iteration_pipeline.rs` is the direct design template: a single typed function with explicit pipeline steps, called from both sequential (engine.rs ~L3204) and wave (engine.rs ~L1166) paths.

#### TaskLifecycle Scope Decision

Resolving "what does the service own?" is the load-bearing decision of Phase 1. The audit categories above map to scope as follows:

- **Category A (user-facing commands) — owned in full.** Every leaf verb routes through `TaskLifecycle::apply(intent)`. `can_transition_to` becomes a private validator inside the service. The 7 command-module functions become thin shells: parse args → build intent → call service.
- **Category B (race-safe pre-claim) — owned, but the API surface carries the conditional predicate.** `TaskLifecycle::try_claim(task_id, expected_statuses: &[TaskStatus])` exposes the `WHERE id = ? AND status IN (...)` guard explicitly. Hidden idempotency is a footgun for wave-mode resumption.
- **Category C (loop-side recovery) — owned via dedicated bulk verbs**, NOT per-task calls in a loop. The service exposes `recover_in_progress_for_prefix(prefix)`, `auto_block_after_failures(task_id, reason)`, and `resurrect_for_iteration(prefix, ids)`. These are not "transitions" in the user-intent sense, but routing them through the service is the only way to make recovery behavior observable and testable.
- **Category D (reconcile / PRD-driven) — owned via a separate API**, `TaskLifecycle::reconcile_from_prd(plan)`, which takes a pre-computed reconciliation plan as input. The plan-building stays in `prd_reconcile.rs`; the *application* of the plan goes through the service so the same side effects (run_tasks, audit, PRD JSON sync) run as for operator intent.
- **Category E (bootstrap / `init`) — stays out of scope.** Initial ingest writes happen before any run exists; routing them through the lifecycle service would add ceremony without value. Documented as a deliberate exclusion.

The corollary: TaskLifecycle is *not* purely a "task" service — it is a status-transition + recovery service. The doc previously implied the former; this section makes the latter explicit.

#### Contract-Level Invariants the Service Must Preserve

These are lifted from current implicit behavior in `apply_status_updates` and the command modules. They become CONTRACT-xxx items in the Phase 1 PRD:

- **Auto-claim on `<task-status>:done` from `todo`.** Today `engine.rs` (~L4726–4750) auto-claims a `todo` task before completion when the LLM emits the side-band tag without a prior claim. The service MUST preserve this; rejecting `done` from `todo` would break loops that complete a task in a single iteration.
- **Per-task partial failure tolerance.** `apply_status_updates` returns `Vec<(task_id, change, applied: bool)>`; one task's dispatch erroring does not abort the iteration. A refactor that converts this to `Result<(), Err>` at the batch level would change observable loop behavior — explicitly disallowed.
- **DB authoritative, PRD JSON best-effort.** `apply_status_updates` writes DB first, then calls `update_prd_task_passes` and emits a stderr warning if the PRD JSON sync fails (engine.rs ~L4787–4793). Operators grep stderr for this string. The service MUST inherit this exact contract.
- **`run_tasks` row bookkeeping moves into the service.** Today the engine wraps each command call with `run_tasks` inserts (engine.rs ~L4736–4749); the command modules do not. After the refactor, the service owns this. Callers do not need to know `run_tasks` exists.
- **Conditional-WHERE predicates are part of the API.** `claim_slot_task` (engine.rs ~L787) uses `WHERE id = ? AND status IN ('todo', 'in_progress')` deliberately for slot-resumption idempotency. The corresponding `try_claim` API exposes this set explicitly.

### 2. Break Up `engine.rs` + Clear Orchestration Boundaries

Carve the 9k-line `engine.rs` along the seams that already partially exist:

- `orchestrator.rs` / `loop_coordinator.rs` — outer `run_loop`, batch lifecycle, signal handling, run begin/end, config loading, worktree setup/teardown.
- `iteration.rs` — sequential iteration path.
- `wave_scheduler.rs` + `slot.rs` — parallel wave execution and slot result processing.
- Retain and strengthen `iteration_pipeline.rs` as the single post-Claude processing contract (already a unification win).

The goal is that cross-cutting concerns (new monitoring, new recovery hook, new progress event) have one obvious registration or call site instead of "search the 9k-line file for the three places this also has to happen."

### 3. Data-Driven Prompt Construction

The current shape is `prompt/core.rs` (shared section helpers) plus two compositions in `prompt/sequential.rs` and `prompt/slot.rs`. The hazard is the *manual wiring rule*, not three forked builders. The fix is incremental:

- Introduce a single `PromptAssembler` (or `PromptBuilder` trait) + registry of sections in `prompt_sections/`.
- `prompt_sections/` modules implement a small `PromptSection` trait (or are listed in one registration table).
- Both sequential and slot execution paths use the same assembler.
- Adding a new section (or modifying an existing one) is a single-site change + a registration line.
- The current human-enforced rule ("new sections MUST also be wired through `slot`," documented in `prompt/mod.rs`) becomes a mechanical or test-enforced property.

Because `core.rs` already factors most helpers, this is more a *registration table* change than a rewrite — pilot one section through the assembler first, then bulk-migrate.

### 4. Strengthen Learnings Retrieval Abstraction

Push the existing `retrieval/composite.rs` direction further:

- Introduce a `RecallQuery` / `RecallPlan` type that describes the full request (text, file patterns, task types, error patterns, UCB context, supersession/retirement flags, reranker config, etc.).
- Each backend (FTS5, patterns, vector) implements a strategy for the plan.
- Supersession, retirement, and curate filtering become decorator layers or a single pre/post filter pass rather than a `SUPERSESSION_SUBQUERY` const that must be manually included in every SQL string plus a Rust post-filter for the vector path.
- The `LearningWriter` chokepoint (already good) is complemented by an equally clear retrieval contract.

### 5. Isolate Compatibility Shims

Move all permanent-but-deprecated dispatch logic (`resolve_loop_command`, `resolve_batch_command`, the `init --from-json` shim, flat-form handling, etc.) into a small `cli/compat.rs` or `deprecations.rs` module.

- `main.rs` dispatch becomes a clean match on canonical forms.
- The "this will continue to work forever" user contract is preserved.
- Future changes to CLI surfaces are less likely to accidentally touch (or be polluted by) legacy paths.

**Caveat — the shim layer is not pure translation today.** `dispatch_init_shim` in `main.rs` runs `init_project` (which has a model-picker TTY side effect) before dispatching to the canonical command. Phase 0 has two options:

1. **Pre-work**: lift the model-picker into the canonical `init` path so the shim becomes pure translation, then move it. (Cleaner final state; slightly more Phase 0 scope.)
2. **Document the exception**: accept that `cli/compat.rs` may carry a small set of side-effecting preludes for shims with installed-base behavior, and require each such prelude to be named in a header comment. (Smaller Phase 0; the "compat is pure translation" rule becomes "compat is the only place behavioral preludes live".)

Pick (1) if the Phase 0 PRD has appetite; (2) is the safer default. Either choice must be made *before* spawning Phase 0 — don't leave it to the implementer.

### 6. (Research Direction, Not a Commitment) Narrow Command/Event Journal for Lifecycle

Once a `TaskLifecycle` service exists and is stable, *consider* making all state changes produce events that are applied through the same service.

**This is research, not planned work.** Two concrete reasons it is harder than a simple sequencing dependency on Item 1:

- `apply_status_updates` already does cross-task work that doesn't fit cleanly into a per-event journal: `run_tasks` row insertion, milestone hooks (engine.rs ~L4800–4807), PRD JSON sync. An event log would need a coarser-grained event ("status-update batch for iteration N") to preserve these — at which point the journal is closer to an audit log than a true event-sourced model.
- Event sourcing implies replay, but `progress::summarize_milestone` and `update_prd_task_passes` are not idempotent under replay (they emit user-visible side effects: stdout summaries, PRD JSON file writes). Making them idempotent is a separate design exercise of comparable size to Phase 1.

Treat this as "evaluate after Phase 1 ships," not "the Phase 3 plan."

## Synergy and Sequencing Analysis

### Synergistic Clusters

**Cluster A — Core Domain Stabilization (Items 1 + 2)**: Strongly synergistic and best executed as a single planning effort (one design doc feeding two sequential or tightly-coupled PRDs).

- Item 1 (TaskLifecycle) gives the engine something clean and narrow to call.
- Item 2 (engine carve) makes the call sites obvious and prevents the new service from being hidden inside the 9k-line monolith.
- Doing the engine split *without* TaskLifecycle first mostly moves the mess. Doing TaskLifecycle without touching the engine leaves dozens of old call sites scattered and the "big ball" still 9k lines.
- The shared `iteration_pipeline.rs` already shows the value of unification; this cluster is the larger version of that win.
- **Recommendation**: Plan these two together under one "Phase 1: Core Domain Stabilization" umbrella. They can be two PRD units if the first PRD lands a stable TaskLifecycle seam that the second PRD then uses while carving the engine.

**Cluster B — Surface Unification (Items 3 + 4)**: Spiritually synergistic (both replace "remember to patch N places" with "register once") but technically loosely coupled.

- They touch different runtime paths (prompt assembly vs. recall before `next`/`loop`).
- Low risk of interference.
- Can be executed in parallel with each other or after late Phase 1 work has settled the iteration boundaries.
- Item 3 has a mild dependency on item 2 (the sequential and slot compositions are called from inside the fat engine); changing the prompt API while the engine is mid-surgery is painful.

**Item 5 (Shim Isolation)**: Low coupling, low risk, low synergy with the others. It is mostly mechanical and improves the readability of the dispatch layer for any subsequent CLI-facing work. Can be done first (as hygiene) or in parallel with anything.

**Item 6 (Event Journal)**: Research direction, not a commitment. Re-evaluate after Phase 1 ships in production loops. The cross-task work currently inside `apply_status_updates` and the non-idempotent side effects in milestone summaries and PRD JSON sync are real obstacles, not implementation details — they need their own design pass.

### Recommended Phasing for Smooth Transition

**Phase 0 — Compatibility Hygiene (low risk, can be first or parallel)**
- Item 5 only.
- Clean `main.rs` and `cli/` dispatch.
- Makes later changes to any command path less noisy.
- Small PRD or even a single focused task list.

**Phase 1 — Core Domain Stabilization (highest impact, highest coordination cost)**
- Items 1 + 2 as one planning unit.
- Heavy emphasis on seams, adapter shims during transition, and exhaustive tests on the lifecycle service *before* large-scale call-site migration.
- The `TaskStatus` model and its transition tests become the contract that the new service must honor.
- Goal: land a stable `TaskLifecycle` that all ~15 status-write sites (per the audit table) already delegate to, even if the engine is still large.
- Then carve the engine while the transition logic is already centralized.
- Serialize the two PRDs ("TaskLifecycle Extraction" → "Engine Orchestration Boundaries"). Doing them in parallel is footgun — they touch overlapping seams.

**Pre-Phase 1 coverage gates** (mandatory before extraction begins):
- Unit tests covering every Category A site (7 commands) — already mostly present; verify and fill gaps.
- Unit tests covering every Category C recovery primitive (currently mostly integration-level). At minimum: `recover_in_progress_for_prefix`, `auto_block_after_failures`, `resurrect_for_iteration`, the `claim_slot_task` predicate.
- A "transition shadow test" harness that, for each known transition site, asserts the new `TaskLifecycle` call produces a byte-identical DB diff to the legacy raw-SQL path. This is the safety net for migration.
- Snapshot tests on `apply_status_updates` stderr output (the "PRD JSON sync failed" warning is a stable contract; operators grep for it).

**Phase 2 — Surface Unification (can overlap late Phase 1 or follow it)**
- Items 3 and 4.
- Item 3 (prompt) is sequenced after the iteration/wave split in Phase 1 has stabilized, so the assembler can be introduced cleanly for both paths.
- Item 4 (recall) is largely independent and can start earlier if desired.
- These produce the visible "fewer rules to remember" wins for future feature work.

**Phase 3 — Future Work (only after Phase 1 is proven)**
- Item 6 (event journal), if the value is still compelling.
- Any further shrinking or extraction that becomes obvious once the core abstractions are stable.

**Why this order is smooth**:
- Phase 1 attacks the two largest sources of "where do I put this new behavior?" pain (fat engine + duplicated transitions).
- All later work benefits from narrower, better-owned call sites.
- Parallelism in Phase 2 is safe because the surfaces are already somewhat separated.
- No phase invalidates the safety layers built for parallel execution, overflow, or learnings feedback; the new abstractions are required to preserve them (and the tests will enforce it).

## Boundary Contract with Runner Trait Hygiene Effort

This effort runs in parallel with the `LlmRunner` Trait Hygiene design documented at `docs/designs/runner-trait-hygiene.md`. The two plans are philosophically aligned (both are "move side-effect ownership to the correct abstraction and eliminate inconsistent call-site patterns"), but they share the same critical edit surface: the iteration spawn + immediate post-processing window inside `src/loop_engine/engine.rs` (both the sequential `run_iteration` path and the slot/wave `run_slot_iteration` + `process_slot_result` paths).

### Shared Boundary Definition

The boundary is the moment after a runner (`dispatch` / `LlmRunner::spawn`) returns and before the next claim or loop iteration decision:

- Runner hygiene Phase 1 introduces an explicit `cleanup_session(...)` call inside `dispatch` (post-spawn) and removes the `cleanup_title_artifact` opt-in flag from all call sites.
- Coherence Phase 1 will route status changes (including the `<task-status>` side-band tag path via `apply_status_updates` and `iteration_pipeline`) through the new `TaskLifecycle` service, while carving the surrounding iteration orchestration into clearer modules.

### Non-Interference Rule for the Thin Post-Dispatch Hook

During the overlapping window (Runner Hygiene Phase 1 and Coherence Phase 1):

- The post-`dispatch` hook in `runner.rs::dispatch` remains a **thin, single-purpose call** that only performs provider-specific artifact cleanup.
- No heavier lifecycle, status, or reconciliation logic is added inside `dispatch` or immediately around the `runner.spawn(...)` call by either effort.
- All status-related work (`apply_status_updates`, completion ladder, feedback, PRD reconciliation, `run_tasks` bookkeeping, etc.) stays in the iteration post-processing layer (the code that will become `iteration_pipeline.rs` + the future `TaskLifecycle` calls). The runner hygiene cleanup call must not grow into that layer.

This keeps the two changes mechanically separable even while both are landing.

### Ownership During the Overlap

- **Runner hygiene** owns changes to `runner.rs`, the `LlmRunner` trait surface, `ClaudeRunner`/`GrokRunner` implementations, the `cleanup_*` helpers in `claude.rs`, and the minimal wiring inside `dispatch`.
- **Coherence** owns changes to `apply_status_updates`, `iteration_pipeline`, the status-write audit sites, and the extraction of `TaskLifecycle`.
- Edits to the raw iteration skeletons in `engine.rs` (the parts that will be carved into `iteration.rs` / `wave_scheduler.rs`) are coordinated: the team that lands first leaves clear seams (small functions or explicit call sites) that the second effort can then move without a second rewrite.

### Module Layout Coordination Point

The "exact module names and crate layout" decision listed in the Next Steps of this document **must** treat `src/loop_engine/runner.rs` as an existing peer. `TaskLifecycle` (and the future orchestration modules) should be placed so that `runner` remains the narrow provider-abstraction layer and does not become the home for task-state or iteration-orchestration logic.

### Risks That Cross Both Efforts

The two new top-level risks added in the Risks section of this document are explicitly called out here:

- **Dogfood concurrency (N-iteration exit gate)**: Both efforts will be editing code that runs in live parallel-slot loops. The serialized execution model chosen for Coherence Cluster A (TaskLifecycle Extraction → dogfood gate of N iterations across two PRDs → Engine Orchestration Boundaries) must be compatible with the soak + integration-test expectations in Runner Hygiene Phase 1. The `N` value and the "no parallel PRDs in the cluster" rule are inputs to the combined schedule.
- **`<task-status>` failure-semantics drift**: Runner hygiene lists "the side-band `<task-status>` tag contract ... must survive bit-identically" as an invariant. Coherence Phase 1's centralization through `TaskLifecycle` must honor the existing per-task partial-failure tolerance and the auto-claim-on-done-from-todo behavior. The transition shadow test harness (pre-Phase 1 coverage gate) is the shared verification mechanism.

### Invariants Both Efforts Must Honor

- The five layers of parallel-slot cascade defenses remain untouched (already stated as orthogonal in the runner document; reaffirmed here).
- The `<task-status>` side-band tag path continues to deliver per-task outcomes; a single failure inside `apply_status_updates` does not abort the iteration.
- `LlmRunner` stays `Send + Sync` (required for parallel-slot dispatch).
- All existing provider-specific workarounds (Claude title-artifact cleanup, Grok session directory handling) and their "NotFound is silent success" semantics are preserved.

### Practical Coordination Steps

Before either Phase 1 PRD is spawned:
1. Add a reciprocal one-paragraph pointer in the runner document (under Risks or a new "Relationship to Coherence Refactoring" note) pointing back to this section.
2. When authoring the Coherence Phase 1 PRD(s), include the runner document's Phase 1 audit rows that touch `engine.rs` spawn sites as "adjacent work" that must be reviewed for merge conflicts.
3. The first of the two efforts to reach code review explicitly lists the other as a "review for overlap" stakeholder.

This contract keeps both hygiene projects moving without forcing either to block on the other, while making the shared danger zone explicit and small.

## How This Document Becomes the Basis for Smaller PRD Unit Efforts

This design is deliberately **not** a single massive PRD. It is the parent narrative.

The expected consumption path (per current task-mgr workflow):

1. Review and ratify this document (or a refined version).
2. For Phase 0: light `/plan-tasks` or a tiny PRD.
3. For Phase 1: one or two focused PRDs (e.g., "PRD: TaskLifecycle Extraction" and "PRD: Engine Orchestration Boundaries") that share the Phase 1 section of this doc as their design context. Use `--depended-on-by` wiring if one is the contract and the other the implementation.
4. For Phase 2: separate PRDs or plan-tasks for prompt unification and learnings retrieval, each referencing the relevant section here.
5. Each PRD produces its own `tasks/<slug>.json` + prompt file and runs through the normal loop or human implementation process.
6. After each landed PRD, the relevant learnings are recorded via `task-mgr learn` and this document is lightly updated (or a "what we learned" appendix is added) before the next phase.

This keeps individual efforts inside the 5–15 task range that the loop engine and human reviewers handle well, while the shared design doc prevents the phases from drifting.

## Risks and Mitigations

- **Risk**: Engine surgery + lifecycle extraction performed in the same window causes temporary instability or duplicated shim code.
  - **Mitigation**: Define the `TaskLifecycle` trait/interface and its test contract in Phase 1 before any large-scale move of call sites. Introduce a temporary forwarding layer if needed so old and new paths can coexist for one or two iterations.

- **Risk**: Prompt assembler change touches every prompt section and both execution modes at once.
  - **Mitigation**: Do the assembler introduction behind a feature flag or as a narrow vertical slice (one section migrated first as proof), then bulk-migrate the rest once the contract is stable.

- **Risk**: Learnings retrieval changes silently alter recall behavior or UCB scoring.
  - **Mitigation**: The existing recall test suite (`src/learnings/recall/tests.rs`, `retrieval/tests.rs`) plus property-based or snapshot tests on scored output must remain green. Supersession and retirement filters get a single "filter contract" test that all backends must pass.

- **Risk**: "Permanent shims" policy makes the compat module grow without bound.
  - **Mitigation**: The compat layer's only job is to translate old shapes into canonical ones and emit the deprecation notice. No new behavior is added there.

- **Risk**: The safety invariants that were hard-won (especially the five layers of parallel-slot defense) are accidentally weakened during refactoring.
  - **Mitigation**: The refactoring plan explicitly requires that every defensive mechanism either moves verbatim into the new structure or is replaced by an equivalent with a clear rationale and test. The relevant sections of `src/loop_engine/CLAUDE.md` and `src/commands/next/CLAUDE.md` are treated as requirements.

- **Risk (NEW)**: The maintainer runs `task-mgr loop` against in-progress PRDs daily on this codebase — the project eats its own dogfood. A Phase 1 PRD that touches `apply_status_updates`, the command modules, and the iteration pipeline in the same window is one bad merge away from corrupting a live PRD's task DB or breaking a running loop iteration.
  - **Mitigation**: Each Phase 1 PRD's exit criteria MUST include "main-branch loop has run continuously for N iterations on the refactored code (against a representative live PRD) before merge-back of the next PRD in the cluster." `N` to be chosen during Phase 1 planning; default proposal is N=10 iterations across two distinct PRDs.
  - **Mitigation**: PRDs in this refactor cluster are serialized, not parallelized. The two-PRD Cluster A executes as TaskLifecycle → engine-carve, with the dogfood gate between them.

- **Risk (NEW)**: Centralizing through `TaskLifecycle` changes the failure semantics of the side-band `<task-status>` tag path. Today, one failed transition inside `apply_status_updates` is reported back per-task and does not abort the iteration; the auto-claim path silently promotes `todo → in_progress` before completion. A naive port would either abort iterations or refuse the auto-promote.
  - **Mitigation**: The contract invariants in §"Contract-Level Invariants the Service Must Preserve" are CONTRACT-xxx tasks in the Phase 1 PRD. The "transition shadow test" harness in the pre-Phase 1 coverage gates catches divergence before merge.

## Invariants That Must Be Preserved

- `TaskStatus` transition rules and the meaning of terminal states (`done`, `irrelevant`) — see `src/models/task.rs`.
- Decay of `blocked`/`skipped` tasks after N iterations (configurable, currently 32).
- All five layers of parallel-slot cascade defenses (synthetic shared-infra slot, buildy-prefix heuristic, ephemeral overlay, consecutive-merge-fail halt, stale-ephemeral hygiene) — or demonstrably better replacements.
- LearningWriter as the creation chokepoint; supersession filter as a single-source contract (`SUPERSESSION_SUBQUERY` + vector post-filter equivalence).
- Permanent user-facing compatibility shims (`init --from-json`, flat `loop`/`batch` forms) continue to work and print the expected notices.
- Atomicity of DB + PRD JSON writes for operations that currently guarantee it (`add --depended-on-by`, init append/update, reconcile paths).
- Graceful degradation everywhere (missing Ollama, missing credentials, non-TTY, etc.).
- The "side-band `<task-status>` tag" contract and the fact that the loop engine is a *client* of the command library for user-facing transitions. (Note: today the engine bypasses the command library for ~12 Category C recovery sites — see audit table. The refactor's job is to route these through `TaskLifecycle`, not to pretend they were ever clean.)
- **Auto-claim on `<task-status>:done` from `todo`** — the loop completes single-iteration tasks without a prior `claim`. The lifecycle service must preserve this.
- **Per-task partial-failure tolerance** — `apply_status_updates` returns per-task outcomes; one task failing does not abort the iteration. Batch-level `Result<(), Err>` is explicitly disallowed for this entry point.
- **DB authoritative, PRD JSON sync best-effort with stderr warning** — operators grep stderr for the "PRD JSON sync failed" string. The exact warning shape is a stable contract.
- **`run_tasks` row bookkeeping migrates into the lifecycle service** — today this lives inside `apply_status_updates`, not the command modules. Post-refactor, the service owns it and command callers do not need to know `run_tasks` exists.
- **Conditional-WHERE predicates are part of the public API surface** — `claim_slot_task` (engine.rs ~L787) deliberately uses `WHERE id = ? AND status IN ('todo', 'in_progress')`. The corresponding `try_claim` API exposes the expected-status set explicitly; it must not be hidden behind an unconditional method.

## Next Steps

1. Review this document with maintainers (especially anyone who has lived through the parallel-slot or learnings-recall work).
2. Resolve the **Phase 0 shim-prelude choice** (Item 5, options 1 vs. 2) — pick before spawning the Phase 0 PRD.
3. Resolve the **dogfood concurrency policy** — agree on the exit-gate `N` (default proposal: 10 iterations across two PRDs) before spawning any Phase 1 PRD.
4. Decide on exact module names and crate layout for the new `TaskLifecycle` service (domain/ vs. commands/ vs. a new lightweight crate?).
5. Author the Phase 0 PRD or task list (shim isolation) — small, can use `/plan-tasks`.
6. Author the Phase 1 design/PRD(s) with concrete interface sketches for `TaskLifecycle` (including the `try_claim`, `recover_in_progress_for_prefix`, `reconcile_from_prd` verbs) and the proposed engine module split. The contract-level invariants in this doc become `CONTRACT-xxx` tasks.
7. Land the pre-Phase 1 coverage gates (transition shadow test harness, Category C unit tests, stderr snapshot tests) as a separate small PRD or task list *before* TaskLifecycle extraction begins.
8. Update this document with any decisions reached during review.
9. After each phase lands, append a short "retrospective" section here and feed the concrete learnings into `task-mgr learn` so future efforts inherit the knowledge.

This document is intended to be the stable context for a sequence of smaller, well-scoped PRD efforts rather than a single heroic rewrite. The bet is that centralizing the lifecycle and clarifying the orchestration boundaries will make every subsequent change (including items 3, 4, and the Item 6 research) cheaper and safer than they would be on the current structure.