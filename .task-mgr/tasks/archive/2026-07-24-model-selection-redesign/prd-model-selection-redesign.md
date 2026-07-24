# PRD: Model-Selection Redesign — Provider-First Config, Capability Tiers, Multi-Provider Orchestration

**Type**: Refactor + Feature
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-06-09
**Status**: Draft (Rev 4 — scope cut per colleague reviews: structural frontier forcing REMOVED [handled at task-generation time instead], review cascade DEFERRED to `tasks/prd-review-cascade.md`; provider stamping retained)
**Design history**: `/home/chris/.claude/plans/help-me-plan-out-wise-puzzle.md` (production-code-architect reviewed; blocking issues B1–B5 resolved)

---

## 1. Overview

### Problem Statement

Model selection in task-mgr has accreted into **5 overlapping config surfaces** (`defaultModel`, `reviewModel`, `primaryRunner` with 3 sub-maps, `fallbackRunner`) plus hardcoded Claude assumptions: `difficulty=high → OPUS_MODEL` and substring-based `ModelTier` classification. With Claude Fable 5 sitting above Opus, substring tier matching is structurally dead (`claude-fable-5` contains no `"opus"`), and the operator wants codex + grok running alongside Claude as first-class providers with a coherent, single-surface routing policy.

### Background

- Wave mode already resolves the runner per slot — heterogeneous providers per wave work mechanically today; what's missing is the *policy* layer.
- Effort is currently a single global `difficulty → effort` table; codex rejects effort entirely (`RunnerCapability::Effort = false`).
- "Planning" and "long-running" task signals don't exist; tasks carry `difficulty` (low/medium/high) and an ID-prefix-derived class.
- The production-code-architect reviewed the full design; the blocking issues relevant to this scope (escape-valve snapshot semantics, hard-break non-loop coverage, blackout/stale-counter interaction, codex flag position) are resolved below.
- **Scope cuts (Rev 4)**: the engine-owned multi-provider review cascade is deferred to `tasks/prd-review-cascade.md` (builds on this foundation; its provider-stamping prerequisite ships HERE so implementer history accrues early). Structural frontier forcing (first-task/keystone rules) is removed from the engine entirely — the intent moves to task-generation guidance in `/prd-tasks` and `/plan-tasks` (author keystone tasks as `difficulty: high` or CONTRACT-prefixed). Revisit trigger: if post-launch we repeatedly observe keystone tasks under-modeled despite generator guidance, reconsider engine-side forcing.

---

## 2. Goals

### Primary Goals

- [ ] One provider-first `models` + `routing` config block replaces all 5 legacy surfaces (hard break).
- [ ] Capability tiers `frontier` / `standard` / `cost-efficient` / `cheapest` replace `ModelTier` and all opus/sonnet/haiku alias keys; an **anchor tier** + difficulty offset window (low→anchor−1, medium→anchor, high→anchor+1, clamped) drives baseline selection.
- [ ] Effort is decoupled from tier: per-provider `difficulty → effort` tables; codex effort wired via `-c model_reasoning_effort=` (positioned **before** the `exec` subcommand).
- [ ] Multi-provider routing policy: role-based split (claude owns planning/review/hard), difficulty spillover to grok/codex, quota-aware failover (spillover-eligible tasks only).
- [ ] Escalation/overflow/recovery ladders redefined in tier terms with config-derived cross-provider fallback directions.
- [ ] Provider stamping (`tasks.completed_by_provider`, `run_tasks.provider/model`) lands now as audit + as the prerequisite for the deferred review-cascade PRD.

### Success Metrics

