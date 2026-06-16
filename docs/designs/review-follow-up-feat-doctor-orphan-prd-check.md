# Review Follow-Up: feat/doctor-orphan-prd-check

**Status:** draft  
**Date:** 2026-06-13  
**Source:** `/review-loop` re-run after `804d69f`  
**Worktree:** `feat/doctor-orphan-prd-check` @ `804d69f`  
**PRDs on branch:** `dd806025` (doctor orphan) · `b2db855c` (batch chain) · `d11f4aa9` (cost-efficient auxiliary)

---

## Summary

| Metric | Count |
|--------|-------|
| Findings inventoried | 20 unique (18 + 2 deduped) |
| **CONFIRMED** | 16 |
| **INVALID** | 1 (BAT-L2) |
| **WONT_FIX** | 1 (AUX-L2) |
| **DOWNGRADE_TO_LOW** | 1 (AUX-L3) |
| **Duplicate (merged)** | 2 (X-M1 → DOC-M2, X-M2 → DOC-L5) |

### Recommended pre-merge (P1)

| ID | Module | Title |
|----|--------|-------|
| DOC-M1 | Doctor | Fix `would_fix` field doc |
| DOC-M2 | Doctor | Multi-slash branch unit test |
| DOC-M3 | Doctor | Mixed dry-run integration test |
| BAT-L4 | Batch | Orchestrator exit-code doc rewrite |
| AUX-L1 | Aux LLM | Neutralize "Claude iteration" arg doc |

### Recommended post-merge / needs-decision

| ID | Module | Title |
|----|--------|-------|
| BAT-M2 | Batch | **`prd_complete` terminal semantics — product decision required** |
| BAT-M1 | Batch | Batch accounting (blocked on BAT-M2) |
| BAT-L3 | Batch | `run_batch` + mock e2e integration test |
| DOC-L4, DOC-L2 | Doctor | UX polish (remediation dedup, verbose numbering) |
| AUX-L3 | Aux LLM | Optional Grok/Codex argv tests |

### Document-only / P2

BAT-M3, BAT-L1, DOC-L1, DOC-L3, DOC-L5, AUX-L3

---

## Validation ledger

### Doctor (`dd806025`)

| ID | Sev | Verdict | Evidence |
|----|-----|---------|----------|
| DOC-M1 | Medium | **CONFIRMED** | `output.rs:70-74` doc vs `mod.rs:232-239` orphan always in `would_fix` |
| DOC-M2 | Medium | **CONFIRMED** | `checks.rs:210-215` logic present; no `feat/sub/feature` test |
| DOC-M3 | Medium | **CONFIRMED** | Orphan-only dry-run test exists; no stale+orphan mixed test |
| DOC-L1 | Low | **CONFIRMED** | `mod.rs:130` "stale PRD" vs PRD spec "stale import" |
| DOC-L2 | Low | **CONFIRMED** | Check 4 conditional; Check 5 always shown (`output.rs:165-198`) |
| DOC-L3 | Low | **CONFIRMED** | "No changes were made" only after auto-fix dry-run block |
| DOC-L4 | Low | **CONFIRMED** | Fix embedded in `Issue.description` and `would_fix.action` |
| DOC-L5 | Low | **CONFIRMED** | No `orphan_branch_prd` JSON test (unlike `test_git_reconciliation_serialization`) |
| X-M1 | Medium | **CONFIRMED** (dup DOC-M2) | — |
| X-M2 | Medium | **CONFIRMED** (dup DOC-L5) | — |

### Batch (`b2db855c`)

| ID | Sev | Verdict | Evidence |
|----|-----|---------|----------|
| BAT-M1 | Medium | **CONFIRMED** | `batch.rs:696-700` counters before chain gate; `exit_code` only |
| BAT-M2 | Medium | **CONFIRMED** | `wave_scheduler.rs:346-351` blocked/skipped terminal; drain semantics differ |
| BAT-M3 | Medium | **CONFIRMED** | `wave_scheduler.rs:333-340` SQL failure → 0; pre-existing, not doctor-introduced |
| BAT-L1 | Low | **CONFIRMED** | `wave_scheduler.rs:434-436` omits Layer 2 `prd_complete` |
| BAT-L2 | Low | **INVALID** | Test helper mirrors production predicate exactly (`batch.rs:712` vs `1260`) |
| BAT-L3 | Low | **CONFIRMED** | No `run_batch` in `tests/`; structural tests only |
| BAT-L4 | Low | **CONFIRMED** | `orchestrator.rs:74-79` stale vs reconcile behavior |

### Cost-efficient auxiliary (`d11f4aa9`)

| ID | Sev | Verdict | Evidence |
|----|-----|---------|----------|
| AUX-L1 | Low | **CONFIRMED** | `ingestion/mod.rs:73` "Claude iteration" |
| AUX-L2 | Low | **WONT_FIX** | FEAT-003 AC requires warn only; extraction debug is FEAT-002 scoped |
| AUX-L3 | Low | **DOWNGRADE_TO_LOW** | `runner.rs:2260` Claude-only argv test; functional contract satisfied |

### Cross-cutting

