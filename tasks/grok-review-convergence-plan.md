# Grok Review: Converging the Sequential and Wave Execution Paths

**Purpose**: Captured review and clarifying decisions to bring full context into planning/implementation sessions for the path-convergence effort.

**Related PRD**: `tasks/prd-unify-sequential-and-wave-execution.md` (the source of the plan under review).

**Reviewer**: Grok 4.3 (April 2026)

---

## Executive Summary

The plan correctly identifies a recurring, high-severity class of bug: behavioral drift between the sequential (`run_iteration` + orchestrator post-processing) and parallel/wave (`run_wave_iteration` + slot) execution paths in the loop engine.

The proposed solution — a single `reactions/` framework for all non-path-specific main-thread post-Claude behavior, backed by behavioral parity tests *and* a structural enforcement mechanism — is the right architectural direction. It follows the successful precedent of `iteration_pipeline`.

**After clarifying questions**, the effort is now understood to be a **significant internal platform refactor** rather than a tactical bug fix with follow-on hygiene:

- `reactions/` will be the new **physical home** for the converged logic (not a thin facade).
- The structural lock must provide **strong compile-time / visibility enforcement** (not best-effort grep).
- Phase 1 (the rate-limit bug fix) is **all-or-nothing** — the full framework, module, enforcement, and harness must exist before the behavioral change can land.
- Minimum success bar: Converge + lock **all six** items marked Divergent/Drifts/easy-to-drift in the plan's audit (#2, #3, #5, #6, #10, #13).

The plan is sound in direction. The clarifications increase scope, first-PR size, and refactoring surface, but also raise the long-term quality bar appropriately.

---

## 1. Original Plan Overview (Condensed)

The plan targets repeated divergence between:

- **Sequential path**: `iteration.rs::run_iteration` + post-processing in `orchestrator.rs::run_loop`
- **Wave/parallel path**: `wave_scheduler.rs::run_wave_iteration` + `slot.rs` (when `parallel_slots > 1`)

Three prior bugs had already been fixed reactively:
1. Wave silently skipped learning extraction / bandit feedback / already-complete fallback → fixed via `iteration_pipeline::process_iteration_output`.
2. Wave empty-group path lacked all-complete + recovery handling → fixed via `classify_drained_queue`.
3. (Current) Rate-limit / session-limit waiting exists only in sequential. In wave mode this produces false "3 consecutive stale iterations" aborts and strands `in_progress` tasks.

**Hard constraints** respected by the plan:
- `rusqlite::Connection` is `!Send` → slot workers never touch DB or `IterationContext`. All reactions are main-thread only (after `run_parallel_wave` joins).
- Convergence means extracting main-thread reactions into shared helpers; wave folds its N results.

**Proposed framework**:
- New module `src/loop_engine/reactions/` (organized by structural position: `pre_spawn`, `account`, `post_completion`).
- No over-abstracting `Reaction` trait (different semantics).
- Behavioral parity tests (`tests/reaction_parity.rs`) + source-grep/structural lock.
- Staged implementation (5 phases), with Phase 1 fixing the rate-limit bug.

**Divergences to converge**: #2 (crash escalation shape), #3 (pre-iteration usage gate), #5 (overflow — lock), #6 (rate-limit — the bug), #10 (human review), #13 (budget accounting — lock).

---

## 2. Initial Review (Before Clarifications)

**Strengths**:
- Excellent root-cause diagnosis of the bug class.
- Correctly chose the `iteration_pipeline` precedent over a polymorphic trait.
- Good pragmatism (Phase 1 as potentially shippable bug fix).
- Respects the `!Send` constraint and main-thread reaction reality.
- Strong enforcement model (parity tests + structural lock).

**Gaps / Risks flagged** (later resolved or sharpened by clarifying questions):
- Ownership model for `reactions/` vs existing `recovery.rs` / `usage.rs` / `signals.rs` was ambiguous.
- Source-grep lock is inherently best-effort; stronger guarantees have cost.
- Mixed-wave timing for human review and budget accounting needed explicit decisions.
- Phase 1 independence vs full-framework requirement was unclear.
- Effort override application in pre-spawn was not explicitly called out as in-scope for Phase 2.
- Overflow folding vs separate reaction needed a decision.
- Enforcement realism and first-PR size needed calibration.

