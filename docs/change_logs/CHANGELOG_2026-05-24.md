# Changelog — 2026-05-24

## Primary Runner Routing (symmetric Claude↔Grok)

**Branch**: `feat/grok-fallback-runner`
**PRD**: `tasks/grok-fallback-runner.md`

### What shipped

A new optional `primaryRunner` block in `.task-mgr/config.json` routes designated
task classes (by `taskType` or id-prefix, e.g. `review` / `MILESTONE-`) to a
non-default runner (Grok) as the FIRST choice — the mirror of `fallbackRunner`,
which promotes a stuck Claude task to Grok as a last resort. Completing the
symmetry, a Grok-primary task that exhausts its overflow ladder or hits repeated
RuntimeErrors now falls back to Claude (`primaryRunner.claudeFallbackModel`).
Resolution precedence: explicit `task.model` → `primaryRunner` match → `difficulty=high`
→ defaults. Absent `primaryRunner` ⇒ behavior byte-identical to before.

### Why it matters

Operators can run cheaper/faster providers on high-volume task classes (reviews,
milestones) while keeping Claude for everything else, with automatic cross-provider
recovery in BOTH directions — bounded to a single provider crossing per task per
run so a flaky provider can't ping-pong.

### Breaking changes

None. The feature is opt-in and inert when `primaryRunner` is unset.

---
