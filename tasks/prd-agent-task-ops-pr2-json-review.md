# JSON review: agent-task-ops-pr2

**Status**: pass 2 — nothing material (2026-09-09)
**Pass 2 reviewer**: `01a088ae-79e5-7fc3-91da-23d5a5f24131`

---

## Pass 1

**Date**: 2026-09-09
**Reviewer**: md-to-json-prd-reviewer (`01a088a7-3588-7703-8c97-4567d7fe1c7b`)

## Summary

- **Total tasks:** 14
- **Critical issues:** 0
- **Warnings:** 3 (apply)

1. **FEAT-002:** Overlay `difficulty`/`estimatedEffort` writes JSON `estimatedEffort` and removes leftover `difficulty`. Optionally `dependsOn: ["CONTRACT-001", "CONTRACT-002"]`. Restate on FEAT-004: either key → `SET difficulty`.
2. **FEAT-003:** Add `src/commands/mod.rs` to `touchesFiles` with `pub mod update` only (no writer).
3. **CODE-REVIEW-1:** Production SQL never SET `status`/`archived_at`/`priority`/`id`; tests may seed `archived_at`.
