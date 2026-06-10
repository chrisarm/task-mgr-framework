# Changelog — 2026-06-10

## Model-Selection Redesign — Provider-First Config, Capability Tiers, Multi-Provider Orchestration

**Branch**: `feat/model-selection-redesign`
**PRD**: `tasks/prd-model-selection-redesign.md`

### What shipped

Replaced five overlapping model-config surfaces (`defaultModel`, `reviewModel`,
`primaryRunner` + 3 sub-maps, `fallbackRunner`) with one provider-first
`models` + `routing` block (hard break). Capability tiers
(`frontier`/`standard`/`cost-efficient`/`cheapest`) with an **anchor-window**
selection (difficulty offsets around a configured anchor tier) replace
substring `ModelTier` classification, which was structurally dead once Fable 5
landed above Opus. Effort is decoupled from tier via per-provider tables
(codex wired through `-c model_reasoning_effort=` before `exec`). Adds
role-split routing, difficulty spillover to grok/codex, quota-aware failover
(temporary `provider_blackouts` channel, never touching `runner_overrides`),
tier-based escalation ladders, and migration v20 provider stamping
(`completed_by_provider`, `run_tasks.provider/model`) as the prerequisite for
the deferred review-cascade PRD.

### Why it matters

Routing policy now has a single source of truth: adding a new model is a
config edit, adding a fourth provider is a schema entry — not a code change.
One anchor knob shifts the whole cost posture up or down. Heterogeneous
providers run in one wave with audit trail.

### Post-review fixes (this `/review-loop` + `/compound` cycle)

- Closed a wave/sequential parity gap: the wave + slot paths still consumed
  `prd_metadata.default_model` after the sequential path dropped it, which under
  a null-rung config could mis-emit `-m <claude-id>` to the Codex CLI.
- Hardened the operator escape valve: the absorb rule now recognizes the
  recovery ladder's own `tasks.model` writes for both NULL-original and
  `Some`-original snapshots (rung-4 cross-provider pivot), and consecutive-
  failure escalation refreshes the snapshot at its write site — preventing the
  six-channel clear from wiping a just-installed promotion.

### Breaking changes

- Legacy config keys (`defaultModel`, `reviewModel`, `primaryRunner`,
  `fallbackRunner`) are a **hard error** at `loop run` / `batch run` and on
  `models` mutating verbs. Migration path: `task-mgr models init
  --force-replace-legacy` (preview with `--dry-run`). Non-loop commands warn
  and proceed. `prd_metadata.default_model` column is retained but has no
  routing effect.
- CLI verbs `models set-default` / `set-review-model` / `set-fallback`
  removed, replaced by `models init/show/set-anchor/enable/disable/set-tier/
  set-effort/set-fallback/route/list`.

---
