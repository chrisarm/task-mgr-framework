# PRD: Multi-Provider Review Cascade (Deferred Follow-Up)

**Type**: Feature
**Priority**: P2 (after model-selection redesign ships and stabilizes)
**Author**: Claude Code
**Created**: 2026-06-09
**Status**: **Deferred / Backlog** — do NOT run through /prd-tasks until `tasks/prd-model-selection-redesign.md` has completed its loop, passed `/review-loop`, and the provider-first routing has been exercised on at least one real PRD.
**Depends on**: `tasks/prd-model-selection-redesign.md` (provider-first config, capability tiers, `ExecutionPlan` resolution, provider stamping `tasks.completed_by_provider` + `run_tasks.provider/model` — the stamping ships there precisely so this PRD starts with real implementer history).

---

## 1. Overview

Each review-class task (CODE-REVIEW-*, MILESTONE-FINAL, REVIEW-*) should be reviewed by **every enabled provider in succession**: provider A reviews → findings spawn fix tasks → fixes complete → provider B reviews the post-fix state → ... If a round spawns no fix tasks, the next reviewer fires immediately. Provider-diverse review catches provider-specific blind spots. Engine-owned (not generator-emitted): adapts to the enabled-provider set at runtime, no PRD bloat.

This design was fully specified and architect-reviewed inside the model-selection redesign effort (blocking issues B2 resolved: restart idempotency + PRD-sync safety verified against `src/commands/init/import.rs` — the `--append --update-existing` upsert path never deletes DB-only rows; deletion lives only in `drop_existing_data`). It was extracted into this follow-up to keep the foundation refactor focused.

## 2. Design (carried over, ready for re-validation at implementation time)

### State: `review_rounds` side table (new migration, v21+)

```sql
CREATE TABLE review_rounds (
    review_task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    cascade_root_id TEXT NOT NULL,      -- round-1 review task id (groups the cascade)
    round INTEGER NOT NULL,             -- 1-based
    provider TEXT NOT NULL,             -- pinned reviewing provider ('claude'|'grok'|'codex')
    spawned_fix_ids TEXT,               -- JSON array of FULL prefixed task ids
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT,
    archived_at TEXT DEFAULT NULL
);
CREATE INDEX idx_review_rounds_root ON review_rounds(cascade_root_id);
```

Side table (not tasks columns) so PRD-sync rewrites can't touch it. Down: `DROP TABLE review_rounds`. Migration discipline: 3 edits in migrations/mod.rs (#503), tests via `run_migrations` (#1550).

### Config: `routing.reviewCascade`

```json
"reviewCascade": { "enabled": true, "providers": ["claude", "codex", "grok"], "crossProvider": true }
```

`crossProvider: true` → rotation prefers next provider ≠ implementer of the work under review (dominant `completed_by_provider`); `false` → plain rotation order. All-providers-once-each semantics either way. (The foundation PRD's validator reserves this key with a "not yet supported" rejection so a premature config doesn't silently no-op — this PRD removes that rejection.)

### Mechanics (engine-owned)

- **Trigger**: new step in `post_completion::react_to_completions_inner` (the converged coordinator, both paths) with an injected `SpawnFn` seam (mirrors `ReviewFn`/`WaitFn`). Runs AFTER the external-git completion shadow; consumes ONLY the provided `completed_ids` set (input-driven, never re-queries).
- **Per completed review-class id R**: lazy round-1 registration (provider = R's `completed_by_provider`); discover fixes (`tasks.source_review = R`, fallback heuristic capped to PRD prefix + `SPAWNED_FIXUP_PREFIXES` + dependency edge); pick next provider via pure `select_next_reviewer(order, used, implementer)`; spawn successor `<root>-R<k+1>` via `add::add_with_conn` with **forward `dependsOn = fixes ∪ {R}`** (never inverted — spawned-fixup deadlock class; destination PRD pinned per learning #2236).
- **Idempotency**: successor spawn is a no-op iff a `review_rounds` row exists for `(cascade_root_id, round+1)` — checked in the same transaction as the insert. Replayed completions (restart) spawn exactly one successor.
- **Zero-fix round**: successor's deps already satisfied → eligible immediately (falls out of normal selection).
- **Provider pin**: resolution rung between explicit `tasks.model` and `routing.byIdPrefix` — review-class tasks read `review_rounds.provider`; successor runs at that provider's **highest defined tier** (frontier where defined; grok's default ladder tops at standard/grok-build — deliberate, banner names provider+model+tier so the downgrade is visible).
- **Rotation edge cases**: provider disabled mid-cascade → recompute from currently-enabled set each round; NULL implementer (pre-stamping tasks) → rotation order without ≠ preference; exactly-once beats cross-provider when unavoidable.
- **Lifecycle**: hard task delete cascades via FK; PRD archive stamps `review_rounds.archived_at` in the same pass that archives tasks. `doctor` gains an orphaned-successor check with fix hint.
- **Module layout**: `src/loop_engine/review_cascade.rs` — pure logic + accessors (CONTRACT task first, hermetic unit tests: restart replay, provider-disabled, NULL-implementer, zero-fix), then engine wiring.

### Carried-over edge cases

| Edge Case | Expected Behavior |
| --- | --- |
| Review round spawns ZERO fix tasks | Successor immediately eligible |
| Loop restart after spawning `-R2` but before it runs | `(root, round)` idempotency row → no duplicate |
| `--append --update-existing` sync while `-R2` exists (DB-only) | Survives (upsert never deletes — verified import.rs:26); regression test |
| Provider disabled mid-cascade | Rotation recomputes from enabled set |
| Fix task ends `blocked`/`skipped` mid-cascade | Successor gated like any blocked dependency; stuck-drain names it; operator unblocks (accepted v1) |
| Agent omits `sourceReview` on spawned fixes | Fallback heuristic (capped); discovered set logged in cascade banner; prompt nudge added to review-task prompts |
| Cascade cost (N frontier-ish reviews per PRD) | `reviewCascade.providers` shrinks rotation |

### Punts (carried over)

No nested cascades (fix tasks never start cascades; only PRD-authored round-1 reviews do). `review_rounds` not exported by `task-mgr export` (audit-only).

## 3. Re-validation checklist before implementing

- [ ] Re-confirm `react_to_completions_inner` step ordering against the post-redesign coordinator shape.
- [ ] Re-confirm the resolution-rung insertion point in the shipped `resolve_execution_plan` (rungs are named by constant, not ordinal, per the foundation PRD).
- [ ] Check accumulated `completed_by_provider` history quality (stamping shipped in the foundation PRD).
- [ ] Re-run `task-mgr recall` for new learnings from the foundation loop before generating tasks.
