# Human review: prd-quota-rung-policy (post architect pass 2)

Date: 2026-09-06
Source: operator review against usage.rs, reactions/account.rs, model.rs, engine.rs, pre_spawn.rs, detection.rs.

PR-1 slice is well specified; architect fold is sound. Blocking contradictions are in PR-2/PR-3 semantics.

Operator instruction: **Fix items 1 through 4 before `/prd-tasks`.** Items 5–8 and Medium may be folded during task authoring (fold them into the PRD in the same pass so the JSON cannot re-encode the contradictions).

## Blocking (spec contradictions)

### 1. Default is auto-downgrade, but the PRD says it isn't

US-006: `tierFallback` unset means no automatic downgrade.
FR-004: frontier low + other rungs runnable + no `tierFallback` → `ask`.
FR-005: `ask` with TTL 0 (the default) continues on working rungs — “implicit downgrade for this run” that does not require `tierFallback`.

Under pure defaults every frontier task, reviews included, drops to standard immediately, and `includeReview` / `includeForced` never gate anything.

**Picked truth (operator, given pin 1 “use standard”):** make it honest.

- Default `tierFallback.maxDifficulty: high`
- Default `includeReview: true`
- `ask`-continue applies the **same eligibility** as `tierFallback` so the two knobs cannot disagree
- If the operator forbids downgrade (unset / narrower `maxDifficulty` / `includeReview: false`), TTL expiry **defers**, it does not continue

### 2. Horizon table hole between 1h and 12h

FR-004: wait for reset ≤ 60m; stop for reset > 12h. A session reset in 3h (the most common case) matches neither row. Same gap for rung-scoped rows.

**Picked truth:** add the middle band explicitly: **wait, capped at `MAX_WAIT_SECS`**. Note that with `stopIfResetBeyondHours: 12` and a 5h cap, the **5h-to-12h band is still a cap-and-repark cycle**.

### 3. Rung keys + “HUD label wins” reopen the shared-model hole

After the PR-1 recipe pins frontier to the standard model, an Opus HUD row marks only **standard** unavailable. Frontier still resolves to the same exhausted model and hits the CLI wall on a 3600s loop. FR-003 currently blesses this.

Honor pin 2 (no model ids in engine state) and close it: at ingest, after mapping display name to a rung, **also mark every rung whose configured model string equals that rung’s model**. Output is still `(Provider, CapabilityTier)`.

This **replaces** the earlier “shared binary model must not mark every matching rung unavailable.”

### 4. Off-ladder explicit pins cannot be deferred by the mechanism described

US-006: `includeForced=false` “defers pinned off-ladder ids”. `tier_of` returns `None` for an off-ladder id; `tasks.model: claude-fable-5-1` at medium difficulty gets tier standard, is not blacked, and dispatches to Fable every cycle.

**Picked truth (option A):** allow the ingest family match against the **explicit model string at resolve time**. Do not drop the AC and document a wait loop.

## High (fold into PRD; may land with task authoring)

5. `reached your` ∧ `limit` is too broad and ignores the API. If Claude phrases a session or weekly-all hit as “You've reached your session limit”, FR-002 would discard the real `api_secs` and sit out an account outage. Narrow the 3600 override to a **model token** (`fable|opus|sonnet|haiku`) followed by `limit`, **or** co-occurrence with “switch models”. Plain `reached your … limit` is ordinary RateLimit (api_secs / spillover as today).

6. PR-1 “does not park the rest of Claude” is false for waves. `Wait` in `react_to_outputs_inner` fires once per wave and sleeps the whole loop, so a single Fable-routed task stalls the standard slots for 3600s. The operator recipe must say the **pin is required for parallel/wave runs**, not merely recommended.

7. `evaluate_quota` cannot emit `ask`. FR-004 lists `ask` as a per-bucket output and then says the apply layer owns remaining-work and `tierFallback` — the only inputs that distinguish `ask` from `unavailable`. Evaluate emits per-bucket low/unavailable (and account wait/stop inputs). The apply layer resolves `ask` / `wait` / `stop`. CONTRACT-001 must not promise `ask` from evaluate’s inputs.

8. PR-2 proto-channel with no expiry. Replace-from-decision is listed under PR-3 (FR-006). With `LOOP_USAGE_CHECK_ENABLED=false`, evaluate runs only post-output, so a frontier exclusion set in PR-2 is never cleared for the rest of the run. State that the PR-2 set is **replaced on each successful evaluate**, or accept run-scoped stickiness in the AC. **Pick: replace on each successful evaluate** (keep snapshot on API fail).

## Medium (fold into PRD)

- `models show`: US-007 promises remaining via live fetch; §5.5 cuts live remaining as config-only without fetch. **Keep US-007** (remaining numbers only with the same live-fetch gate as `models list --remote`). Strike the contradictory §5.5 cut.
- Rung-scoped stop continues the chain. Every subsequent PRD with frontier work will also stop, so `batch --chain` degrades to skipping the whole chain one PRD at a time. **Pick: the next PRD inherits the rung-unavailable decision** (batch/process-local), so it can clamp to standard instead of stopping again. Account-binding `stop` still stops the chain.
- `ask` with TTL > 0 has no wake — a config write only takes effect after the sleep ends. **Pick: re-evaluate config on the stop-check cadence** (not a deaf fixed sleep).
- FR-002 test (b) guard is fine. Wave: one Fable RateLimit and two completions must still produce **exactly one** 3600s wait and no `provider_blackouts.record`.
- Glossary “working rungs” includes the spillover target, but FR-002 and Non-Goals forbid provider spillover on rung-scoped phrasing. **Spillover is never a working rung for rung-scoped decisions.**
