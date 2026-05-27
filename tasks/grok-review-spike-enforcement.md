# Grok Spike Review: Enforcement Mechanism for Reactions Framework

**Purpose**: Captured findings, decision, and rationale from SPIKE-001-enforcement-mechanism. This is the second review document for the sequential/wave convergence effort.

**Related Artifacts**:
- Plan: `~/.claude/plans/what-can-we-do-breezy-shore.md`
- First review: `tasks/grok-review-convergence-plan.md`
- Spike ID: `SPIKE-001-enforcement-mechanism`

**Reviewer**: Grok 4.3 (April 2026)

**Date of Spike Execution**: Immediately following user confirmation to run the spike.

---

## Executive Summary

The spike evaluated the three enforcement technique candidates defined in the plan for making bypass of the old reaction logic (from `iteration.rs` and `wave_scheduler.rs`) a hard compile error.

**Clear winner**: Candidate 1 — `#[deprecated]` + `#[deny(deprecated)]` (scoped to the two orchestrator files).

This approach:
- Directly satisfies the clarified success criteria ("sufficient to make calls to the old leaf functions a compile error").
- Leverages existing, proven patterns already in the crate (three live deprecation examples).
- Requires minimal new machinery.
- Provides a loud, reviewable signal when someone attempts to regress.
- Keeps the spike output cheap (decision + small proof-of-concept).

The exhaustive destructure pattern (Candidate 3) was validated as an excellent *secondary* practice for the new `reactions/` coordinator functions themselves, but it does not solve the primary goal of blocking calls to the legacy leaves.

**Recommended next action**: Proceed directly to defining and emitting `CONTRACT-001` using the findings in this document + the original plan.

---

## 1. Spike Target (as executed)

**Uncertain / high-impact area**:
The compile-time / visibility enforcement mechanism for `src/loop_engine/reactions/`.

**Goal of the spike**:
Determine the simplest technique that makes direct calls to the old leaf functions from the two main execution paths (`iteration.rs` and `wave_scheduler.rs`) become a hard compile error, while supporting a realistic transition.

**Clarifying questions asked (per spike process) and answers received**:

1. **Definition of "hard enforcement"**  
   Is it sufficient to block *calls to the old leaf functions*, or must we also prevent copy-pasting the logic of reactions?  
   **Answer**: Sufficient to block calls to the old leaf functions.

2. **Scope of spike output**  
   Decision + small proof-of-concept (A), or a more complete vertical slice (B)?  
   **Answer**: A — Decision + small proof-of-concept (including proposed public surface for the coordinators).

These answers kept the spike tightly scoped and cheap.

---

## 2. Exploration (Step 2 — Time-boxed & Targeted)

Only the files and symbols directly relevant to the three candidates and the clarified constraint were examined:

### Key Files / Symbols Inspected
- Leaf function definitions and visibility:
  - `recovery.rs`: `check_crash_escalation`, `check_override_invalidation`, `handle_task_failure`
  - `usage.rs`: `check_and_wait`, `wait_for_usage_reset`, `parse_reset_from_output`
  - `overflow.rs`: `handle_prompt_too_long`
- Direct call sites in the two target files (`iteration.rs` and `wave_scheduler.rs`)
- Existing deprecation patterns in the crate (3 live examples)
- `ProcessingParams` exhaustive destructure pattern in `iteration_pipeline.rs`
- Module visibility declarations in `mod.rs`
- Re-export patterns in `engine.rs` (FR-008 considerations)
- Relevant learnings via `task-mgr recall`

### Important Discoveries
- The crate already has a mature, low-ceremony deprecation culture:
  - `#[deprecated(note = "...")]` is used on shims during carve-outs.
  - `#[allow(deprecated)]` is applied locally or on specific `use` statements when the old path is still temporarily needed.
  - Example: `wave_scheduler.rs` already uses `#[allow(deprecated)]` for `claim_slot_task`.

- `recovery` is already `pub(crate)`. This is helpful.

- The exhaustive destructure pattern used by `process_iteration_output` is strong for protecting the *new* API surface from future drift, but provides **zero protection** against someone writing `use crate::loop_engine::recovery::check_crash_escalation;` in the future.

- `trigger_human_reviews` is currently a private function inside `orchestrator.rs` (not a `pub` leaf), so it has different migration characteristics.

- Existing `pub use` re-exports from `engine.rs` must remain functional for integration tests.

---

## 3. Evaluation of Candidates

| Candidate | Delivers "calls to old leaves from the two orchestrators become compile error"? | Complexity | Fits existing patterns? | Long-term maintenance | Verdict |
|-----------|----------------------------------------------------------------------------------|------------|--------------------------|-----------------------|---------|
| **1. `#[deprecated]` + `#[deny(deprecated)]` scoped to `iteration.rs` + `wave_scheduler.rs`** | Yes — directly and precisely | Very low | Excellent (3 live examples + `#[allow(deprecated)]` precedent) | Low | **Winner** |
| 2. Sealed trait / marker type | Yes (indirectly) | High | Poor | High | Rejected |
| 3. Exhaustive param-struct destructure on new coordinators | No (only protects the *new* surface) | Medium | Good (already used by pipeline) | Low (for new API) | Strong complement, not primary |

**Primary Recommendation**: Use Candidate 1 as the core enforcement mechanism.

**Secondary Recommendation**: Mandate exhaustive struct destructuring (no `..`) on all five new `reactions/` coordinator functions as a forward-looking hygiene rule. This protects the new surface from future drift (the original motivation for the pipeline precedent).

---

## 4. Recommended Enforcement Approach (Proof-of-Concept)

### Core Mechanism

Add at the top of both execution path files (after the module documentation):

