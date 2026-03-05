# PRD: Fix import_learnings Bugs

**Type**: Bug Fix
**Priority**: P3 (Low)
**Author**: Claude Code
**Created**: 2026-02-19
**Status**: Draft

---

## 1. Overview

### Problem Statement
The `import-learnings` command has five bugs that make it unreliable and misleading. Two CLI flags (`--reset-stats`, `--learnings-only`) are no-ops that advertise non-existent behavior. Imports are not transactional, risking partial writes. A foreign key violation crashes imports when the source DB has task IDs not present in the target. MD5 is used unnecessarily for simple in-memory deduplication.

### Background
The export/import learnings pipeline enables transferring institutional memory between projects. `export --learnings-file` produces a JSON array of learnings; `import-learnings --from-json` ingests them. The pipeline was built but not fully wired — several features were stubbed but never connected.

---

## 2. Goals

### Primary Goals
- [ ] `--reset-stats` actually controls whether bandit statistics are preserved or zeroed
- [ ] `--learnings-only` flag removed (run history import was never implemented)
- [ ] Import is atomic — all-or-nothing via transaction
- [ ] No FK violation crash when importing learnings with source-DB task IDs
- [ ] MD5 removed from dedup path (use direct string key)

### Success Metrics
- All existing import_learnings tests pass after refactor
- New tests cover stats preservation, task_id nullification, atomicity, within-batch dedup
- `cargo run -- import-learnings --help` shows no `--learnings-only` flag

---

## 2.5. Quality Dimensions

### Correctness Requirements
- Imported learnings must exactly preserve all fields from the export (outcome, confidence, title, content, root_cause, solution, applies_to_files, applies_to_task_types, applies_to_errors, tags)
- When `--reset-stats` is NOT passed, times_shown/times_applied/last_shown_at/last_applied_at must match the export values
- When `--reset-stats` IS passed (or by default), stats start at zero
- Deduplication must prevent both cross-DB duplicates AND within-batch duplicates
- A failed import must leave the database unchanged (transaction rollback)

### Performance Requirements
- Best effort — import sizes are typically < 100 learnings

### Style Requirements
- Follow existing codebase transaction pattern (`conn.transaction()` / `tx.commit()`)
- Use SQLite datetime format `%Y-%m-%d %H:%M:%S` (not RFC 3339) to match `parse_datetime`
- No `.unwrap()` on fallible operations

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| Learning with `task_id` referencing non-existent task | FK violation crashes import | Set task_id to None (same as run_id) |
| `last_shown_at` is None but `times_shown > 0` | Inconsistent but valid export data | Preserve as-is (NULL datetime, non-zero count) |
| Duplicate learnings within same import file | Only DB-level dedup existed | Detect and skip within-batch duplicates |
| Empty import file `[]` | Degenerate input | 0 imported, 0 skipped, success |
| Existing scripts passing `--learnings-only` | CLI breaking change | Error on unknown flag (flag removed) |
| Import interrupted mid-batch | Partial data corruption risk | Transaction rollback — no partial writes |
| DateTime format mismatch | Codebase uses `%Y-%m-%d %H:%M:%S` only | Format exported datetimes in SQLite format |

---

## 3. User Stories

### US-001: Preserve bandit statistics on import
**As a** user migrating learnings between projects
**I want** imported learnings to retain their times_shown/times_applied history
**So that** the bandit ranking reflects real-world effectiveness, not just recency

**Acceptance Criteria:**
- [ ] Default import (no `--reset-stats`) preserves times_shown, times_applied, last_shown_at, last_applied_at from export
- [ ] `--reset-stats` flag zeroes all four fields
- [ ] Output text indicates whether stats were preserved or reset

### US-002: Remove misleading --learnings-only flag
**As a** user reading the help text
**I want** the CLI to only advertise capabilities that exist
**So that** I don't waste time with flags that do nothing

**Acceptance Criteria:**
- [ ] `--learnings-only` flag removed from CLI definition
- [ ] `--learnings-only` removed from help examples
- [ ] `learnings_only` field removed from `ImportLearningsResult`
- [ ] `import_learnings()` function signature drops the parameter

### US-003: Atomic imports via transaction
**As a** user importing learnings
**I want** a failed import to leave my database unchanged
**So that** I don't end up with partial data

**Acceptance Criteria:**
- [ ] All inserts wrapped in a single SQLite transaction
- [ ] If any insert fails, all prior inserts in the batch are rolled back
- [ ] Successful imports commit atomically

### US-004: Handle task_id foreign key safely
**As a** user importing learnings from a different project
**I want** imports to succeed even when source task IDs don't exist in target DB
**So that** cross-project learning transfer works reliably