- Default config + anchor=standard reproduces: low→sonnet, medium→opus-4-8, high→fable. Anchor=cost-efficient shifts to haiku/sonnet/opus.
- A loop with grok+codex enabled runs heterogeneous providers in one wave; completed tasks carry `completed_by_provider`.
- A simulated Claude rate limit reroutes a medium FEAT task to grok while a frontier REVIEW task defers — with zero stale-counter increments.
- `cargo test` green, `cargo clippy -- -D warnings` clean, `cargo run --bin gen-docs -- --check` passes.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Resolution determinism**: `resolve_execution_plan` is a pure function of (task fields, config); identical inputs → identical `ExecutionPlan` in sequential and wave paths (reaction-parity pinned).
- **Escape-valve contract preserved**: `resolve_execution_plan` NEVER writes `tasks.model`; escalation/promotion paths continue to write it; `pre_spawn::invalidate_stale_overrides` keeps keying off `tasks.model` vs `ctx.overflow_original_task_model`, with NULL-original semantics for anchor-resolved tasks.
- **Promotion idempotency**: the quota-blackout channel (`ctx.provider_blackouts`) never reads or writes `runner_overrides`; `promote_once` remains the single permanent-promotion guard (learnings #4921, #4060).
- **Provider inference invariants unchanged**: `provider_for_model` token-equality (Groq ≠ Grok); Codex never inferred from a model string — config-explicit only.

### Performance Requirements

- Best effort generally; resolution stays allocation-light on the per-slot hot path (config resolved once per run, cached — never re-read inside wave hot paths, matching the existing run-level config caching contract).
- Quota-deferral wait paths must not busy-spin: reuse `wait_for_usage_reset` / probe machinery.

### Style Requirements

- All `tasks.status` writes via `TaskLifecycle` verbs — no raw UPDATEs.
- New loop behaviors go through the reactions single-home contract: `#[deprecated]` leaf locks, exhaustive param-struct destructure (no `..`), hermetic `_inner` + injected seams.
- Strict parse for all config enums (`CapabilityTier::parse`, `parse_config_provider`) — typos are CONFIG ERRORs, never silent fall-throughs.
- Model-ID literals live only in `src/loop_engine/model.rs` (no_hardcoded_models discipline).

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| `claude-fable-5[1m]` in `tasks.model` | `tier_of` must strip `[1m]` before exact match or 1M-promoted tasks lose their tier | Reverse-maps to Frontier |
| Anchor-resolved task (NULL `tasks.model`) → overflow → escalation → operator edit | Escape-valve snapshot recorded NULL; comparison must still fire the six-channel clear | Overrides cleared, one stderr line |
| Sparse grok ladder (only `standard` defined), anchor window lands on `frontier` | Nearest-defined clamping ambiguity | Clamp prefers stepping down, then up; pinned by tests |
| Anchor=cheapest + difficulty=high; anchor=frontier + difficulty=low | Window offset at ladder ends | Clamped to cheapest/frontier respectively (offset never wraps) |
| Single-tier ladder (codex default); gap ladder (only cheapest + frontier defined) | Clamping across undefined rungs | Single tier always wins; gap ladders clamp down-first across the gap, then up — exhaustive matrix in CONTRACT-001 tests |
| Wave where ALL candidates are quota-deferred | Empty selection historically feeds the stale-abort counter (documented false-abort bug class) | Deferral-first branch waits for reset; stale counter untouched |
| Codex route + `difficulty=high` | xhigh must never reach codex (deliberate policy — CLI 0.136.0 accepts it; operator decision 2026-06-09) | Validation caps codex effort at `high`; `-c model_reasoning_effort=` precedes `exec` |
| `groq-llama-*` model string | Substring matching would mis-route to xAI Grok runner | Token-equality keeps it Claude |
| Legacy config keys + non-loop command (`recall`, `models show`) | Hard error there would be hostile / strand the operator | Warn-and-proceed; `models show` detect-and-instruct; hard error only at loop/batch entry + models mutating verbs |

---

## 2.6. Boundary Contracts & Modularity Targets

- **CONTRACT-001 (foundation)**: `CapabilityTier`, `ModelsConfig`/`ProviderConfig`/`RoutingConfig` serde + built-in defaults + field-wise merge, `ResolvedModelsConfig` (`model_for` / `tier_of` / `effort_for`, sparse-ladder clamping), `ExecutionPlan`, `FABLE_MODEL` / `ONE_M_SUFFIX`, `validate_models_config`, `detect_legacy_model_keys`. **Strictly pure, no I/O** — `validate_models_config` performs schema/semantic validation ONLY. Binary probes are a SEPARATE function (`probe_enabled_provider_binaries`, enabled-gated) invoked from `preflight_validate_and_probe` at loop/batch entry and from `models enable` — never from the pure validator. Every downstream task consumes these signatures.
- **Modularity rule**: anchor/tier derivation lives in exactly ONE function shared by the spawn site and every recovery path (the FIX-001 divergence lesson).

---

## 3. User Stories

### US-001: Provider-first configuration
**As an** operator, **I want** to declare enabled providers, their submodel→tier ladders, and effort tables in one config block, **so that** routing policy has a single source of truth.
**Acceptance Criteria:**
- [ ] `task-mgr models init` writes the default `models` + `routing` block; `models show` renders the full routing table.
- [ ] Legacy keys produce the per-command behaviors in FR-002's coverage table.
- [ ] `validate_models_config` rejects: unknown tier keys (no legacy aliases), disabled primaryProvider, ambiguous reverse model lookup, per-provider effort violations (codex xhigh), fallback to self/disabled, routes referencing disabled providers.

### US-002: Anchor-tier selection
**As an** operator, **I want** to set a "most-used tier" anchor that difficulty offsets around, **so that** I can shift the whole cost posture up or down with one knob.
**Acceptance Criteria:**
- [ ] anchor=standard: low→`claude-sonnet-4-6`, medium→`claude-opus-4-8`, high→`claude-fable-5`; review/planning classes force frontier regardless of difficulty.
- [ ] anchor=cost-efficient: low→haiku, medium→sonnet, high→opus.
- [ ] Sparse ladders clamp to nearest defined tier (down first, then up).

### US-003: Per-provider effort incl. codex
**As an** operator, **I want** effort tuned per provider, **so that** grok/codex (where the submodel choice is trivial) are tuned by effort alone.
**Acceptance Criteria:**
- [ ] Claude/grok receive `--effort` from their own tables; codex receives `-c model_reasoning_effort=<level>` positioned BEFORE `exec`.
- [ ] `CodexRunner::supports(Effort) == true`; capability matrix + CLAUDE.md table updated.
- [x] Spike verified against pinned codex-cli: exact key name, accepted levels, flag position — all three, before validation freezes. *(CONFIRMED 2026-06-09 vs codex-cli 0.136.0: key `model_reasoning_effort`; levels `none|minimal|low|medium|high|xhigh`; `-c` valid before `exec`. Cap at `high` retained as policy — see §7.)*

### US-004: Role split + difficulty spillover
**As an** operator, **I want** claude to own planning/review/hard work while codex/grok take easy/medium implementation, **so that** Claude quota is preserved for high-leverage tasks.
**Acceptance Criteria:**
- [ ] `routing.taskClasses` routes by class (prefix-derived via existing `id_body_matches_prefix`) with `byDifficulty` overrides; high-difficulty implementation stays on claude.
- [ ] Explicit `tasks.model` and `routing.byIdPrefix` still win (precedence rungs 1, 2).

### US-005: Quota-aware failover
**As an** operator, **I want** spillover-eligible tasks to drain to grok/codex when Claude rate-limits, **so that** the loop keeps moving without degrading frontier-class work.
**Acceptance Criteria:**
- [ ] On Claude RateLimit with eligible `todo` work: blackout recorded from `parse_reset_from_output`, wait skipped, `RerouteAndRetry` returned (budget give-back; no merge-fail-counter zeroing).
- [ ] Frontier-class tasks defer; a wave whose only candidates are quota-deferred waits for reset and NEVER increments the stale tracker (parity test, both paths).
- [ ] Blackout self-clears on expiry; never touches `runner_overrides`.

### US-006: ~~Successive multi-model review cascade~~ — **DEFERRED**
Moved to `tasks/prd-review-cascade.md` (follow-up PRD; depends on this one). Within THIS PRD, review-class tasks simply route per `routing.taskClasses.review` (frontier on claude by default). The cascade's prerequisite — provider stamping — ships here (FR-006).

### US-007: ~~Structural frontier forcing~~ — **REMOVED**
Intent relocated to task-generation time: `/prd-tasks` and `/plan-tasks` guidance now instructs generators to author the highest-fan-out ("keystone") task as `difficulty: high` (or CONTRACT-prefixed), which the anchor window then resolves to frontier. No engine code. Revisit trigger recorded in §1 Background.

---

## 4. Functional Requirements

### FR-001: New config schema

Canonical shape (this PRD is the authoritative source — implementers must not need external documents):

```json
{
  "version": 2,
  "models": {
    "anchor": "standard",
    "primaryProvider": "claude",
    "providers": {
      "claude": {
        "enabled": true,
        "tiers": {
          "frontier": "claude-fable-5",
          "standard": "claude-opus-4-8",
          "cost-efficient": "claude-sonnet-4-6",
          "cheapest": "claude-haiku-4-5-20251001"
        },
        "effort": { "low": "medium", "medium": "high", "high": "xhigh" },
        "fallback": "grok"
      },
      "grok": {
        "enabled": false,
        "tiers": { "standard": "grok-build" },
        "effort": { "low": "low", "medium": "medium", "high": "high" },
        "fallback": "claude",
        "cliBinary": null,
        "runtimeErrorThreshold": 2
      },
      "codex": {
        "enabled": false,
        "tiers": { "standard": null },
        "effort": { "low": "low", "medium": "medium", "high": "high" },
        "fallback": "claude",
        "runtimeErrorThreshold": 2
      }
    }
  },
  "routing": {
    "taskClasses": {
      "planning":       { "providers": ["claude"], "tier": "frontier" },
      "review":         { "providers": ["claude"], "tier": "frontier" },
      "implementation": { "providers": ["codex", "grok", "claude"],
                          "byDifficulty": { "high": ["claude"] } },
      "default":        { "providers": ["claude"] }
    },
    "classify": {
      "implementation": ["FEAT-", "FIX-", "CODE-FIX-", "WIRE-FIX-", "IMPL-FIX-", "REFACTOR-"],
      "planning":       ["PLAN-", "SPIKE-", "CONTRACT-"]
    },
    "byIdPrefix": {},
    "spillover": {
      "quotaFailover": true,
      "eligibleClasses": ["implementation"],
      "maxDifficulty": "medium",
      "blackoutFallbackSecs": 3600
    }
  }
}
```

*(A `routing.reviewCascade` block is reserved for the deferred cascade PRD — unknown keys under `routing` are preserved on write but rejected with a "not yet supported" note by validation only if named `reviewCascade`, so a premature config doesn't silently no-op.)*

**Field defaults** (a key absent from config takes the value shown above; a provider absent entirely takes its full default block):
- `models.anchor`: `"standard"`; `models.primaryProvider`: `"claude"`.
- `providers.claude.enabled`: `true`; `providers.{grok,codex}.enabled`: `false`. Present providers merge **field-wise** over their default block, so `{"grok": {"enabled": true}}` is a complete opt-in.
- `tiers` is sparse: `null` value = "route to this provider, omit the model flag" (codex CLI default). Absent tier = no model at that rung. Grok `composer` is operator-added (e.g. `"cost-efficient": "composer"`), not a built-in default.
- `effort` keys ∈ {low, medium, high} (task difficulty); values validated per provider: claude/grok ∈ {low, medium, high, xhigh}, codex ∈ {low, medium, high} *(policy cap — pinned codex-cli 0.136.0 itself accepts `xhigh`; operator decision 2026-06-09. Validation error message should say "by policy" so a future operator knows loosening is a config-validation change, not a CLI fight)*.
- `fallback`: optional provider name; absent = no cross-provider pivot from this provider. `runtimeErrorThreshold` default 2. `cliBinary` default null (PATH lookup).
- `routing.taskClasses.<class>`: `providers` = ordered preference list; optional `tier` forces a tier; optional `byDifficulty` remaps the preference list per difficulty.
- `routing.classify.<class>`: ID-prefix lists (dash-boundary matching). Unmatched IDs → class `default`. Review class is built-in (`CODE-REVIEW-`, `MILESTONE-FINAL`, `REVIEW-`, excluding `REFACTOR-REVIEW`) and not redefinable via `classify`.
- `routing.byIdPrefix.<prefix>`: `{ "provider": "<name>", "tier": "<tier>" }` (tier optional → anchor-derived).

**Validation:** unit tests per rule in US-001; `models show` round-trips.

### FR-002: Hard break with per-command coverage
| Surface | Behavior on legacy keys |
| --- | --- |
| `loop run` / `batch run` (`preflight_validate_and_probe`) | **Hard error**: names each key, prints schema skeleton, points at `models init --force-replace-legacy` |
| `read_project_config` (recall, curate, next, show, ...) | **One-line stderr warning**, proceed with defaults (routing not consumed there) |
| `models show` / `list` | Detect-and-instruct banner, never hard-fail |
| `models` mutating verbs | Hard error (never write new keys beside legacy) |
| `models init --force-replace-legacy` | The one sanctioned migration: delete 4 legacy keys, write default block |

Delete legacy `ProjectConfig` fields + ALL migration machinery (`migrate_project_config_value`, `update_project_config_format`, `canonical_baseline_tier_key`, serde aliases). **Deliberate deviation from learnings #4670/#4633** (read-time normalizers) per operator decision.

**PRD/user default disposition (explicit)**:
- `prd_metadata.default_model` DB column **stays** (no schema churn; old exports/imports round-trip unchanged) but the resolution chain stops reading it. The PRD JSON `model` field still parses and imports into the column — it just has no routing effect.
- **Warning sites**: `loop init` / `batch init` / `loop run` emit one stderr line when the PRD JSON (or `prd_metadata`) carries a `default_model` ("ignored under the models config; use models.anchor / routing instead"). User-config `defaultModel` warning is emitted at loop/batch preflight only. Non-loop commands stay silent about defaults (they don't consume routing).
- Export/import: column exported and re-imported verbatim; never deleted, never migrated.

**Migration preview**: `models init --dry-run` prints, without writing, (a) the legacy keys that `--force-replace-legacy` would delete with their current values, and (b) the new block that would be written — a non-destructive diff for high-stakes operators.

### FR-003: Resolution precedence (6 rungs)
1. Explicit `tasks.model` → provider via `provider_for_model`, tier via `tier_of`
2. `routing.byIdPrefix` explicit route
3. Task class → provider preference + forced tier (review/planning → frontier)
4. Quota-blackout reroute (spillover-eligible only; derived per pass, never stored)
5. Anchor window (difficulty offset, clamped)
6. Tier → model via `model_for`; effort last, from the final provider's table

Single fn `resolve_execution_plan` called by both prompt builders. `resolve_execution_plan` never writes `tasks.model` (escape-valve contract). `ctx.effort_overrides` (overflow downgrade) still wins over plan effort at spawn. *(The deferred cascade PRD will insert its provider-pin rung between 1 and 2; the rung structure is designed to accept it without renumbering churn — name rungs in code by constant, not ordinal.)*

**Codex operator-pin story (explicit)**: `tasks.model` cannot express Codex (Codex is never inferred from model strings — invariant preserved). The named path for pinning a task to codex is **route-only**: `task-mgr models route <prefix> --provider codex` (rung 2). Writing a codex-looking string into `tasks.model` routes to Claude by design — `models show` documents this, and no new provider column is added in v1.

**Classification SSoT**: one `classify_task(id, &RoutingConfig) -> TaskClass` function (built on `id_body_matches_prefix`) is the single classifier consumed by rung 3 and spillover eligibility. It does NOT replace `SPAWNED_FIXUP_PREFIXES` or `BUILDY_TASK_PREFIXES` (shared-infra slot heuristic) — those serve different decisions — but a **consistency test** asserts `SPAWNED_FIXUP_PREFIXES ⊆ classify.implementation` defaults and flags drift between the prefix sets so they cannot silently diverge.

**Validation:** precedence table test (every rung beats all lower rungs); escape-valve lifecycle test on a model-less task; reaction-parity.

### FR-004: Tier system
`CapabilityTier {Cheapest, CostEfficient, Standard, Frontier}` ordered; delete `ModelTier`/`model_tier`/`parse_baseline_tier_key`. `FABLE_MODEL` constant; `ONE_M_SUFFIX` replaces `OPUS_MODEL_1M`. `resolve_iteration_model` rewritten on `tier_of` (None < Some(Cheapest); last-wins tie-break preserved).

### FR-005: Escalation/overflow/recovery in tier terms
`escalate_tier` (up one defined tier, same provider; ceiling self-loop), `escalate_below_ceiling` (None at top), `to_1m_model` suffix-append (Claude-only, covers fable). `select_fallback_target` config-derived from `providers.<p>.fallback` (tier-preserving); codex gains rung-4 iff fallback set. `maybe_codex_fallback_to_claude` → `maybe_provider_runtime_fallback`. `promote_once` / deferred-apply / `invalidate_stale_overrides` untouched.

### FR-006: Migration v20 — provider stamping (cascade prerequisite, retained)
Columns only: `tasks.completed_by_provider`, `run_tasks.provider`, `run_tasks.model`. **No `review_rounds` table** (deferred with the cascade PRD; a later migration adds it). Down migration: version-only column convention (learning #348). Three edits in migrations/mod.rs (learning #503); tests via `run_migrations` not bare `create_schema` (learning #1550). Stamping happens in `iteration_pipeline::process_iteration_output` (single home, both paths — reads `effective_runner`/`effective_model` already on the result structs). Rationale for shipping now: audit value on its own, and the deferred cascade's reviewer≠implementer rule gets real history from day one instead of a NULL backlog.

### FR-007: ~~Review cascade engine wiring~~ — **DEFERRED**
Moved wholesale to `tasks/prd-review-cascade.md` (review_rounds table, post_completion step, `SpawnFn` seam, rotation, fix discovery, archive lifecycle, doctor check).

### FR-008: Quota failover machinery
`ctx.provider_blackouts` (temporary channel, documented in CLAUDE.md as never touching `promote_once`/`runner_overrides`); `RerouteAndRetry` + `ProceedWithSpillover` variants in the account coordinators; `excluded_ids` on `select_parallel_group` + sequential equivalent; **deferral-first branch** in `handle_no_eligible_tasks` + sequential no-eligible branch (waits, clears blackout, returns without stale increment / auto-recovery churn / drained classification).

**Channel discipline (type-enforced)**: `BlackoutState` is a newtype living only on `IterationContext` with a doc comment stating the three rules — (1) never persisted to or read from disk/DB (in-memory only; clears on restart by design), (2) never read or written by `promote_once`/`runner_overrides` paths, (3) set only by account-level rate-limit signals, never by task failures. The `loop_engine/CLAUDE.md` footgun section gains a matching paragraph.

### FR-009: models CLI verbs
`init` (+`--force-replace-legacy`, +`--dry-run` preview per FR-002), `show`, `set-anchor`, `enable|disable <provider>` (probe on enable), `set-tier`/`unset-tier`, `set-effort`, `set-fallback`/`unset-fallback`, `route`/`unroute`, `list` (config reverse lookup). Remove `set-default`/`set-review-model`/`set-fallback` legacy verbs (+unset). Writes preserve unknown config keys. Picker retargets to anchor selection.

**Cost-posture surfacing**: `models show` prints the anchor-derived difficulty→model mapping AND a one-line note that crash escalation on model-less tasks lands at anchor+1 (with the resolved model + relative cost, e.g. "high-difficulty + escalations → claude-fable-5 ($10/$50 per MTok)"). The default config written by `models init` carries a `"_comment"`-style note (or doc) stating the same.

### FR-010: Docs/tests/fixtures
`no_hardcoded_models.rs` regex gains `fable` (lands with CONTRACT-001). gen-docs extracts `FABLE_MODEL` + tier/effort tables, renders tier matrix + anchor explanation. Fixture placeholders `{{FRONTIER_MODEL}}` etc.; sweep `tests/fixtures/*.json.tmpl`. Dedicated test-migration sweep task (grep-derived list: recovery.rs unit tests, runtime_error_fallback, primary_runner_*, reaction_parity, review_model_routing, fallback_config, overflow_*, models_command, synergy/iteration-model tests, escalation tests, prompt-section tests calling old resolution fns — derive the final list by grepping for `ModelTier`, `model_tier`, `resolve_task_model`, `resolve_task_execution_target`, `compute_baseline_model`). `reaction_parity` gains a new case: `RerouteAndRetry` in both paths. Rewrite `src/loop_engine/CLAUDE.md` routing/fallback/capability sections + blackout-channel paragraph; update root CLAUDE.md.

### FR-011: Task-generator guidance (replaces engine-side structural forcing) — ✅ ALREADY DELIVERED
`/prd-tasks` and `/plan-tasks` skill documents (in `~/.claude/commands/`) updated 2026-06-09 with the keystone rule: (a) the run's opening/foundation task should be CONTRACT-/PLAN-prefixed (classified `planning` → frontier), and (b) the task with the most transitive dependents carrying medium+ complexity is stamped `estimatedEffort: "high"` (imports to `tasks.difficulty` → anchor window resolves it to the frontier tier; ties all promoted; reasoning recorded in `notes`). Generation-time policy, zero engine code. **Do NOT generate a loop task for this — it is complete.**

---

## 5. Non-Goals (Out of Scope)

- **Engine-owned review cascade** — deferred to `tasks/prd-review-cascade.md`. Reason: separable workflow-engine feature; safer built second on a stabilized foundation. Review-class tasks route to frontier-claude via `routing.taskClasses.review` in the meantime.
- **Engine-side structural frontier forcing (first-task/keystone rules)** — removed. Reason: marginal coverage over existing class/difficulty forcing is narrow; the intent is served at task-generation time (FR-011) and by manual `models route` / model pins. Revisit trigger in §1.
- **Per-provider quota tracking for grok/codex** — blackout machinery is Claude-only (map is provider-keyed for later extension).
- **Selection-time provider preference** — selection stays provider-agnostic; only hard exclusion is added. Reason: scheduler churn risk.
- **Soft migration of legacy config** — operator-confirmed hard break; `models init --force-replace-legacy` is the only path.
- **`max` effort tier** — retired previously for context overflow; ladder still floors at `high`.

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/model.rs` — tier type, constants, resolution rewrite, deletions
- `src/loop_engine/project_config.rs` — new schema, validation, hard-break detection, deletion of migration machinery
- `src/loop_engine/engine.rs` — `EffectiveRunnerInput` population, `IterationContext.provider_blackouts`
- `src/loop_engine/recovery.rs`, `src/loop_engine/reactions/post_output.rs` — tier-based ladders, config-derived fallback
- `src/loop_engine/reactions/{pre_spawn,account}.rs` — reroute rung, RerouteAndRetry/ProceedWithSpillover
- `src/loop_engine/iteration_pipeline.rs` — provider stamping
- `src/loop_engine/runner.rs` — codex effort capability + argv
- `src/loop_engine/prompt/{sequential,slot}.rs`, `slot.rs`, `wave_scheduler.rs`, `iteration.rs` — plan threading
- `src/commands/next/selection.rs` — `excluded_ids`
- `src/loop_engine/wave_orchestration.rs` — deferral-first branch
- `src/commands/models/*` — CLI verb redesign
- `src/db/migrations/{mod.rs,v20.rs}` — schema v20 (stamping columns)
- `src/bin/gen-docs.rs`, `tests/*` — docs + test migration
- `~/.claude/commands/{prd-tasks,plan-tasks}.md` — FR-011 generator guidance (markdown, outside the Rust loop)

### Dependencies

- Pinned codex-cli version (effort flag spike); grok CLI binary probe (existing).
- No new crates anticipated.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| Provider-first config + capability tiers + anchor (selected) | Single source of truth; tier names survive model churn; one knob shifts cost posture; subsumes baselineTierRoutes | Hard break costs operator migration; large test blast radius | **Preferred** |
| Layer new block atop legacy keys (precedence) | No migration event | Two routing code paths forever; ambiguity about which surface wins; perpetuates the 5-surface sprawl | Rejected (operator chose hard break) |
| Patch in-place: add fable to `model_tier` substring match + extend primaryRunner | Smallest diff | Keeps substring fragility, 5 surfaces, global effort; no anchor; blocks multi-provider strategies | Rejected |
| Engine-side structural frontier forcing | Automatic keystone coverage | Graph traversal + tie/cycle/restart semantics + scheduler threading for narrow marginal value | Rejected — moved to generation-time guidance (FR-011) |
| Cascade in this PRD | One migration event | Stretches a refactor into a workflow-engine change; highest-risk component rides the biggest diff | Rejected — deferred to follow-up PRD on a stable foundation |

**Selected Approach**: Provider-first config with capability tiers and anchor window; temporary derived blackout channel for quota failover; provider stamping now, cascade later.

**Phase 2 Foundation Check**: The tier abstraction + per-provider config costs ~1 extra task of foundation work (CONTRACT-001) but makes every future model launch a config edit instead of a code change, and makes adding a 4th provider a config schema entry instead of a routing rewrite. Provider stamping landing early means the deferred cascade starts with real implementer history. Comfortably clears the 1:10 bar.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Escape-valve mis-fire on anchor-resolved (NULL-model) tasks after escalation | High (override channels corrupt, wrong models mid-run) | Low–Med | Explicit contract (resolution never writes `tasks.model`); full-lifecycle test is a named AC |
| Quota-deferred empty selection feeds stale-abort counter → false loop abort (documented bug class) | High | Med | Deferral-first branch ordered before auto-recovery/stale logic; parity test both paths |
| Test blast radius underestimated (ModelTier deletion touches ~10 test files) | Med (schedule) | High | Dedicated test-migration sweep task with grep-derived file list; deletions isolated in REFACTOR-005 |
| Codex `-c` flag wrong key/position silently ignored | Med (codex effort no-op) | Med | Spike AC: verify key + levels + position-before-`exec` against pinned CLI before validation freezes |
| Cost-posture surprise: model-less crash escalation becomes anchor→anchor+1 (opus→fable on defaults, 2× $/MTok) | Med (cost) | High | Called out here + in `models show`; anchor is the operator's lever |

### Security Considerations

- Binary probes (`grok`, `codex`) keep existing executable-bit + empty-env-var-falls-through invariants (`is_executable_path`).
- Codex protected-state guard (snapshot/verify-restore) is untouched and continues to wrap all codex iterations.

### Public Contracts

#### New Interfaces

| Module/Function | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `model::CapabilityTier::parse` | `(s: &str) -> Result<CapabilityTier, String>` | tier | message naming accepted set | none |
| `model::resolve_execution_plan` | `(ctx: &PlanContext) -> ExecutionPlan` | `{provider, model, tier, effort}` | total fn, no error | none (never writes tasks.model) |
| `ResolvedModelsConfig::model_for` | `(Provider, CapabilityTier) -> Option<&str>` | model id (nearest-defined clamp) | None for undefined ladder | none |
| `ResolvedModelsConfig::tier_of` | `(Provider, &str) -> Option<CapabilityTier>` | tier (strips `[1m]`) | None for unknown id | none |
| `ResolvedModelsConfig::effort_for` | `(Provider, Option<&str>) -> Option<&str>` | effort level | None when difficulty unset | none |
| `project_config::detect_legacy_model_keys` | `(&serde_json::Value) -> Vec<&'static str>` | found key names | — | none |
| `project_config::validate_models_config` | `(&ModelsConfig, &RoutingConfig) -> Result<ResolvedModelsConfig, String>` | resolved config | CONFIG ERROR string | **none — pure** (no I/O) |
| `project_config::probe_enabled_provider_binaries` | `(&ResolvedModelsConfig) -> Result<(), String>` | () | probe failure naming binary + provider | filesystem/PATH probes; called only from `preflight_validate_and_probe` + `models enable` |

#### Modified Interfaces

| Module/Function | Current | Proposed | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `resolve_task_execution_target` / `resolve_task_model` / `compute_baseline_model` | model-string chain + provider_hint | **deleted** → `resolve_execution_plan` returning `ExecutionPlan` | Yes (internal) | REFACTOR-005 sweep updates all callers |
| `escalate_model` / `escalate_below_opus` / `to_1m_model` | Claude-constant ladders | `escalate_tier` / `escalate_below_ceiling` / suffix-append, config-driven | Yes (internal) | FEAT (ladders) |
| `CodexRunner::supports(Effort)` | `false` | `true` | No (loosening) | capability table + CLAUDE.md update |
| `build_codex_argv` | no effort | `-c model_reasoning_effort=<level>` BEFORE `exec` | No | spike-verified |
| `select_parallel_group` | no exclusion param | `+ excluded_ids: &HashSet<String>` | Yes (internal) | both callers updated in quota-failover task |
| `is_review_class` | 3 prefixes | `is_frontier_class` (+ classify-config classes) | Yes (internal) | resolution-rewrite task |
| `iteration_pipeline::process_iteration_output` | no provider stamping | stamps `completed_by_provider` + `run_tasks.provider/model` | No (additive) | v20 task |
| CLI `models set-default/set-review-model/set-fallback` | write legacy keys | **removed**, replaced by FR-009 verb set | Yes (operator-facing) | error message points at new verbs; `models init --force-replace-legacy` |

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| config.json → `ModelsConfig.providers` → tier map | JSON string keys → `HashMap<String, ProviderConfig>` (provider name, lowercase) → `HashMap<String, Option<String>>` (tier kebab-case string key → model id) | `cfg.providers.get("claude").and_then(\|p\| p.tiers.get("cost-efficient"))` — note `cost-efficient` is kebab-case in JSON; `CapabilityTier::as_str()` must emit exactly `"cost-efficient"`, and the resolved layer converts to typed keys: `resolved.model_for(Provider::Claude, CapabilityTier::CostEfficient)` |
| task → `ExecutionPlan` → `RunnerOpts` | `Task.difficulty: Option<String>` (DB, lowercase) → `ExecutionPlan.effort: Option<String>` → `RunnerOpts.effort: Option<String>` | effort precedence at spawn: `ctx.effort_overrides.get(task_id).copied().map(String::from).or(plan.effort.clone())` — ctx override (overflow downgrade) wins |
| `routing.taskClasses.*.byDifficulty` | JSON string key (`"high"`) → `HashMap<String, Vec<String>>` (difficulty → provider names) | difficulty is matched lowercase-trimmed via the same `normalize_difficulty` used by effort lookup — do NOT add a second normalizer |
| result structs → provider stamping | `IterationResult.effective_runner: RunnerKind` / `SlotResult.effective_runner` → `tasks.completed_by_provider: TEXT` (lowercase provider name via `Provider::as_str`) | stamp in the completion arm of `process_iteration_output`; store `"claude"`/`"grok"`/`"codex"`, never model strings |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `prompt/sequential.rs:413`, `prompt/slot.rs:335` | call `resolve_task_execution_target` | BREAKS | rewired to `resolve_execution_plan` |
| `iteration.rs:426`, `wave_scheduler.rs:805` | `apply_review_model_override` | BREAKS (deleted) | plan already carries review routing (rung 3) |
| `recovery.rs:242,314,361,495` | write `tasks.model` on escalation/promotion | OK (contractually preserved) | escape-valve lifecycle test |
| `reactions/pre_spawn.rs:186` | `invalidate_stale_overrides` snapshot vs `tasks.model` | NEEDS REVIEW (NULL-original case) | explicit NULL semantics + AC test |
| `wave_orchestration.rs::handle_no_eligible_tasks` | empty-selection → auto-recovery → stale counter | NEEDS REVIEW | deferral-first branch ordered before it |
| ~10 test files importing `ModelTier`/`model_tier`/deleted fns | unit + integration tests | BREAKS | test sweep task (grep-derived list) |
| `src/commands/models/handlers.rs` legacy verbs + their tests | write legacy keys | BREAKS | FR-009 verb redesign |
| `gen-docs` + `.claude/commands/tasks.md` MODELS block | extracts constants/effort table | BREAKS | docs task; CI `--check` enforces |
| Operator configs in the wild with legacy keys | loop startup | BREAKS (intentional) | FR-002 coverage table + `models init --force-replace-legacy` |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| `provider_for_model` vs `tier_of` | provider inference vs capability ranking | token-equality vs substring | stays split: token-equality untouched; `tier_of` = config exact-match (substring dies with `ModelTier`) |
| `runner_overrides` vs `provider_blackouts` | permanent promotion vs temporary quota reroute | only `runner_overrides` exists | strictly separate channels; blackout never touches overrides; overrides keep higher precedence |
| `ctx.effort_overrides` vs per-provider effort tables | overflow recovery downgrade vs baseline tuning | global EFFORT_FOR_DIFFICULTY | override channel unchanged and still wins at spawn |
| `classify_task` vs `SPAWNED_FIXUP_PREFIXES` vs `BUILDY_TASK_PREFIXES` | routing class vs fixup detection vs shared-infra slot claiming | three independent prefix sets | stay separate (different decisions) but consistency test pins `SPAWNED_FIXUP_PREFIXES ⊆ classify.implementation` defaults and flags drift |
| `tasks.model` pin vs `models route` pin | operator pins Claude/Grok vs operator pins Codex | tasks.model covers all | tasks.model cannot express Codex (by invariant); codex pinning is route-only — documented in `models show` |

### Inversion Checklist

- [x] All callers of deleted resolution fns identified (grep sweep is the test-sweep task's input)
- [x] Routing/branching on output reviewed (`resolve_effective_runner` precedence unchanged: overrides → hint → token-equality)
- [x] Tests validating current behavior identified (Consumers table)
- [x] Same-code-different-purpose paths documented (Semantic Distinctions)

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/loop_engine/CLAUDE.md` | Update | Rewrite routing-precedence diagram, primaryRunner/fallbackRunner sections → models/routing; new blackout-channel contract paragraph (promote_once must never read/write it) |
| `CLAUDE.md` (root) | Update | "Model IDs and Effort Mapping" + "task-mgr models" sections for the new verb set, anchor, tiers |
| `.claude/commands/tasks.md` | Regenerate | `cargo run --bin gen-docs` (tier matrix + anchor) |
| `~/.claude/commands/{prd-tasks,plan-tasks}.md` | Update | FR-011 generator guidance (keystone → difficulty: high / CONTRACT prefix) |
| `docs/designs/model-selection-redesign.md` | Create | Condensed architecture record of tiers/anchor/blackout for future sessions |

---

## 7. Open Questions

- [x] Codex CLI spike outcome: exact `model_reasoning_effort` key, accepted levels, position-before-`exec`. *(RESOLVED 2026-06-09, pre-loop spike vs pinned codex-cli 0.136.0: key = `model_reasoning_effort`; accepted levels = `none|minimal|low|medium|high|xhigh`; `-c` accepted both before AND after `exec` — before-`exec` placement pinned as the global-flag position. NOTE: the CLI **accepts** `xhigh`, falsifying the original premise — the validation cap at `high` is retained as deliberate cost/latency POLICY, operator decision 2026-06-09, not a CLI constraint.)*
- [x] Grok `composer` submodel: operator-added, not a built-in default. *(RESOLVED 2026-06-09 — FR-001 field defaults.)*
- [x] Review cascade scope: deferred to `tasks/prd-review-cascade.md`. *(RESOLVED 2026-06-09 — colleague review.)*
- [x] Structural frontier forcing: removed; generation-time guidance instead (FR-011). *(RESOLVED 2026-06-09 — colleague review.)*

---

## Appendix

### Related Documents

- Deferred follow-up: `tasks/prd-review-cascade.md` (multi-provider review cascade — depends on this PRD's foundation + provider stamping)
- Design history: `/home/chris/.claude/plans/help-me-plan-out-wise-puzzle.md`
- Learnings honored: #4921/#4060 (promote_once), #4865/#4186 (single-home coordinators), #503/#348/#1550 (migrations); deliberate deviation: #4670/#4633 (hard break vs read-time migration)

### Glossary

- **Capability tier**: provider-neutral capability/cost level (`frontier` > `standard` > `cost-efficient` > `cheapest`)
- **Anchor**: the configured "most-used tier"; difficulty offsets the selection window around it
- **Blackout**: temporary, account-global, self-expiring provider unavailability record driving derived (never stored) rerouting
- **Spillover-eligible**: implementation-class task with difficulty ≤ `spillover.maxDifficulty` and no runner override

### Task breakdown preview (for /prd-tasks)

CONTRACT-001 (foundation types, pure) → FEAT-002 (hard break + probes split) → {FEAT-003 (v20 stamping columns + pipeline stamping), FEAT-004 (resolution rewrite: `resolve_execution_plan` + classifier + anchor window + `is_frontier_class`, wire both prompt builders + `SlotPromptBundle` + `resolve_effective_runner` input + `resolve_iteration_model`, escape-valve lifecycle test)} → REFACTOR-005 (deletion sweep of superseded fns as its own diff) → {FEAT-006 (codex effort + spike), FEAT-007 (tier ladders + config-derived fallback), FEAT-008 (quota failover + deferral-first branch)} → FEAT-009 (models CLI) → FEAT-010 (test-migration sweep) → FEAT-011 (gen-docs/fixtures/CLAUDE.md) → CODE-REVIEW / MILESTONE-FINAL.
