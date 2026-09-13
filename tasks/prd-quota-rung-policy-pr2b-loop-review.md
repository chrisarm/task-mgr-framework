# Loop Review: Quota rung policy PRE-PR-3 (2b)

**Worktree:** `/home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-quota-rung-policy-pr2b`
**Branch:** `feat/quota-rung-policy-pr2b` (`jq -r .branchName` from `tasks/quota-rung-policy-pr2b.json`)
**HEAD:** `dd2f78d` vs `main` (`dacccb3`)
**Commits beyond main:** 16 (CONTRACT-002 → FEAT-008/009 → FIX-010/011/012 → DOCS-001 → CODE-REVIEW-1 → REVIEW-001)
**Uncommitted:** `tasks/progress-a593d39e.txt`, `tasks/quota-rung-policy-pr2b.json` (loop status bookkeeping only; not product code)
**Reviewed:** 2026-09-07 (headless `/review-loop`; rust-python-code-reviewer + inline PRD coherence)

## Summary

PRE-PR-3 (2b) lands HUD-family extra-mark identity union, unlabeled `seven_day_*` with `rungs: None`, wait-driving probe after apply, `AccountReaction::{OperatorStopped, StopSpend}`, evaluate-time extra_usage Ignore, `has_review = is_frontier_class`, remaining-min `> 100` preflight, and pin-optional docs. All PRE-PR-3 known-bads are inverted; dual Anthropic I/O predicates, Fable 3600 skip, and `WaitFn`/`UsageGateFn` signatures are unchanged. PR-3 work (`--use-other-models-ttl`, down-only walker, inherit / `account_quota_stopped`) is correctly absent. No correctness, security, or contract holes that block merge.

**Verdict: CLEAN**

## Code Review Summary

- **Files reviewed:** 12 product/source files (`src/loop_engine/{config,iteration,merge_resolver,project_config,quota,usage,wave_scheduler}.rs`, `reactions/{account,pre_spawn}.rs`, `tests/reaction_parity.rs`, `CLAUDE.md`, `src/loop_engine/CLAUDE.md`) plus CONTRACT-002 in `tasks/progress-a593d39e.txt`
- **Critical findings:** 0
- **High findings:** 0
- **Medium findings:** 0
- **Low / residual:** 4 (documented; none require a CODE-FIX)

## Critical

None.

## High

None.

Prior PR-2 High (named `seven_day_opus` / `seven_day_sonnet` family-token-map onto standard / cost-efficient, which quota-emptied mixed work) is closed: `ingest_named_sibling` never calls `map_unlabeled_token` (`usage.rs:356-380`); live-shaped `evaluate_quota` unavailable is `{(Claude, Frontier)}` only with and without the frontier→opus pin (`usage.rs:2292-2330`, `pre_spawn.rs:466-506`, `account.rs:4446+`).

## Medium

None.

## Low / residual risks

1. **`wait_probe_lifted` vacuous `.all()`** — `src/loop_engine/usage.rs:971-983`. Scoped-only lift is “every nonempty-rungs bucket remaining `> floor`, or missing percent.” An empty filtered set is vacuously `true`. Production HUD Wait always carries the Fable `limits[]` row, so the live-shaped week-45% pin holds (`usage.rs:1710-1726`, `account.rs:3805-3889`). Hole: explicit `onLow: wait` on `seven_day_opus` (`rungs: None`) with no HUD scoped row, or a second load that is org-only (`oauth_json: None` → empty `info.buckets`). Spec wording is vacuous-true; fail-closed would be “no nonempty-rungs buckets → do not lift.” Not in the known-bad list; do not treat as a merge blocker.

2. **CLI phrase tokens in `account.rs`** — `RUNG_MODEL_TOKENS` still contains `fable`/`opus`/`sonnet`/`haiku` for PR-1 RateLimit phrase matching. CONTRACT-002 grep scope is `quota.rs` / `engine.rs` (both clean). Intentional ingest/detection, not engine state.

3. **All-high / review / explicit-frontier** still horizon-Stops when only frontier is left. Documented as PR-3 clamp. Pin is correctly optional for mixed standard/medium after extra-mark.

4. **Fable CLI 3600** still sleeps the whole wave if a Fable-routed task actually spawns (`LOOP_USAGE_CHECK_ENABLED=false`, usage fetch fail, explicit `tasks.model`). Documented residual; Fable skip of `usage_gate` / `probe_rate_limit_lifted` is untouched (`account.rs:570-628`).

## Coherence Assessment

- **PRD alignment:** FULL (PRE-PR-3 slice US-008–US-012 / FR-009–FR-011 / CONTRACT-002)
- **Deviations found:** none vs this gate. PR-3 items (`--use-other-models-ttl`, down-only walker, three clamp sites, expiry map, inherit / `account_quota_stopped`, `models set-usage-rule` / `set-tier-fallback`) are correctly out of this JSON and not implemented.
- **Cross-PRD contract status:** CONTRACT-002 is recorded under `## CONTRACT-002` in `tasks/progress-a593d39e.txt`. Downstream extra-mark / unlabeled / probe / banner code matches the identity-set union (always family constant plus snapshot id; not `exact_model_for(mapped_rung)`; not prefer-id).

### Story spot-check