**Acceptance Criteria:**
- [ ] `task_id` set to None on import (mirrors existing `run_id` handling)
- [ ] No FK violation errors during import

### US-005: Replace MD5 with direct string dedup
**As a** maintainer
**I want** unnecessary crypto dependencies removed from simple code paths
**So that** the codebase is simpler and has fewer dependencies to audit

**Acceptance Criteria:**
- [ ] `md5::compute` call removed from `compute_learning_hash` (renamed to `compute_dedup_key`)
- [ ] Dedup uses `format!("{}:{}", title, content)` as HashSet key
- [ ] md5 crate remains in Cargo.toml (still used by `loop_engine/engine.rs`)
- [ ] Within-batch duplicates also detected (not just DB-level)

---

## 4. Functional Requirements

### FR-001: Stats preservation via post-insert UPDATE
When `reset_stats` is false, after each `record_learning` insert, execute:
```sql
UPDATE learnings SET times_shown = ?, times_applied = ?,
  last_shown_at = ?, last_applied_at = ? WHERE id = ?
```
Using values from the `LearningExport` struct. Datetimes formatted as `%Y-%m-%d %H:%M:%S`.

### FR-002: Transaction wrapping
`load_existing_hashes` called before `conn.transaction()` (avoids mutable borrow conflict). Insert loop runs inside transaction. `tx.commit()` at end.

### FR-003: Within-batch dedup
Convert `existing_hashes` into a mutable `seen` set. After each dedup check, insert the new key. `seen.insert(key)` returns false if already present → skip.

---

## 5. Non-Goals (Out of Scope)

- **Run history import** — Would require FK handling for runs→tasks, complex schema mapping. Separate feature.
- **Removing md5 crate from Cargo.toml** — Still used by `loop_engine/engine.rs`.
- **Dedup key collision resistance** — `"a:" + "b"` vs `"a" + ":b"` is the same collision class as MD5. Not a regression. Not worth a separator escape scheme for this use case.

---

## 6. Technical Considerations

### Affected Components
- `src/commands/import_learnings/mod.rs` — All 5 fixes
- `src/commands/import_learnings/tests.rs` — Update existing + new tests
- `src/cli/commands.rs` — Remove `--learnings-only` arg + example
- `src/main.rs` — Remove `learnings_only` from dispatch

### Dependencies
- `rusqlite::Transaction` — auto-derefs to `&Connection`, so `record_learning(&tx, params)` works without changing the `record_learning` API
- `chrono::DateTime::format()` — for SQLite-format datetime strings

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| Post-insert UPDATE for stats | No API change to `record_learning`; simple | Extra SQL per learning | **Preferred** |
| Modify `record_learning` to accept stats | Single INSERT with all fields | Touches stable internal API; all callers affected | Rejected |
| Batch UPDATE after loop | Fewer SQL calls | Need to track all IDs; more complex | Rejected |

**Selected Approach**: Post-insert UPDATE. Keeps `record_learning` unchanged, stats logic is contained in import_learnings.

### Risks & Mitigations
| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| CLI breaking change (--learnings-only removal) | Low — scripts using the flag break | Low — flag never worked | Accept: flag was always a no-op |
| DateTime format mismatch in UPDATE | Med — stats datetimes unparseable | Med — easy to get wrong | Use `%Y-%m-%d %H:%M:%S` explicitly; test round-trip |
| Transaction scope too large for big imports | Low — SQLite handles large txns well | Low — imports are typically small | Accept: single txn is correct semantics |

### Security Considerations
- No new attack surface — import reads a local file the user specifies
- FK violation fix (setting task_id to None) loses provenance but prevents crashes

### Public Contracts

#### Modified Interfaces
| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|----------------|-------------------|-------------------|-----------|-----------|
| `import_learnings()` | `(dir, from_file, learnings_only, reset_stats)` | `(dir, from_file, reset_stats)` | Yes (internal) | Remove `learnings_only` from call sites |
| `ImportLearningsResult` | Has `learnings_only: bool` field | Field removed | Yes (JSON output) | Remove field references |
| CLI `import-learnings` | Has `--learnings-only` flag | Flag removed | Yes (CLI) | Remove from scripts |

### Consumers of Changed Behavior
| File:Line | Usage | Impact | Mitigation |
|-----------|-------|--------|------------|
| `src/main.rs:661-667` | Destructures `learnings_only`, passes to `import_learnings()` | BREAKS | Remove from destructure and call |
| `src/commands/mod.rs:65` | Re-exports `ImportLearningsResult` | OK | Struct change is internal |

---

## 7. Open Questions

- None — all questions resolved during review.