The initial review concluded the plan was "directionally excellent" but needed the above clarifications before PRD generation, with a recommendation to consider a preceding spike for the riskiest architectural decisions.

---

## 3. Clarifying Questions and Answers

These questions were asked to make the review concrete. Answers are recorded verbatim.

### 3.1 Module Character of `reactions/`

**Question**: Intended as (a) thin coordination/facade, (b) new physical home for the logic, or (c) other?

**Answer**: Leaning toward **new physical home** (functions move or are reimplemented there) to make future maintenance easier.

**Implication**: Higher refactoring cost. `recovery.rs`, parts of `usage.rs`, etc. will shrink or become thin callers. "Single home" in the CLAUDE.md contract means the authoritative location of the logic.

### 3.2 Structural Lock Strength

**Question**: What strength, and what maintenance cost are you willing to accept?

**Answer**: **Stronger compile-time or visibility enforcement** (not just best-effort grep).

**Implication**: Must design real friction (e.g. `pub(crate)` surface inside `reactions/`, `#[deprecated]` + deny warnings, sealed patterns, or equivalent) so that re-inlining from `iteration.rs` or `wave_scheduler.rs` is a hard compile or test failure.

### 3.3 Human Review in Mixed Waves

**Question**: Allow `trigger_human_reviews` on a `WaitedAndRetry` wave, or defer until a clean post-completion wave?

**Answer**: **Allow it** (current plan shape).

**Implication**: Human review is a loop-level side effect. It can fire even when some slots in the wave are still waiting on rate limits. The interactive session blocks while other ephemerals remain unmerged. This must be documented as intentional behavior.

### 3.4 Budget Accounting for Rate-Limit Waves

**Question**: Does a wave that waits on rate limit (`WaitedAndRetry`) consume an iteration against `max_iterations`?

