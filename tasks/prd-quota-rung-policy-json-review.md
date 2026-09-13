# JSON review: quota-rung-policy-pr1 (phase 1)

Date: 2026-09-06
Reviewer: md-to-json-prd-reviewer `01a07a74-9e12-78e0-86f6-9aee2e387216`
PRD: `tasks/prd-quota-rung-policy.md`
JSON: `tasks/quota-rung-policy-pr1.json`
Prompt: `tasks/quota-rung-policy-pr1-prompt.md`

**Status**: APPROVED (pass 2)

Pass 1 was NEEDS_CHANGES (warnings). Apply closed W1–W4 and S1–S3. Pass 2: no material findings.

---

## Pass 2 (after apply) — APPROVED

Date: 2026-09-06
Reviewer: `01a07a7f-27ff-7d93-a393-6200a94f1ea4`

No material findings. Parent can skip further apply.

Pass-1 close-out: W1 95/50 band closed; W2 FIX-002 not split; W3 test (h) closed; W4 parse-only AC1 closed; S1 rustdoc; S2 Opus-limit; S3 clippy `--all-targets` is REVIEW-001 only.

Non-material residuals: JSON global AC still names `--all-targets` (prompt overrides); FIX-001 9 ACs / FIX-002 11 ACs (under hard 12).

---

## Pass 1 (historical) — NEEDS_CHANGES

PR-1 slice is independently shippable and correctly locked: FR-001 + FR-002 / US-001 + US-002 only; tests (a)–(g); narrow 3600 predicate; pin **required** for parallel/wave. Human items 1–4 are not implementation tasks. No `model` keys. Lean shape (no TEST-INIT / CONTRACT / ANALYSIS) matches this repo. Nothing here should start PR-2/PR-3.

Parent should apply finding 1. Findings 2–4 are small and optional; skip those if you want a thin apply.

---

Summary

- Total tasks: 4 (FIX-001, FIX-002, CODE-REVIEW-1, REVIEW-001)
- Tasks needing revision: 2 (FIX-001, FIX-002)
- Critical issues: 0
- Warnings: 4
- Suggestions: 3

---

Critical Issues (Must Fix)

None. The loop can execute this list without a structural failure.

---

Warnings (Should Fix)

1. **[FIX-001] severity: warning** — Missing 92–99 band known-bad for `reset_at`.

   Live fixture (55) and inverse (100) do not kill the old `exhausted = util ≥ 100 || severity == critical` predicate. A naive fold-only patch keeps `soonest_reset` among `exhausted`, then:
   - weekly-all **95**, session **50** → `percentage = 95` (gate waits, good) but `reset_at` prefers session (nothing ≥ 100). That is a 5h-cap loop on the wrong window until the weekly reset.
   AC 3 (“several windows ≥ 92 → latest”) can be “satisfied” with two 100% fixtures and never exercise ≥92-but-<100.

   **JSON fix:** add an AC + edge case + failureMode, e.g.
   - AC: `Band: seven_day / weekly_all used 95, session 50 → percentage = 95, reset_at = weekly (not session). Discriminator: old exhausted=≥100 would keep session reset.`
   - failureMode: `{ "cause": "reset_at still uses exhausted=util≥100", "expectedBehavior": "95/50 band test fails (session reset instead of weekly)" }`

2. **[FIX-002] severity: warning** — 10 acceptance criteria (flag at 7; under the hard 12). Five `touchesFiles`. `estimatedEffort: high`.

   **Do not split.** Detection + `decide_account_rate_limit` + production wait-closure skip + tests (a)–(g) are one coordinator contract. Landing classification without the 3600 override parks a RateLimit for ~6 days / session 5h / 300s / 30s probe-lift.

   **JSON/prompt fix (keep one task):** leave FIX-002 intact; optional: fold the docs AC into notes (REVIEW-001 already requires the pin recipe) to get to 9. Prompt already points high-effort at `/ralph-loop`. No further prompt change required.

3. **[FIX-002] severity: warning** — `/model` alone is in the predicate AC but not a lettered test.

   Tests (a)–(g) never assert that `/model` alone is insufficient. A stub that keys 3600 on `contains("/model")` still passes (a)–(g) if the live sentence includes `/model`.

   **JSON fix:** add a negative, e.g. `(h) output containing only \`/model\` (no model token, no \`switch models\`, no \`limit\`) does not take the 3600 override; if classified RateLimit at all, keep api_secs / may Blackout.`

4. **[FIX-001] severity: warning** — AC 1 names `check_and_wait` as if it were the test entry.

   `check_and_wait` calls `load_usage_info()` (network). AC 7 and the prompt forbid live Anthropic. A fresh instance may try to invoke the coordinator and hang or skip the parse test.

   **JSON fix:** reword AC 1 to parse-only: `parse_oauth_usage_json(live fixture) → percentage ≈ 55, reset_at = session; 55 < usage_threshold 92 implies the existing check_and_wait compare would return BelowThreshold — do not call check_and_wait / load_usage_info in this test.`

---

Suggestions (Nice to Have)

1. **[FIX-001]** Add an AC to update `UsageInfo` / `parse_oauth_usage_json` rustdoc (still says max across all windows + soonest exhausted). CODE-REVIEW-1 already lists this as a common miss; cheaper to require it on FIX-001.

2. **[FIX-002]** One non-Fable model-token case (`You've reached your Opus limit` → Wait 3600) so the override is not implemented as `contains("fable")` only. Live sentence + `fable|opus|sonnet|haiku` in the AC should be enough; a single extra fixture makes it fail-closed.

3. **[prompt / global AC]** Per-iteration clippy in the prompt is `cargo clippy -- -D warnings`; JSON global AC requires `cargo clippy --all-targets -- -D warnings`. Prompt says trust the prompt on conflict, and REVIEW-001 is the full gate — fine. Optionally say `--all-targets` is REVIEW-001 only so FIX-001 does not eat unrelated clippy.

---

Missing Tasks

None for PR-1.

---

Apply instruction from parent: **must apply finding 1**; also apply findings 3 and 4 and suggestions 1–2 (cheap). Finding 2: do **not** split FIX-002; skip folding the docs AC unless easy.
