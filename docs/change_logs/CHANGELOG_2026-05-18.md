# Changelog — 2026-05-18

## Grok Fallback Runner for task-mgr Loop

**Branch**: `feat/grok-fallback-runner`
**PRD**: `tasks/prd-grok-fallback-runner.md`

### What shipped

A 5th rung on the overflow-recovery ladder and a RuntimeError fallback hook that promote terminally-failing tasks from Claude to Grok when the Claude ladder is exhausted, plus a new `LlmRunner` trait + `RunnerKind` enum that abstracts subprocess dispatch with static enum-match dispatch (no `Box<dyn>`). Default disabled; opt-in via `.task-mgr/config.json -> fallbackRunner.enabled: true`. Includes an operator escape valve that clears all six per-task auto-recovery channels when the operator edits `tasks.model` out of band.

### Why it matters

Previously the loop dead-ended on `PromptTooLong` once the 4-rung Claude ladder reached Opus[1M] at high effort, and burned iterations forever on `RuntimeError` after the Opus ceiling was hit. Grok 4 Fast's ~2M context and different reasoner profile unsticks both cases without operator intervention. All existing `spawn_claude` callers continue to compile unmodified (preserved via type aliases), so the trait extraction is byte-identical to today's behavior when fallback is disabled or absent.

### Quality + review

- 39 commits across the branch; 7965+ insertions across 46 files including ~1200 lines in the new `src/loop_engine/runner.rs`, comprehensive test coverage in 12 new test files, and full library suite green (1708 passed, 1 ignored).
- `/review-loop` independent pass surfaced 5 medium findings (W1–W5) — all addressed in commit `05a5ae7` with regression tests, and the W3/W5 patterns documented in `src/loop_engine/CLAUDE.md`.
- Forward-looking learnings captured in this session: deferred-ctx-after-commit pattern, startup-probe-must-mirror-runtime invariant, stderr-sniff-runbook discipline, assert-vs-debug-assert for drift sentinels.

### Breaking changes

None. `fallbackRunner` config defaults to `None`; absent or `null` resolves to disabled. All 10 existing `spawn_claude` call sites compile unchanged via `SpawnOpts`/`ClaudeResult` type aliases. The new `RecoveryAction::FallbackToProvider` variant and `OverflowEvent.runner: Option<String>` field are additive-only on the JSONL telemetry surface.

---