| Story | Status | Evidence |
| --- | --- | --- |
| US-008 / CONTRACT-002 extra-mark identity union | satisfied | `extra_mark_for_hud_tier` builds `I = {canonical_model_for_hud_tier(R)} ∪ {scope.model.id?}` (`usage.rs:574-595`); helper takes identity set (`usage.rs:602-625`); Fable HUD + pin → frontier only (`usage.rs:2359-2394`); Opus HUD + snapshot id + pin → standard and frontier (`usage.rs:2400-2427`); Opus HUD no pin → standard only (`usage.rs:2431-2446`) |
| US-009 unlabeled named siblings | satisfied | `ingest_named_sibling` `rungs: None` (`usage.rs:369-379`); live-shaped evaluate frontier-only; snapshot `other_rungs_runnable` + exclude keeps medium/standard with and without pin |
| US-010 wait-driving probe | satisfied | `Wait { secs, account_binding }` (`account.rs:1184`); `wait_probe_lifted` after apply in `run_preflight_wait` (`account.rs:1845-1878`); scoped-only + week 45% does not lift; mixed session+scoped `account_binding: true`; `Wait { secs: 0 }` ready-now; post-output `WaitFn = Fn(u64) -> bool` (`account.rs:256`) and `UsageGateFn = (u8, &Path, u64)` (`account.rs:58`) unchanged |
| US-011 OperatorStopped vs StopSpend | satisfied | `Stop` deleted; sequential Empty triples (`iteration.rs:877-912`, `account.rs:191-207`); wave both exit 0 via `account_stop_wave_mapping` (`wave_scheduler.rs:1103-1120`, `account.rs:211-227`); spend scan before prefer-rung (`account.rs:667-682`); mixed Fable+spend → StopSpend; mixed Fable + hit-your-limit → Wait 3600 |
| US-012 hygiene | satisfied | `has_review = is_frontier_class` (`account.rs:1933-1936`); extra_usage Ignore before amount-exhausted AccountLow (`quota.rs:291-300`) then dropped from `is_spend_kind` (`account.rs:1453-1455`); remaining-min `> 100` at `preflight_validate_and_probe` names `LOOP_USAGE_REMAINING_MIN` (`project_config.rs:1074-1096`); post-output banner via `react_to_outputs` closure over `params.models` (`account.rs:509-513`) |
| DOCS-001 pin optional | satisfied | `CLAUDE.md` Fable pin section; `src/loop_engine/CLAUDE.md` PRE-PR-3 block. No “pin required for parallel/wave”. All-high clamp still named PR-3. No `--use-other-models-ttl` in clap (`src/**/*.rs` comments only) |

### Known-bads (inverted)

| Known-bad | Status | Evidence |
| --- | --- | --- |
| extra-mark keys on `exact_model_for(primary)` — Fable HUD + pin marks standard | closed | identity union; Fable+pin → frontier only |
| `ingest_named_sibling` calls `map_unlabeled_token` | closed | `rungs: None`; `limits[]` unlabeled ids still map |
| `account_quota_preflight` builds `usage_suggests_lifted` before apply | closed | probe built in `run_preflight_wait` after apply using `Wait.account_binding` |
| `iteration.rs` `== Stop` → RateLimit / `operator_stopped: false` | closed | exhaustive match → Empty triples; orchestrator Empty + `operator_stopped` → exit 0 `was_stopped`; Empty without → quota soft-stop exit 0 (`orchestrator.rs:714-725`) |
| `wave_scheduler.rs` every Stop → exit 130 | closed | both variants exit 0; StopSpend `was_stopped: false`, reason `"usage/spend limit"` |
| `amount_exhausted` AccountLow before extra_usage Ignore | closed | Ignore at `evaluate_one` first |
| `has_review` via `id.contains("REVIEW")` | closed | `is_frontier_class`; `REFACTOR-REVIEW-FINAL` false; claimed `CODE-REVIEW-1` true |

### Quality / security (held)

- Dual Anthropic I/O: pre-gate `execute_account_action = usage_params.enabled` (env ∧ Claude); post-output `anthropic_account_io_allowed` = Claude enabled only; env still gates the usage-API leg (`iteration.rs:844-850`, `wave_scheduler.rs:1059-1065`, `account.rs:612-628`).
- Seq/wave share the inner; `QuotaPreflightParams` / `AccountReactionParams` exhaustive destructure, no `..` (`account.rs:553-568`, `1740-1753`).
- `handle_rung_only_empty_selection` still before `handle_quota_deferral`.
- No unwrap on API JSON; malformed buckets skipped (`usage.rs:333-349`).
- No secrets, no injection, no remaining-min `> 100` silent-accept on loop/batch.
- `quota.rs` / `engine.rs` have no `fable` / `claude-fable-5` literals.

## Action Items

- None for merge of this PRE-PR-3 gate.
- Residual Low #1 (`wait_probe_lifted` vacuous `.all()`) is optional hardening for PR-3 if an explicit `onLow: wait` on unlabeled named siblings is ever used without a HUD scoped row.
- Do not spawn CODE-FIX / WIRE-FIX from this review (no Critical findings).
- `/compound` was **not** run (headless review instructions). Review is clean; a human may run `/compound` to capture forward-looking learnings.

## Nits (not findings)

- Sequential OperatorStopped / StopSpend arms in `iteration.rs:877-912` are duplicated except the mapping; a single `|` arm would match the wave pattern.
- `extra_mark_rungs_matching` is `pub(crate)` with a single production call site (`usage.rs:602`); can be private.
- Stale comment in `quota.rs:78-79` still says serde “lands with FEAT-008”; `UsagePolicy` already lives on `ProjectConfig` (PR-2). Harmless.
