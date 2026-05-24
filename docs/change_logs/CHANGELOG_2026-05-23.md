# Changelog — 2026-05-23

## Engine Orchestration Boundaries (loop_engine carve)

**Branch**: `refactor/engine-orchestration-boundaries`
**PRD**: `tasks/prd-02-engine-orchestration-boundaries.md`

### What shipped

Carved the 9,644-line `src/loop_engine/engine.rs` into five focused sibling
modules — `slot.rs` (slot lifecycle + result processing), `recovery.rs`
(per-task recovery cluster), `wave_scheduler.rs` (parallel wave + merge-back),
`iteration.rs` (sequential per-task body), and `orchestrator.rs` (outer
`run_loop` + lifecycle). `engine.rs` is now a ~1,180-line dispatcher/re-export
hub. Added a compile-time `iteration_pipeline` parity assertion and checked-in
dogfood baseline fixtures (sequential + wave `.db`/`.stderr`) so the
byte-identical-behavior gate is reproducible.

### Why it matters

The monolithic `engine.rs` was the single biggest navigation and merge-conflict
hotspot in the codebase. The carve gives parallel-slot, recovery, and
orchestration logic clear module boundaries with an enforced visibility ladder,
so future loop-engine work (and concurrent PRDs that touch the engine) edits a
focused file instead of a 9.6k-line monolith. Behavior is byte-identical —
verified against the dogfood baselines, full suite green.

### Breaking changes

None. `crate::loop_engine::run_loop` remains importable from the same path
(`pub use orchestrator::run_loop`); all public re-exports preserved.

---

