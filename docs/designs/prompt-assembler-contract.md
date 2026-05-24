# Prompt Assembler Contract (Phase 2 / Item 3)

Spike-validated design context for *Data-Driven Prompt Construction*
(`docs/designs/coherence-refactoring.md` §3). This is the stable contract the
downstream per-section migration stories implement against.

## [2026-05-23] — SPIKE

- **Hypothesis**: one `PromptSection` abstraction + a shared `PromptAssembler`
  can serve BOTH `prompt/sequential.rs` and `prompt/slot.rs`, retiring the
  hand-enforced "any new section MUST also be wired through slot" rule
  (`prompt/mod.rs`) — without breaking the `!Send` `Connection` constraint, the
  `TOTAL_PROMPT_BUDGET` cap + dropped-section accounting, or the genuine
  per-path section differences.
- **Cheapest falsifier (run)**: a throwaway `assembler_spike.rs` (approach C,
  fn-pointer spec table) wired into `prompt/mod.rs`, type-checked against the
  REAL section signatures (`build_dependency_section(conn, &task.id)`,
  `core::build_learnings_block(conn, task, budget)`, `task_ops_section()`),
  with `try_fit_section` reused verbatim and `const`-assert `Send` proofs on
  `SectionSpec` and the render fn pointer. `cargo build` passed; file removed.
- **Result**:
  - **(a) Send/Connection — NOT a blocker.** The assembler runs entirely on the
    main thread and returns owned Strings; `Send` is a property of the OUTPUT
    bundle, not the assembler. fn-pointer specs are `Send` for free (const-asserted).
  - **(b) budget/drops — trivial.** The existing `try_fit_section` drops into a
    shared Phase-2 loop unchanged.
  - **(c) heterogeneous inputs / per-path rosters — handled.** A single
    `PromptContext<'a>` (closed set of main-thread-available inputs) + a per-path
    ordered `Vec<SectionSpec>` (slot's roster ⊂ sequential's) type-checked
    against critical, trimmable+conn, and side-output sections.
  - **Two contract details surfaced** (spike simplified them): trimmables need
    their **budget** at render time, and the roster must encode a **display order
    distinct from the render phase** (criticals like `completion`/`base_prompt`
    render in Phase 1 but emit LAST).
- **Decision**: B — emit `CONTRACT-001` (below). Hand off to `/plan-tasks`.
- **CONTRACT emitted**: CONTRACT-001 (Prompt assembler section contract).
- **Key learning**: the `!Send` fear was unfounded — the assembler is a
  main-thread function; the real design work is display-order-vs-phase and the
  learnings side-output invariant, both centralizable wins.

---

## Approaches & Tradeoffs

| Approach | Shape | Pros | Cons |
|---|---|---|---|
| **A. `dyn PromptSection`** | `Vec<Box<dyn PromptSection + Send>>` | per-section state/config; extensible | heap + dyn dispatch per section; must add `+ Send`; object-safety ceremony |
| **B. enum + match** | `enum Section {…}`, `match` in `render` | exhaustive match = compile-time drift protection | one giant match; budget/order data lives outside the enum |
| **C. fn-pointer spec table** ✅ | `SectionSpec { name, kind, render: fn(&Ctx,…)->Rendered }`, `Vec<SectionSpec>` | simplest; `Send` for free; roster = plain ordered data both paths share; no dyn/heap | fn-ptr can't capture per-call config (budget threaded via `kind`); no built-in exhaustiveness |

**Recommendation: C**, plus a **completeness test** (template:
`tests/iteration_pipeline_parity.rs` and the runner `CHECKS`-table completeness
guard) asserting every known section name appears in the sequential roster —
recovering B's exhaustiveness as a test instead of the type system.

**Top-2 residual risks**
1. *Display-order vs render-phase.* Contract separates "render phase" (driven by
   `SectionKind`) from "emit order" (driven by roster position). Assembler
   renders criticals first for the budget gate, stores results keyed by name,
   emits in roster order.
