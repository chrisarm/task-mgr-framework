# JSON review: quota-rung-policy-pr2 (phase 2)

Date: 2026-09-07
Reviewer: md-to-json-prd-reviewer `01a07ba4-9d4c-7d70-bed9-26b2ff7139f7`

**Status**: APPROVED (pass 2)

Pass 1 was NEEDS_CHANGES. Apply closed W1, W2, S1, S2, S4. FEAT-005 not split.

---

## Pass 2 — APPROVED

Date: 2026-09-07
Reviewer: `01a07bae-93ea-7b81-aaaf-1865c7f6f731`

No material findings. Parent can skip further apply.

---

## Pass 1 (historical) — NEEDS_CHANGES

Parent should apply **W1 and W2**. Skip split of FEAT-005.

## Warnings

1. **[FEAT-005]** Proto-channel exclusion no-ops if `compute_quota_excluded_ids` still early-returns empty when `provider_blackouts` is empty. Add AC + failureMode for empty blackouts + frontier unavailable.

2. **[FEAT-005]** Absent `routing.tierFallback` must deserialize to factory Some, not None. Explicit null is the ask opt-out.

3. **[FEAT-005]** 7 ACs / 9 files — do **not** split.

## Suggestions

1. FEAT-004 `5d 13h` vs `display.rs` days band — add file or reword.
2. Ingest as new `ingest_oauth_value`, do not change `parse_oauth_usage_json` signature for extra-mark.
3. `TEST_USAGE_PARAMS.threshold` 92 → 8 if remaining-min semantics.
4. Prohibit documenting wait-loop as accepted for off-ladder `tasks.model`.