```rust
// In src/loop_engine/iteration.rs and src/loop_engine/wave_scheduler.rs
#![deny(deprecated)]
```

### When relocating leaves (example)

In the source file (e.g. `recovery.rs`) during migration:

```rust
#[deprecated(
    note = "use reactions::pre_spawn::resolve_task_execution instead — \
            direct calls from iteration.rs / wave_scheduler.rs are a compile error"
)]
pub fn check_crash_escalation(...) { ... }
```

Any attempt to call the old function (or import it) from `iteration.rs` or `wave_scheduler.rs` will now produce a hard compile error.

Places that still legitimately need the shim during transition can use a targeted `#[allow(deprecated)]` (already an established pattern in the crate).

### Handling of `engine.rs` re-exports
- Keep the `pub use` statements needed for `task_mgr::loop_engine::engine::*` paths (FR-008).
- Apply `#[allow(deprecated)]` at the re-export site or in test modules as needed.
- The deny only applies inside the two execution path modules.

---

## 5. Proposed Public Surface for the Five Coordinator Functions

These should become the **only** allowed entry points from the two orchestrators.

```rust
// === reactions/pre_spawn.rs ===
pub fn resolve_task_execution(
    ctx: &mut IterationContext,
    conn: &Connection,
    task_id: &str,
    prompt_result: &PromptResult,
    project_config: &ProjectConfig,
) -> TaskExecutionPlan;   // contains effective_model, effort, runner, etc.

// === reactions/pre_spawn.rs ===
pub fn account_usage_gate(
    usage_params: &UsageParams,
    tasks_dir: &Path,
    permission_mode: &PermissionMode,
) -> GateDecision;

// === reactions/account.rs ===
pub fn react_to_outputs(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &AccountReactionParams<'_>,
) -> AccountReaction;      // None | WaitedAndRetry | Stop

// === reactions/post_output.rs ===
pub fn handle_overflow(...);   // Full signature to be finalized in first increment

// === reactions/post_completion.rs ===
pub fn react_to_completions(
    conn: &mut Connection,
    completed_ids: &[String],
    params: &PostCompletionParams<'_>,
);
```

**Additional rule (recommended)**: All five functions should internally use exhaustive destructuring of their parameter structs (no `..`), following the `ProcessingParams` precedent. This should be documented in the new "Reaction framework (shared)" section of `CLAUDE.md`.

---

## 6. Relevant Institutional Knowledge (from recall)

Several high-confidence learnings were surfaced that reinforce this approach:

- Strong existing pattern: Use `pub(crate)` for internal-only modules and `pub(super)` for more restricted helpers.
- The crate already treats deprecation shims as a normal part of module carve-outs and refactoring.
- `pub(crate)` visibility is consistently used when extracting subsystems.

The chosen approach (Candidate 1 + exhaustive destructuring on new surface) aligns cleanly with this body of knowledge.

---

## 7. Remaining Risks (Post-Spike)

1. **Transition noise** — Multiple files (especially tests and `engine.rs` re-exports) will need temporary `#[allow(deprecated)]`.  
   **Mitigation**: Keep the deny narrowly scoped to just the two execution files. Be aggressive about cleaning up shims in follow-on tasks.

2. **Other call sites for `handle_prompt_too_long`** — Notably `slot.rs:492` currently calls it directly.  
   **Note**: The clarified spike scope only required blocking the two main orchestrator paths. Explicit decision needed in CONTRACT-001 or first increment about whether `slot.rs` must also route through the reaction.

3. **Engine re-export contract (FR-008)** — Must not break external integration test paths. The recommended approach preserves this.

4. **Human factors** — Developers may be surprised by the new deny.  
   **Mitigation**: Clear error messages in the deprecation notes + update to the convergence CLAUDE.md section.

---

## 8. Concrete Recommendations

1. **Emit CONTRACT-001 immediately** using the findings in this document. The contract should explicitly name:
   - `#[deny(deprecated)]` + `#[deprecated]` as the primary enforcement technique.
   - Exhaustive struct destructuring as a required practice on all new reaction coordinators.
   - The five coordinator functions and their high-level signatures.
   - The two files that will carry the deny attribute.
   - Required updates to `src/loop_engine/CLAUDE.md` and `iteration_pipeline.rs` "Out of scope" lists.

2. **Do not** pursue sealed traits or attempt to make logic-copying a compile error (per clarified scope).

3. **Do** treat the exhaustive destructure rule as mandatory hygiene for the new module (even though it is secondary for enforcement).

4. Document the `handle_prompt_too_long` / `slot.rs` situation explicitly in the contract so it is not forgotten.

---

## 9. Suggested Next Steps

- Run `/spike` output review (this document + plan) with relevant maintainers if needed.
- Generate `CONTRACT-001` task (via `task-mgr add --stdin` or equivalent).
- Wire the six target items (#2, #3, #5, #6, #10, #13) as dependents of the contract.
- Then proceed with the "First large increment" as described in the plan.

---

## Appendix: Spike Artifacts & References

- Spike target statement and clarifying questions (in conversation history)
- Targeted exploration covered: `iteration.rs`, `wave_scheduler.rs`, `recovery.rs`, `usage.rs`, `overflow.rs`, `iteration_pipeline.rs`, `engine.rs`, `mod.rs`, `slot.rs`
- Existing deprecation sites: `recovery.rs:428`, `engine.rs:788`, `slot.rs:336`
- Relevant learnings: #3900, #2070, #1406, #444 (module visibility + deprecation patterns)

---

**End of spike review document.**

This file, together with `tasks/grok-review-convergence-plan.md` and the original plan (`~/.claude/plans/what-can-we-do-breezy-shore.md`), forms the authoritative record for the enforcement decision and should be referenced when writing CONTRACT-001 and the full task list.