2. *Byte-identical parity during migration.* Migrate ONE section at a time
   behind a parity test asserting `assemble()` output == legacy builder output
   for that section. Pilot `dependencies` first (trimmable + `conn`, in both
   rosters).

---

## CONTRACT-001 — Prompt assembler section contract

**Owning module (proposed):** `src/loop_engine/prompt/assembler.rs` (+ section
specs registered alongside the existing `prompt_sections/*`). `prompt/core.rs`
helpers stay; they become the render-fn bodies.

### Interface (approach C)

```rust
pub struct PromptContext<'a> {
    pub conn: &'a Connection,
    pub task: &'a Task,
    pub task_files: &'a [String],
    pub project_root: &'a Path,
    pub base_prompt_path: &'a Path,
    pub permission_mode: &'a PermissionMode,
    pub steering_path: Option<&'a Path>,
    pub session_guidance: &'a str,
    pub run_id: Option<&'a str>,
    pub task_prefix: Option<&'a str>,
    // sequential-only inputs as Option<…>; slot leaves them None:
    pub reorder_hint: Option<&'a str>,
    pub batch_sibling_prds: Option<&'a [PathBuf]>,   // real input to build_sibling_prd_section
}

// NOTE (corrected after the md-to-json PRD review, 2026-05-23): an earlier draft
// of this struct named phantom types `SiblingPrd` / `SynergyCluster` that do not
// exist in the crate. The real signatures are:
//   build_sibling_prd_section(conn, task_id, task_prefix, batch_sibling_prds: &[PathBuf])
//   build_synergy_section(conn, task_id, run_id)  // currently a permanent no-op:
//                                                  // returns String::new() (synergy
//                                                  // relationships were dropped in favour
//                                                  // of runtime file-overlap detection).
// The synergy section migrates as a dead-but-present section behind a parity test.

pub enum SectionKind { Critical, Trimmable { budget: usize } }

#[derive(Default)]
pub struct Rendered { pub text: String, pub shown_learning_ids: Vec<i64> }

pub struct SectionSpec {
    pub name: &'static str,                       // stable id; matches section_sizes keys
    pub kind: SectionKind,
    pub render: fn(&PromptContext, SectionKind) -> Rendered, // kind carries the budget
}

pub struct Assembled {
    pub prompt: String,
    pub section_sizes: Vec<(&'static str, usize)>,
    pub dropped_sections: Vec<String>,
    pub shown_learning_ids: Vec<i64>,
}

pub fn assemble(ctx: &PromptContext, roster: &[SectionSpec], total_budget: usize) -> Assembled;
```

### Invariants every implementation + caller MUST maintain
- **Single render site per section.** A section's text is produced in exactly
  one `render` fn; both paths reach it only via the roster. No section text is
  inlined in `sequential.rs`/`slot.rs` after migration.
- **Roster = display order, PER PATH.** `assemble` emits sections in roster
  order, regardless of render phase. Criticals are rendered first (budget gate)
  but emitted in their roster position. **Each path supplies its OWN ordered
  `Vec<SectionSpec>`** — the slot roster is a set-SUBSET of the sequential roster
  but is *independently ordered* (today slot emits `task` first, sequential emits
  it mid-list; they do NOT share a relative order). The parity unit is therefore
  the **per-section rendered text** (`Rendered.text == legacy builder output`)
  PLUS **whole-prompt bytes per path** (each path's `assemble()` output ==
  that path's own legacy whole-prompt output, because each roster preserves its
  own legacy order). There is no single global section order; do not try to make
  one roster reproduce both paths' byte layouts.
- **Critical-overflow translation (sequential).** `assemble` reports criticals
  overflow uniformly via `dropped_sections == ["CRITICAL"]`. The sequential
  `build_prompt` caller MUST translate that back into
  `Err(TaskMgrError::PromptOverflow{..})` so `overflow::handle_prompt_too_long`'s
  five-rung ladder is unchanged. The slot caller keeps today's sentinel-in-bundle
  behavior. The two paths have different overflow CONTRACTS even though `assemble`
  signals overflow the same way — the translation lives in each caller, not in
  `assemble`.
