# JSON review: agent-task-ops-pr3

**Status**: pass 2 — nothing material (2026-09-18)
**Pass 2 reviewer**: `01a0b713-c936-7722-8669-c7d375108193`
**Pass 1**: two criticals + four apply warnings folded.

---

## Pass 2 (2026-09-18)

**Verdict:** Pass-1 criticals and apply-warnings landed. Nothing material remains. Tasks-author apply pass: not required. List is loop-ready (do **not** loop until PR-2 is merged).

Landed: FEAT-001 `export/mod.rs` + 4-arg dump-all AC; FEAT-002 `main.rs` + ExportOpts forward; FEAT-002 CLI-test notes; FEAT-007 Update-absent gate; FEAT-010 sibling-only; FEAT-012 no SKILL.md in `touchesFiles`.

---

## Pass 1 (2026-09-18) — applied

**Status**: pass 1 — apply required (2026-09-18)
**Reviewer**: md-to-json-prd-reviewer (`01a0b70b-ac27-70b2-9392-ab956ae5fb22`)
**Critical**: 2 (FEAT-001 `export/mod.rs` arity; FEAT-002 `main.rs` `ExportOpts`)
**Warnings (apply)**: FEAT-002 note on `--no-prefix` CLI tests; FEAT-007 cheatsheet_drift gate; FEAT-010 sibling-file only; FEAT-012 drop `SKILL.md` from `touchesFiles`

---

## Pass 1 (2026-09-18)

**Verdict:** Pins 8/9/18, architect fold, create-then-drive, and scope lock are encoded well. Two compile-window gaps would fail `cargo check` at the end of FEAT-001 / FEAT-002. **Tasks-author apply pass: required.**

### Summary

- **Total tasks:** 17
- **Tasks needing revision:** 5 (FEAT-001, FEAT-002, FEAT-007, FEAT-010, FEAT-012)
- **Critical issues:** 2
- **Warnings:** 7 (4 apply, rest ignore split)

### Apply vs ignore

| Finding | Severity | Apply? |
| --- | --- | --- |
| FEAT-001 new `load_*` signatures vs `export/mod.rs` still 1-arg | Critical | **Apply** |
| FEAT-002 `ExportOpts` vs `main.rs` still 4-arg `export()` | Critical | **Apply** |
| FEAT-002 11 ACs / 7 files | Warning | **Ignore split** |
| FEAT-001 10 ACs | Warning | **Ignore split** |
| FEAT-003 9 ACs | Warning | **Ignore split** |
| CLI `--no-prefix` tests red between FEAT-002 and FEAT-003 | Warning | **Apply** as a one-line FEAT-002 note only |
| FEAT-007 `cheatsheet_drift` vs PR-2 Update clap | Warning | **Apply** — make the gate the required path |
| FEAT-010 writes `tests/worktree_db_resolution.rs` | Warning | **Apply** — sibling file only |
| FEAT-012 `touchesFiles` includes `SKILL.md` | Warning | **Apply** — drop SKILL.md |
| No TEST-INIT / TEST-xxx | Warning | **Ignore** |
| Empty `requires` for PR-2 | Warning | **Ignore** as a `requires` entry |
| Missing SEC-xxx / FEAT-006 dependsOn / relationship arrays | Suggestion | **Ignore** |

### Criticals (must fold)

1. **FEAT-001:** Add `src/commands/export/mod.rs` to `touchesFiles`. Extra AC: `export()` still 4-arg dump-all; call `load_tasks(&conn, None)` + unscoped metadata (`LIMIT 1`); no `ExportOpts`, no lock, no `unique_tmp`.
2. **FEAT-002:** Add `src/main.rs`. Forward `ExportOpts { from_json: None, all: false, force: false, … }` from the still-3-field clap. No `LockGuard` on the Export arm. Do not grow clap. Do not batchWith FEAT-003.

### Warnings (fold)

- FEAT-002 notes: `human_review_cli` / `model_fields_cli` / `test_export_roundtrip` wait for FEAT-003; do not restore smash default.
- FEAT-007: if `Commands::Update` is absent, name `update --stdin --from-json` in prose; only emit clap-parsed flag tokens that exist on this binary.
- FEAT-010: `touchesFiles` = `tests/worktree_export_dest.rs` only; `worktree_db_resolution.rs` is read-only pattern.
- FEAT-012: drop `SKILL.md` from `touchesFiles`; drive it, do not edit it.

Do not start a PR-3 loop until the two criticals are patched.