| Item | Verdict |
|------|---------|
| `804d69f` fixes intact | ✅ All 7 items present on HEAD |
| BAT-M2/M3 on `wave_scheduler.rs` | Pre-existing batch-chain inheritance; doctor PRD did not touch file |
| BAT-L2 | INVALID — intentional test mirror, not drift |

---

## Doctor module (`dd806025`)

### DOC-M1 — `would_fix` field doc wrong

- **Priority:** P1
- **Approach:** Update `DoctorResult` field docs in `output.rs:70-74` to describe print-only orphan path: `would_fix` populated for (a) dry-run auto-fixable types, (b) `OrphanBranchPrd` whenever `effective_auto_fix`, regardless of `dry_run`.
- **Files:** `src/commands/doctor/output.rs`, optional `mod.rs` docblock
- **Tests:** None (doc-only)
- **Risks:** Low
- **Effort:** S (~15 min)

### DOC-M2 — Multi-slash branch test missing

- **Priority:** P1
- **Approach:** Add `doctor_ignores_prd_with_remote_multi_slash_branch`: seed `branch_name = 'feat/sub/feature'`, `git update-ref refs/remotes/origin/feat/sub/feature HEAD`, assert `find_orphan_branch_prds` empty.
- **Files:** `src/commands/doctor/tests.rs`
- **Tests:** New unit test + optional negative control without `update-ref`
- **Risks:** Low — documents existing `split_once` logic
- **Effort:** S (~20 min)

### DOC-M3 — Mixed dry-run test missing

- **Priority:** P1
- **Approach:** Add `doctor_dry_run_mixed_auto_and_manual_remediation`: stale in-progress task + orphan PRD, `doctor(true, true, ...)`, assert `[DRY RUN] Would fix 1` and `Manual remediation required for 1` partitions.
- **Files:** `src/commands/doctor/tests.rs`
- **Tests:** Assert `would_fix.len() == 2`, partition text, DB unchanged
- **Risks:** Low; may surface DOC-L3 footer gap
- **Effort:** S (~30 min)

### DOC-L1 — "stale PRD" vs "stale import"

- **Priority:** P2
- **Approach:** Change `mod.rs:130` and `test_format_text_orphan_branch_prd` to "stale import".
- **Files:** `src/commands/doctor/mod.rs`, `src/commands/doctor/tests.rs`
- **Effort:** S

### DOC-L2 — Verbose Check 4/5 numbering gap

- **Priority:** P2
- **Approach:** Refactor `format_doctor_verbose` to dynamic check numbering (counter instead of hardcoded Check 4/5).
- **Files:** `src/commands/doctor/output.rs`, `tests.rs`
- **Tests:** Update `test_format_doctor_verbose_orphan_branch_prd`; add reconcile+orphan combo test
- **Risks:** Medium — verbose output label change
- **Effort:** M (~1 hr)

### DOC-L3 — Orphan dry-run footer

- **Priority:** P2
- **Approach:** After manual remediation block in dry-run, append `No changes were made. Manual steps above require operator action.`
- **Files:** `src/commands/doctor/output.rs`, `tests.rs`
- **Effort:** S

### DOC-L4 — Duplicate remediation text

- **Priority:** P2
- **Approach:** Remove embedded `Fix:` from `Issue.description`; show remediation once via `would_fix` / check-only inline in `format_text`.
- **Files:** `mod.rs`, `output.rs`, `tests.rs`
- **Risks:** Medium — check-only vs `--fix` paths
- **Effort:** M (~1 hr)

### DOC-L5 — JSON serialization test

- **Priority:** P2
- **Approach:** Add `test_orphan_branch_prd_serialization` mirroring `test_git_reconciliation_serialization`.
- **Files:** `src/commands/doctor/tests.rs`
- **Tests:** Assert `"orphan_branch_prd"`, `"orphan_branch_prds":1`
- **Effort:** S (~15 min)

**Doctor implementation order:** DOC-M1 → DOC-M2, DOC-M3, DOC-L5 → DOC-L1, DOC-L4 → DOC-L2, DOC-L3

---

## Batch chain module (`b2db855c`)

### BAT-M2 — `prd_complete` terminal semantics (NEEDS DECISION)

- **Priority:** P0 decision gate (blocks BAT-M1 final shape)
- **Approach:** Product chooses what "complete" means for chain gate. Do not implement until decided.

| Option | Predicate | Chain behavior |
|--------|-----------|----------------|
| **A — Status quo** | `count_remaining_active_tasks == 0` (blocked/skipped terminal) | Current Layer 2 |
| **B — Success-only** | Only `done` + `irrelevant` | Chain stops on blocked/skipped |
| **C — Dual flags** | Keep `prd_complete`; add `prd_successful` | Chain uses success flag |
| **D — Align archive** | Match `is_prd_completed_by_prefix` (blocked non-terminal) | Consistent with archive |

- **Recommendation:** Option C — preserves tests, fixes accounting without overloading one boolean.
- **Files (post-decision):** `wave_scheduler.rs`, `engine.rs`, `orchestrator.rs`, `batch.rs`, `loop_engine/CLAUDE.md`
- **Effort:** Decision S; implementation M

