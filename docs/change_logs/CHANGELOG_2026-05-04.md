# Changelog — 2026-05-04

## Prompt-Overflow Recovery Escalation + Diagnostics

**Branch**: `feat/overflow-recovery-and-diagnostics`
**PRD**: `tasks/prd-overflow-recovery-and-diagnostics.md`

### What shipped

A third recovery rung in the loop engine's `PromptTooLong` handler so Sonnet-default
loops escalate to Opus instead of blocking on iteration 1, plus diagnostics: per-section
byte breakdown in `PromptResult`, on-disk prompt dumps to
`.task-mgr/overflow-dumps/<task>-iter<n>-<ts>.txt` (kept N=3 newest per task), a JSONL
event log at `.task-mgr/overflow-events.jsonl`, and a banner annotation when a task is
mid-recovery. Stderr messages now have four distinct phrasings (one per rung) instead of
the misleading "effort floor + 1M model exhausted" string.

### Why it matters

Before this change, a Sonnet+high task that overflowed was immediately blocked: the two
existing checks (`downgrade_effort`, `to_1m_model`) were both no-ops for Sonnet, leaving
zero recovery path. The loop made no forward progress without manual model swaps. The
diagnostics layer also gave users the first concrete signal (per-section byte breakdown,
dropped sections, dump of the actual prompt) for diagnosing what made a prompt too long
— previously a single stderr line and a wedged task.

### Breaking changes

None. Additive struct fields on `PromptResult` (`section_sizes`) and `IterationContext`
(`overflow_recovered`, `overflow_original_model`); new `pub fn` in `model.rs`
(`escalate_below_opus`); new module `src/loop_engine/overflow.rs`; new `display.rs`
extracted for unit-testable banner formatting. Parallel slot/wave mode is unchanged —
overflow recovery is sequential-only by deliberate design (slot prompts are minimal, so
overflow is unreachable in practice; documented at `engine.rs:487-488`).

---

## Milestone Soft-Dep Guard

**Branch**: `feat/milestone-soft-dep-guard`
**PRD**: `tasks/milestone-soft-dep-guard-prompt.md` (+ `tasks/milestone-soft-dep-guard.json`)

### What shipped

A two-layer fix preventing parallel-slot waves from dispatching milestone tasks while
spawned-fixup siblings (`REFACTOR-N`, `CODE-FIX`, `WIRE-FIX`, `IMPL-FIX`) are still
active. (A) Selection-side soft-dep filter in `build_scored_candidates` defers
milestone-class candidates whose acceptance criteria reference a known fixup prefix
while a same-prefix `todo`/`in_progress` sibling exists in the same PRD; sibling
fixups remain co-schedulable. Token-aware exact-prefix matching with mandatory
trailing dash so `CODE-FIXTURE-1` never collides with `CODE-FIX`. (B) Prompt-side
teaching in `task_ops_section()` instructs the loop agent to pass
`--depended-on-by <milestone-id>` when spawning a fix in response to a milestone's AC.

### Why it matters

A real wave on `cbd7d081-MILESTONE-FINAL` wasted ~75 minutes because the spawning
task `REFACTOR-REVIEW-FINAL` created `REFACTOR-N-001`/`-002` children without
`--depended-on-by`, leaving no `task_relationships` row. `select_parallel_group`
saw all formal `dependsOn` deps satisfied and dispatched the milestone alongside
its still-active fixups. The slot self-detected the miss via the milestone's AC
text and emitted `<promise>BLOCKED</promise>`, but only after three stale
iterations. The new filter catches the case at selection time; the prompt-side
teaching addresses the root cause.

### Breaking changes

None. Public signatures of `select_next_task` and `select_parallel_group` are
unchanged — only the internal `build_scored_candidates` filter pipeline gets a
new stage. `SPAWNED_FIXUP_PREFIXES` is a private const; new ad-hoc-spawn task
types are added by extending this slice.

---
