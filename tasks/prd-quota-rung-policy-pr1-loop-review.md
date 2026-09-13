# Loop review: quota-rung-policy PR-1

Date: 2026-09-07
Reviewer: rust-python-code-reviewer `01a07b79-62e1-72b1-989c-660d597b6468`
Worktree: `/home/chris/Documents/startat0/Projects/task-mgr` branch `feat/quota-rung-policy-pr1` HEAD `587e1b1`

**Status**: REQUEST CHANGES (1 High, 2 Medium, 2 Low)

## Loop Review: Quota buckets PR-1

### Code Review Summary

- **Files reviewed**: 8 production/test files (`usage.rs`, `detection.rs`, `account.rs`, `oauth.rs`, `wave_scheduler.rs`, `reaction_parity.rs`, CLAUDE.md ×2)
- **Critical findings**: 0
- **High findings**: 1
- **Medium/Low findings**: 2 medium, 2 low

Prior High (mixed-wave first-vs-any) and Mediums (live load_usage_info; UTF-8 slice) at FIX-002 land are **closed** by CODE-FIX-001/002/003.

#### High

1. **`src/loop_engine/reactions/account.rs:290-296`** — Hyphen treated as word boundary, so `claude-opus-5` / `claude-fable-5` match tokens `opus`/`fable`. Full CLI stdout for an ordinary session RateLimit almost always contains the model id and the word `limit` → false 3600 Wait, skip probe, never Blackout. Isolated negatives (`hit your limit · resets 4pm`, `reached your session limit`) do not hold once the rest of the capture is present. Confirmed: `"use claude-opus-5 for this task; you hit a rate limit later"` matches.

   **Fix:** Require a real word boundary (start/end/whitespace, **not** `-`) *or* the phrase `reached your (fable|opus|sonnet|haiku) limit`. Fixture: `claude-opus-5` + `You've hit your limit · resets 4pm` in one string must **not** take 3600 / must still Blackout under spillover.

#### Medium

2. **`account.rs:276-278`** — Unanchored `contains("switch models")` on entire stdout. Restrict to the rate-limit sentence / same line as `reached`/`limit`.

3. **`usage.rs:308`** — `reset_at` gate-relevant bar is compile-time 92, not `LoopConfig::usage_threshold`. Pass the live threshold into parse (or select `reset_at` in `check_and_wait`).

#### Low (do not block this fix cycle)

4. Prefer-rung-scoped can mask `StopSpend` in mixed waves.
5. `decide_account_rate_limit` rustdoc stale vs spend/output_secs.

### Coherence Assessment

- **PRD alignment**: PARTIAL — FR-001 fold matches; FR-002 classify matches; FR-002 coordinator 3600 is a unit-test illusion against full CLI captures (human review item 5 asked to narrow to model token + limit, implementation treats hyphen as a boundary so model **ids** match).
- **Deviations found**: hyphen-boundary; whole-stdout `switch models`.
- **Cross-PRD contract status**: dual predicates intact; no `quota.rs`; no remaining rename.

### Action Items

- CODE-FIX-004 (high) hyphen/word-boundary
- CODE-FIX-005 (medium) `switch models` line-scope
- CODE-FIX-006 (medium) pass live threshold into `reset_at` selection
- Reset REVIEW-001 and re-loop