**Answer**: **Do not consume** (matches the spirit of sequential's "skip RateLimit" arm).

**Implication**: `WaveOutcome { iteration_consumed: false, ... }` on rate-limit waits. This decision must be made through the shared reaction helper and pinned by parity tests.

### 3.5 Overflow Recovery Placement (#5)

**Question**: Make `handle_prompt_too_long` part of / adjacent to the pipeline, or its own top-level reaction?

**Answer**: Treat it as its **own top-level reaction** (`reactions::post_output::handle_overflow`).

**Implication**: Not folded into `iteration_pipeline`. Becomes one of the six behaviors the harness and structural lock must protect.

### 3.6 Effort Overrides in Phase 2

**Question**: Is applying `ctx.effort_overrides` via the new `resolve_task_execution` a hard requirement for Phase 2?

**Answer**: **Hard requirement for Phase 2**.

**Implication**: Pre-spawn convergence is incomplete until both model *and* effort overrides flow through the shared helper on both paths.

### 3.7 Phase 1 Independence

**Question**: Can Phase 1 (rate-limit fix) be a narrow, standalone change, or must the full framework exist first?

**Answer**: **All-or-nothing** — full framework must exist for the rate-limit fix.

**Implication**: No tactical "just fix the bug" PR. Scaffolding, module, strong enforcement mechanism, parity harness, and at least the rate-limit reaction must ship together. This significantly increases the size of the first deliverable.

### 3.8 Minimum Scope for Success

**Question**: What is the minimum set that must be converged + locked to declare the bug class closed?

**Answer**: **All items marked 'Divergent' or 'Drifts' or 'easy to drift'** in the audit table (#2, #3, #5, #6, #10, #13).

**Implication**: Clear, non-negotiable acceptance bar for the overall effort.

### 3.9 Other Hard Constraints

**Answer**: None stated at this time.

---

## 4. Revised Concrete Assessment (After Clarifications)

The plan is now substantially stronger and more honest, but also materially larger and riskier.

**Key realization**: The combination of physical relocation + strong compile-time enforcement + all-or-nothing Phase 1 + six-item minimum scope turns this into a **significant, multi-phase internal platform refactor** of the loop engine's reaction surface.

This is the correct level of ambition for permanently closing the drift class of bugs. The choices are internally consistent.

**Trade-off**: Delivery timeline and first-PR size will be larger than the original "Phase 1 as urgent bug fix" framing suggested. Review surface and rebase risk increase accordingly.

**Remaining strengths**:
- Clear success bar.
- Stronger long-term maintainability from choosing the physical home model.
- Explicit handling of mixed-wave human review and budget semantics.

---

## 5. Concrete Recommendations

### 5.1 CONTRACT-001 (Highest Priority)

The PRD **must** open with (or immediately produce) a `CONTRACT-001` task. Its acceptance criteria should include at minimum:

- `src/loop_engine/reactions/` directory + `mod.rs` + the planned submodules exist and are registered in `loop_engine/mod.rs`.
- The four primary public reaction entry points exist (`resolve_task_execution`, `account_usage_gate`, `react_to_outputs`, `react_to_completions` — plus `handle_overflow` per the decision).
- The **strong compile-time / visibility enforcement mechanism** is implemented and active (the specific technique chosen during CONTRACT work must be documented).
- Old call sites in `iteration.rs` and `wave_scheduler.rs` for the six target behaviors are already denied or routed exclusively through the new reaction helpers.
- `tests/reaction_parity.rs` skeleton + behavioral parity tests for the six required items (at least the structural shape and negative controls) are present and passing.
- The "Reaction framework (shared)" section in `src/loop_engine/CLAUDE.md` is written, accurate, and references CONTRACT-001.
- All six target items are declared as dependents of CONTRACT-001.

Only after CONTRACT-001 is accepted should the phased FEAT tasks proceed.

### 5.2 Enforcement Mechanism Design

Because "stronger compile-time or visibility enforcement" was chosen, a short design spike or explicit section in the PRD/CONTRACT must decide the mechanical approach before large-scale moves begin. Viable options that actually deliver the guarantee:

- Make leaf functions (`check_crash_escalation`, the usage wait functions, `trigger_human_reviews`, `handle_prompt_too_long`, etc.) `pub(crate)` only from within `reactions/`. Provide deprecated shims in `recovery.rs` / `usage.rs` that emit warnings and are denied (via `#[deny(deprecated)]` or a custom lint) when called from `iteration.rs` or `wave_scheduler.rs`.
- Use a sealed trait or marker type so only the coordinator functions in `reactions/` are callable from the two engine orchestrators.
- Combination of the above.

The chosen mechanism must be prototyped enough during planning that implementers have a concrete blueprint.

### 5.3 Phase 1 Scope (Now the First Large Increment)

Phase 1 must deliver (as a single coherent unit):

- Full `reactions/` scaffolding + strong enforcement mechanism.
- `resolve_task_execution` (pre_spawn) including crash escalation + model + **effort** overrides + override invalidation.
- `react_to_outputs` (account) with rate-limit detection, `WaitedAndRetry` / `Stop` handling, `recover_in_progress_for_prefix` usage, and the injected-wait seam for hermetic tests.
- `usage_params` added to `WaveIterationParams` and wired from `orchestrator.rs`.
- Insertion of the new reaction calls in both paths (replacing the three sites in `iteration.rs` and adding the wave call after `process_slot_result`).
- Behavioral parity tests covering sequential (1-item), uniform wave (N-item), and mixed wave (some rate-limited, some completed) shapes for the rate-limit reaction, plus negative controls.
- Structural lock test that would have caught the original omission.
- The CLAUDE.md contract section.
- Migration of overflow to `reactions::post_output::handle_overflow` (or at minimum the call-site pinning + lock for #5).

This is a large first PR by loop-engine standards. Plan review time and commit strategy accordingly.

### 5.4 Relocation & Module Hygiene

- Explicit migration plan for each moved function (especially anything with transactional promotion logic or complex test dependencies in `recovery.rs`).
- Decision on whether `recovery.rs` is slimmed to near-zero or kept as a deprecated compatibility layer during transition.
- Same analysis for the relevant pieces of `usage.rs`.

### 5.5 Documentation Obligations

- New human-review behavior (review can fire on partial `WaitedAndRetry` waves) must be called out in the PRD, in `signals.rs` docs, and in the relevant human-review tests.
- The budget-accounting invariant ("RateLimit / WaitedAndRetry waves set `iteration_consumed = false`") must be stated as a one-line contract in the new CLAUDE.md section and backed by a parity test case.
- Ordering contract for `handle_overflow` relative to `process_iteration_output` must be written before implementation.

### 5.6 Testing Discipline

- All parity tests must follow the existing `iteration_pipeline_parity.rs` model: two independent DBs, `TASK_MGR_NO_EXTRACT_LEARNINGS=1`, public API setup only.
- Negative-control cases (no RateLimit → `AccountReaction::None`, zero DB writes) are mandatory.
- Mixed-wave human-review + rate-limit case should be exercised (simulated input).

---

## 6. Updated Risks and Mitigations

| Risk | Likelihood after decisions | Mitigation |
|------|---------------------------|----------|
| First PR is very large (scaffolding + enforcement + multiple reactions + moves + tests) | High | Break the PR into a tightly stacked sequence if the tooling supports it; ensure CONTRACT-001 is reviewed in isolation first. |
| Moving complex logic (`handle_prompt_too_long` with dumps/JSONL/rotation, transactional promotion paths) | Medium-High | Extract seams early; keep the existing `recovery.rs` tests passing during the move. |
| Deprecation/deny noise during transition | Medium | Decide the exact deny scope (only the two orchestrator files?) before the first code change. |
| Operator surprise from human review firing on partial waves | Low (decision already accepted) | Clear documentation + a banner explaining that other slots are still in flight. |
| Enforcement mechanism itself has maintenance cost | Medium | Choose the simplest mechanism that actually delivers compile-time prevention; document the escape hatch (if any). |
| Scope creep beyond the six items | Low (clear minimum bar chosen) | CONTRACT-001 and the CLAUDE.md section should explicitly list the six items as the current convergence target. |

---

## 7. Suggested Next Steps

1. **CONTRACT spike / design session** (highest leverage)
   - Decide and prototype the exact strong enforcement mechanism.
   - Write the precise `reactions/` public surface and module layout.
   - Draft the "Reaction framework (shared)" CLAUDE.md section with all decisions baked in.
   - Produce the CONTRACT-001 task description with full acceptance criteria.

2. Generate the full task list via the normal flow (`/prd` or `/plan-tasks` → `/tasks`), wiring all six target items as dependents of CONTRACT-001.

3. Treat the combination of:
   - The original plan document,
   - This review file, and
   - The output of the CONTRACT spike
   as the authoritative spec for the effort.

4. Accept that the first deliverable that actually closes the rate-limit bug will be large. Do not artificially split it in a way that leaves the enforcement mechanism incomplete.

---

## Appendix: Key Files Referenced in the Review

- `src/loop_engine/iteration.rs` (pre-usage gate, crash escalation, rate-limit wait sites)
- `src/loop_engine/wave_scheduler.rs` (pre-spawn loop, post-`process_slot_result` region, `wave_preflight_check`, `handle_no_eligible_tasks`)
- `src/loop_engine/orchestrator.rs` (post-pipeline human review, budget accounting, external git)
- `src/loop_engine/engine.rs` (`IterationParams`, `WaveIterationParams`)
- `src/loop_engine/recovery.rs` (`check_crash_escalation`, `check_override_invalidation`, `handle_task_failure`)
- `src/loop_engine/usage.rs` (`check_and_wait`, `wait_for_usage_reset`, `parse_reset_from_output`)
- `src/loop_engine/iteration_pipeline.rs` (the precedent module + `ProcessingParams` / `ProcessingOutcome`)
- `src/loop_engine/CLAUDE.md` (existing "Iteration pipeline (shared)" and "Drained-queue classification" sections)
- `tests/iteration_pipeline_parity.rs` (the model for the new parity harness)
- `src/loop_engine/wave_scheduler.rs` (the `defense_layer_1_*` structural lock tests)

---

**End of captured review context.**

This document should be loaded or referenced at the start of any planning or implementation session for the convergence effort.