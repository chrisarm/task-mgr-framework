# JSON review: agent-task-ops-pr2

**Status**: pass 4 — nothing material (2026-09-18)
**Pass 4 reviewer**: `01a0b6f8-8cf3-71d3-8023-cbc04c3e573d`
**Pass 3**: five apply warnings folded; zero remaining apply items. List is loop-ready.

---

## Pass 4 (2026-09-18)

**Status**: pass 4 — nothing material
**Reviewer**: md-to-json-prd-reviewer (`01a0b6f8-8cf3-71d3-8023-cbc04c3e573d`)

Five pass-3 apply items landed (FEAT-001 `prd_json.rs`; FEAT-007 tests-only `touchesFiles`; FEAT-008 clone-source out of `touchesFiles`; sandbox vs checkout CLAUDE.md; JSON-only `invalid_state` ≠ `--no-prefix`). Critical 0, warnings 0, apply pass not required.

---

## Pass 3 (2026-09-18) — applied

**Reviewer**: md-to-json-prd-reviewer (`01a0b6f2-2674-7131-b173-fb3ac918d61b`)
**Critical**: 0
**Warnings (apply)**: 5 — FEAT-001 add `prd_json.rs` to `touchesFiles`; FEAT-007 drop `update.rs`; FEAT-008 drop clone-source from `touchesFiles`; FEAT-008 CLAUDE.md proof is checkout not sandbox; FEAT-008 spell no-`task_list` vs `--no-prefix`
**Tasks-author apply pass:** folded before pass 4.

---

## Pass 3 (2026-09-18)

Sources: `tasks/prd-agent-task-ops-pr2.md` (architect APPROVED 2026-09-18), `tasks/agent-task-ops-pr2.json`, `tasks/agent-task-ops-pr2-prompt.md`, `.claude/skills/verify-task-mgr/SKILL.md`. Historical pass 2 “nothing material” (2026-09-09) is **not** current truth.

### Summary

- **Total tasks:** 14 (CONTRACT-001–003, FEAT-001–008, CODE-REVIEW-1, REFACTOR-REVIEW-FINAL, REVIEW-001)
- **Tasks needing revision:** 3 (FEAT-001, FEAT-007, FEAT-008)
- **Critical issues:** 0
- **Warnings:** 5 (all **apply**)
- **Suggestions:** 5 (**ignore** unless a cheap fold)
- **PRD story coverage:** complete (US-001…008 mapped)
- **PR-3 scope leak:** none
- **HEAD law (2026-09-18):** present on the matching FEATs
- **Tasks-author apply pass:** fold the five apply warnings.

HEAD law table (extract preflight; write-path `default_prd_roots` → `resolve_context_with_roots`; `choose_cli_write_path`; CONTRACT-002 type split; US-007 live-path names; US-008 clones `add-and-current-from-json.md`; leftover `"add"` includes preflight; mixed+empty pin 11; write-only rustdoc; no `use add`; verify skill FEAT-008 + REVIEW-001 only) — **On FEAT**.

### Warnings (apply)

1. **[FEAT-001]** Add `src/commands/prd_json.rs` to `touchesFiles` (test literals only; do not land `patch_user_story` here).
2. **[FEAT-007]** Drop `src/commands/update.rs` from `touchesFiles`. Keep `tests/worktree_db_resolution.rs` and `tests/add_integration.rs`.
3. **[FEAT-008]** Remove `features/add-and-current-from-json.md` from `touchesFiles`.
4. **[FEAT-008]** Sandbox drive does not prove in-tree `CLAUDE.md`; that proof is checkout after FEAT-006.
5. **[FEAT-008]** Spell JSON-only `invalid_state` as missing/empty write path — **not** `--no-prefix`. Keep separate `--no-prefix` update-succeeds. ≥2 refuse remains two prefixed `loop init`s.

### Suggestions (ignore)

FEAT-002/004 sizing keep atomic; FEAT-008 dependsOn FEAT-007 soft; CODE-REVIEW-1 optional `main.rs`; deprecated relationship arrays / TEST-xxx / SEC-xxx ignore.

### Apply vs ignore

| ID | Finding | Apply? |
| --- | --- | --- |
| FEAT-001 | Add `prd_json.rs` to `touchesFiles` | **apply** |
| FEAT-007 | Drop `update.rs` from `touchesFiles` | **apply** |
| FEAT-008 | Drop clone-source feature file from `touchesFiles` | **apply** |
| FEAT-008 | CLAUDE.md proof is checkout, not sandbox | **apply** |
| FEAT-008 | Spell no-`task_list` vs `--no-prefix` | **apply** |
| FEAT-002 / FEAT-004 sizing | Keep atomic | **ignore** |
| FEAT-008 dependsOn FEAT-007 | Soft dep | **ignore** |
| CODE-REVIEW-1 `main.rs` | Optional | **ignore** |
| Relationship arrays / TEST-xxx / SEC-xxx | Not this skeleton | **ignore** |

---

## Historical — pass 2 nothing material (2026-09-09)

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