### BAT-M1 — `succeeded`/`failed` use `exit_code` only

- **Priority:** P1 (implementation blocked on BAT-M2)
- **Approach:** Introduce `prd_run_succeeded(loop_result)` helper; minimum interim fix: `exit_code == 0 && !prd_complete` → failed. Store `prd_complete` on `PrdRunResult` for summary. Reorder accounting after chain-break evaluation.
- **Files:** `batch.rs`, `main.rs` (~1402-1404)
- **Tests:** `test_incomplete_exit_zero_counts_as_failed`, extend sequence test
- **Risks:** Behavior change for non-chain batch runs with budget exhaustion
- **Effort:** M

### BAT-M3 — SQL failure returns 0

- **Priority:** P2 — document-only
- **Approach:** Add "Failure modes" subsection in `src/loop_engine/CLAUDE.md`; optional strengthen `tracing::warn!` with `task_prefix`. Do not change `count_remaining_tasks` return type.
- **Files:** `loop_engine/CLAUDE.md`, optional `wave_scheduler.rs` doc
- **Effort:** XS (~30 min)

### BAT-L1 — Stale reconcile comment

- **Priority:** P2
- **Approach:** Update `orchestrator.rs:680-682` comment to align with Layer 2 wording (snapshot before step 17.5 reset).
- **Files:** `orchestrator.rs` (comment only)
- **Effort:** XS (~15 min)

### BAT-L3 — No `run_loop` + `run_batch` e2e

- **Priority:** P2
- **Approach:** Pragmatic mock-layered test — **not** full agent e2e. Add `tests/batch_chain_integration.rs` reusing `e2e_loop` mock binary pattern: two PRDs, `chain=true`, first exhausts budget with todo remaining, assert second `skipped=true`. Mark `#[ignore]` for nightly/local.
- **Files:** `tests/batch_chain_integration.rs`, `tests/fixtures/batch-chain/`
- **Effort:** M (1 PR)

### BAT-L4 — Orchestrator exit-code doc stale

- **Priority:** P1
- **Approach:** Rewrite `orchestrator.rs:74-79` exit-code docblock: budget/deadline with remaining tasks → 1 post-reconcile; cross-ref `prd_complete`; fix SIGINT/SIGTERM mapping (130 on `LoopResult`).
- **Files:** `orchestrator.rs`, optional `engine.rs` field doc
- **Effort:** S (~1 hr)

**Batch implementation order:** BAT-M2 decision → BAT-L4 + BAT-L1 + BAT-M3 (parallel) → BAT-M1 → BAT-L3

---

## Cost-efficient auxiliary LLM (`d11f4aa9`)

### AUX-L1 — Stale "Claude iteration" arg doc

- **Priority:** P1
- **Approach:**
  1. `ingestion/mod.rs:73` → `Raw output from a loop iteration (any primary provider)`
  2. `cli/commands.rs` ExtractLearnings help — neutralize "Claude output" wording
- **Files:** `src/learnings/ingestion/mod.rs`, `src/cli/commands.rs`
- **Tests:** `cargo check`; grep gate for stale phrase
- **Risks:** Low — doc only
- **Effort:** S (~15 min)

### AUX-L2 — Milestone debug log

- **Priority:** WONT_FIX
- **Rationale:** FEAT-003 AC requires `tracing::warn` on failure only; debug spawn logging is FEAT-002 scoped to extraction. Milestone runs at low frequency; parity is optional polish for a future observability PRD.
- **Files:** None
- **Effort:** 0

### AUX-L3 — Claude-only `dispatch_auxiliary` argv test

- **Priority:** P2 (optional backlog)
- **Approach:** Add Grok argv integration test reusing `make_grok_argv_recorder`; pure `build_codex_argv` assertion for Codex effort omission. Extract shared `assert_auxiliary_argv_invariants` helper.
- **Files:** `src/loop_engine/runner.rs` tests
- **Risks:** Per-provider assertions must differ; mutex ordering
- **Effort:** M (~45-90 min)

**Aux implementation order:** AUX-L1 now · AUX-L2 record WONT_FIX · AUX-L3 defer

---

## Suggested execution order (all modules)

1. **Quick wins (parallel, ~2 hr):** DOC-M1, DOC-M2, DOC-M3, DOC-L5, AUX-L1, BAT-L4, BAT-L1, BAT-M3
2. **Decision meeting:** BAT-M2 (Option A/B/C/D)
3. **Post-decision:** BAT-M1 batch accounting
4. **Polish backlog:** DOC-L1–L4, DOC-L2/L3, BAT-L3 e2e, AUX-L3

---

## Out of scope for this follow-up

- Implementing fixes (this document is the plan only)
- `task-mgr add` task spawning (optional after user approves sections)
- `/compound` forward-looking capture
- BAT-L2 (invalid finding — no action)

---

## References

- Design: [`batch-chain-incomplete-stop.md`](batch-chain-incomplete-stop.md)
- Review fix commit: `804d69f` — `fix: address review-loop high/medium findings`
- Task prefixes: `dd806025`, `b2db855c`, `d11f4aa9`