- **Critical-overflow sentinel preserved.** If criticals alone exceed
  `total_budget`, return `Assembled` with empty `prompt` and
  `dropped_sections == ["CRITICAL"]` (today's `CRITICAL_OVERFLOW_SENTINEL`).
- **Learnings side-output invariant (centralized).** `shown_learning_ids` is
  populated only for sections that fit; when the `learnings` section is dropped,
  the assembler clears its ids (today duplicated in both builders — now owned
  by `assemble`).
- **`TOTAL_PROMPT_BUDGET` parity.** slot and sequential pass the same
  `total_budget` (80_000) so the aggregate cap stays identical.
- **`SectionSpec: Send`.** fn-pointer specs are `Send`; a roster may be
  referenced while building a `SlotPromptBundle`. No `&Connection` is ever
  stored in `Assembled`/`SlotPromptBundle`.
- **`bundle.task_id == task.id`** and the existing slot bundle field contract
  are unchanged — only the assembly internals move.

### Known-bad (a wrong impl that still passes naive tests)
- Emits sections in render-phase order (criticals last → all-criticals-first),
  passing a "contains every section" test but producing a **different byte
  layout** than today. Parity test against the legacy builder is the catch.
- Forgets to clear `shown_learning_ids` when `learnings` is trimmed → bandit
  gets credited for learnings the agent never saw (UCB skew). Silent.
- Threads budget via `PromptContext` instead of `SectionKind` → every trimmable
  shares one budget field and the per-section budgets (`LEARNINGS_BUDGET=4000`,
  `SOURCE_CONTEXT_BUDGET=2000`) collapse.
- Builds one shared roster for both paths and tries to make it reproduce both
  byte layouts simultaneously → the slot and sequential paths have **different
  section orders** (today slot emits `task` first; sequential emits it mid-list)
  so a single roster produces an incorrect byte layout for at least one path.
  Each path MUST supply its OWN independently ordered `Vec<SectionSpec>`; the
  slot roster is a set-subset of the sequential roster but is *not* a positional
  sub-sequence.

### Edge cases (from the spike + current code)
- Empty `task_files` → source section empty (not an error).
- Missing base prompt / steering file → empty section, warn, continue.
- Section render returns `""` → `try_fit_section` must NOT push the name into
  `dropped_sections` (only non-empty-but-too-large drops count).

### Downstream impact (stories that will `dependOn` CONTRACT-001)
- **FEAT-001**: assembler + `PromptContext` + `SectionSpec` skeleton in
  `assembler.rs` (engine, types, `assemble()` loop, isolated unit tests).
- **FEAT-002**: pilot-migrate `dependencies` through the assembler in BOTH paths
  + byte-parity test; establishes the per-path roster pattern.
- **FEAT-003**: migrate the critical sections shared by both paths (task,
  task_ops, completion, base_prompt) + sequential overflow translation
  (`dropped == ["CRITICAL"]` → `Err(TaskMgrError::PromptOverflow)`).
- **FEAT-004**: migrate the `learnings` section (Trimmable) + centralize
  `shown_learning_ids` side-output (remove duplicated clears from builders).
- **FEAT-005**: migrate the remaining shared trimmables (source/source-context,
  steering, session_guidance, tool_awareness, key_decision).
- **FEAT-006**: migrate sequential-only sections (synergy, escalation, siblings,
  reorder-hint); confirms slot roster ⊂ sequential roster as a SET.
- **FEAT-007**: delete the hand-enforced wiring rule from `prompt/mod.rs` AND
  from `src/loop_engine/CLAUDE.md`; add roster-completeness test; mark Item 3
  done in `docs/designs/coherence-refactoring.md` §3.
- REFACTOR-001 / REVIEW-001 milestones.
