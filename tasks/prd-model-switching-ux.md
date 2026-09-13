# PRD: Model-Switching UX — Coverage Warning + `curate switch-model`

**Type**: Enhancement
**Priority**: P2 (Medium)
**Author**: Claude Code
**Created**: 2026-07-29
**Status**: Draft

---

## 1. Overview

### Problem Statement

Switching the embedding (or reranker) model is currently four manual steps
spread across three surfaces: hand-edit `.task-mgr/config.json`
(`embeddingProfile` has no CLI, unlike the entire `task-mgr models` family),
get the model serving (docker stack or `ollama pull`), remember to run
`task-mgr curate embed` for gap-fill, and know the gap-fill-vs-`--force`
distinction exists at all. The failure mode is **silent**: skip the embed step
and vector recall quietly returns thin or empty results — the
"Empty vector hits after profile switch" row in `docker/SETUP.md`'s
failure-mode table exists precisely because of this.

Migration v21 (composite PK `(learning_id, model)`) made switching *cheap* —
prior models' vectors are retained, so only a gap-fill is needed and switching
back is instant — but nothing in the UX surfaces that cheapness or guides the
operator through it.

### Background

- Migration v21 landed in commit `e1f4f6d` with gap-fill semantics,
  `--prune-stale --yes`, per-model row breakdowns in `curate embed --status` /
  `curate count`, and a dims-consistency guard.
- The multi-agent edge-case audit of v21 confirmed the silent-degradation
  problem (stale `--force` doc advice, invisible per-model state) — all fixed —
  but the *switch workflow itself* remains manual.
- Design discussion settled on two layers shipped together:
  **Layer 1** (safety net): recall warns when the active model has coverage
  gaps. **Layer 2** (workflow): one `curate switch-model` command that
  validates, writes config, probes, reports the coverage delta, and offers to
  gap-fill.

---

## 2. Goals

### Primary Goals

- [ ] A profile switch (embedding, optionally reranker) is **one command**
      with validated inputs and round-trip-safe config writes.
- [ ] Vector recall **never degrades silently**: any coverage gap for the
      active model produces a one-line stderr warning naming the remedy.
- [ ] The v21 retention benefit is **visible**: the switch command reports
      rows retained under prior models and that switching back is instant.

### Success Metrics

- Switch workflow: 4 manual steps → 1 command (+ optional prompted embed).
- Recall coverage warning: fires in an integration test whenever
  `missing > 0`; absent when coverage is 100%.
- Config safety: a round-trip test proves unrelated `config.json` keys
  (`models`, `routing`, `rerankerOverFetchPercent`, …) survive a switch
  byte-for-byte in value terms.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Config writes must round-trip**: read → modify only the touched keys →
  write via `serde_json::Value`, preserving all unrelated keys (same contract
  as the `task-mgr models` family; reuse
  `loop_engine/config_io.rs::write_config_value_at`).
- **No split-brain config**: `resolve_embedding` prefers `embeddingProfile`
  over raw `embeddingModel` (verified at
  `src/learnings/embeddings/profiles.rs:90-129`). `switch-model` MUST remove
  the raw `embeddingModel` key when writing `embeddingProfile`, and say so in
  its output — a lingering raw key is dead config that misleads operators.
- **Coverage numbers come from the DB, not Ollama**: warning and delta use
  `count_embedded(conn, model)` vs active learnings — correct even when
  Ollama is down.
- **Prompt never fires non-interactively**: interactive gap-fill prompt is
  gated on `stdin.is_terminal() && stderr.is_terminal()` and suppressed in
  auto mode — exact precedent: `src/commands/models/ensure_default.rs:57`.
  Loop agents run `claude --print` (non-TTY): a blocking prompt would hang an
  autonomous run.
- **CONTRACT-LOG-001 discipline**: warning and all switch output go through
  `ui::emit` / `ui::emit_err` / `ui::prompt` (product UX). `--format json`
  stdout payloads must remain unpolluted (warnings are stderr-only).

### Performance Requirements

- Coverage check is two `COUNT(*)` queries — no measurable recall latency.
  Compute it **once per recall invocation** (command layer), not per backend
  call, and only when the vector backend is actually consulted
  (`--query` present).
- `switch-model` without `--embed-now` must complete without any Ollama
  round-trip except the availability probe (which has a 3s timeout and is
  non-fatal).

### Style Requirements

- Follow existing codebase patterns: no `.unwrap()` outside tests,
  `TaskMgrResult` error propagation, `thiserror` variants over ad-hoc strings.
- New CLI subcommand mirrors the clap conventions in
  `src/cli/commands.rs::CurateAction` (after_help EXAMPLES block, kebab-case
  flags, `conflicts_with` for mutually exclusive flags).
- Text output formatting lives in `src/commands/curate/output.rs`; result
  structs in `types.rs` with serde defaults for JSON-compat evolution.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --------- | -------------- | ----------------- |
| Unknown profile id (`switch-model jina-smal-q8` typo) | Silent acceptance would write dead config | Error listing known ids (reuse `resolve_embedding`'s error shape: `unknown embeddingProfile 'X'; known: …`); exit non-zero; config untouched |
| Raw `embeddingModel` already set in config | Profile wins at resolve time, but the stale raw key misleads | Remove `embeddingModel` key in the same write; print `removed raw embeddingModel (superseded by profile)` |
| Switch to the **already-active** profile | Idempotence; operators re-run commands | No-op on config (still reports coverage); exit 0 |
| `config.json` absent (fresh project) | First-run switch | Create file containing just the written keys |
| `config.json` malformed JSON | Overwrite would destroy operator's file | Error and abort before any write; never clobber |
| Ollama unreachable / model not pulled during switch | Model may still be downloading (docker rebuild) | Config IS written; probe failure is a warning with `ollama pull <model>` / `scripts/recall-stack-up.sh` hint; gap-fill prompt is skipped |
| `--embed-now` with model not pulled | Embed preflight fails after config write | Config stays switched (correct: the switch succeeded); embed error propagates with clear next step |
| Non-TTY stdin/stderr (loop, `--print`, CI) | Blocking prompt would hang autonomous runs | Never prompt; behave as lazy (print the `curate embed` hint) |
| Zero active learnings | Empty project; nothing to embed | No coverage warning; switch reports `0/0` |
| Recall `--for-task` only (no `--query`) | Vector backend not consulted | No coverage warning (nothing vector-scored) |
| Recall `--format json` | Machine consumers parse stdout | Warning on stderr only; JSON payload unchanged |
| `--rerank nemotron-rerank-1b` | Profile exists but is blocked on llama-box v0.0.171 (`llama-embed` arch) | Accept + write, but print the catalog `notes` blockage warning (SSoT: `src/learnings/reranker/profiles.rs`) |
| Mixed-dims rows under the target model (pre-existing corruption) | Gap-fill refuses in this state (post-`e1f4f6d` guard) | Coverage delta surfaces the refusal hint (`--force` needed) instead of prompting a gap-fill that will fail |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **CONTRACT-001 — `embedding_coverage` helper** (recommended contract task):
  - **Contract owner**: `src/learnings/embeddings/mod.rs`
  - **Signature**: `pub fn embedding_coverage(conn: &Connection, model: &str) -> TaskMgrResult<EmbeddingCoverage>` where
    `EmbeddingCoverage { model: String, total_active: i64, embedded: i64, missing: i64 }`
  - **Consumers (2+ → contract justified)**: (1) recall coverage warning
    (`src/commands/recall.rs`), (2) `curate switch-model` coverage delta,
    (3, incidental) `curate embed` status path may adopt it.
  - Composes existing `count_embedded` + the active-learnings count; no new
    SQL semantics.

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --------- | ----------------------- | ----------------------------- |
| config file → resolved embedding | JSON file (camelCase string keys: `embeddingProfile`, `embeddingModel`, `ollamaUrl`) → `ProjectConfig` (serde snake_case fields: `embedding_profile: Option<String>`) → `ResolvedEmbedding { model, expected_dims, profile_id, … }` | `let cfg = read_project_config(db_dir); let resolved = cfg.resolved_embedding().map_err(...)?; resolved.model` |
| config write (round-trip) | `serde_json::Value` (string keys, camelCase) — NOT `ProjectConfig` (serializing the typed struct would drop unknown keys) | `let mut v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?; v["embeddingProfile"] = json!(profile_id); v.as_object_mut().unwrap().remove("embeddingModel"); write_config_value_at(&path, &v)?` |
| reranker config | JSON `rerankerProfile` / `rerankerUrl` → `ProjectConfig.reranker_profile` → `resolve_reranker_pair(...)` (`src/learnings/reranker/profiles.rs:131`) | `cfg.resolved_reranker_config()` — returns `Option`; `None` = reranker disabled (URL unset) |
| coverage | SQLite → `EmbeddingCoverage` struct (typed fields) | `let cov = embedding_coverage(conn, &resolved.model)?; if cov.missing > 0 { ui::emit_err(...) }` |

**⚠ Type transition**: config **reads** go through the typed `ProjectConfig`
(snake_case fields), config **writes** must go through raw `serde_json::Value`
(camelCase string keys). Writing via the typed struct silently drops every key
it doesn't model — this is the #1 foot-gun for this PRD.

### Modularity & Coupling Targets

- **Target public surface**: 1 new CLI subcommand (`curate switch-model`),
  1 new command fn (`curate_switch_model`) + result struct, 1 shared helper
  (`embedding_coverage` + struct). No new DB columns, no migration.
- **Ownership**: switch workflow owned by `src/commands/curate/`; coverage
  helper owned by `src/learnings/embeddings/`; config write stays in
  `loop_engine/config_io.rs` (existing owner).
- **Coupling budget**: `commands/recall.rs` may call
  `learnings::embeddings::embedding_coverage` (already depends on that
  module); it must NOT grow a dependency on `commands/curate`.
  `curate_switch_model` must not reach into `commands/models/handlers.rs` —
  if writer logic needs sharing, it lives in `config_io.rs`.
- **Cohesion**: profile validation stays in the two catalogs
  (`embeddings/profiles.rs`, `reranker/profiles.rs`) — switch-model calls
  them, never duplicates the id lists.

---

## 3. User Stories

### US-001: Guided model switch

**As a** task-mgr operator experimenting with embedding models
**I want** `task-mgr curate switch-model <profile>` to validate, write config,
probe Ollama, and show me exactly where coverage stands
**So that** I can switch models in one step without hand-editing JSON or
remembering the gap-fill incantation.

**Acceptance Criteria:**
- [ ] Valid profile id → config written (round-trip safe), raw
      `embeddingModel` removed, coverage delta printed (embedded/total for the
      new model, rows retained per prior model, "switching back is instant"
      when prior rows exist).
- [ ] Invalid id → catalog-listing error, exit non-zero, config untouched.
- [ ] Ollama probe failure → config still written, actionable warning printed.
- [ ] TTY: prompted `Run gap-fill embed now? [y/N]` via `ui::prompt`;
      `y` runs the equivalent of `curate embed` (gap-fill, never `--force`).
- [ ] Non-TTY or `--no-embed`: no prompt, hint printed. `--embed-now`: no
      prompt, gap-fill runs. `--embed-now` and `--no-embed` conflict (clap).

### US-002: Reranker switch in the same command

**As a** task-mgr operator
**I want** `--rerank <profile>` on the same command
**So that** a paired model change (e.g. moving to the Nemotron stack) is one
invocation.

**Acceptance Criteria:**
- [ ] `--rerank <id>` validates against `RERANKER_PROFILES`, writes
      `rerankerProfile` in the same config write.
- [ ] Blocked/limited profiles print their catalog `notes` as a warning.
- [ ] No stored-state migration implied (reranker keeps no vectors); output
      says the change takes effect on next recall.
- [ ] Omitting `--rerank` leaves all reranker keys untouched.

### US-003: Recall tells me when coverage is incomplete

**As a** task-mgr user running `recall --query`
**I want** a warning when the active embedding model is missing vectors for
any active learnings
**So that** thin results are never mistaken for "there's nothing relevant".

**Acceptance Criteria:**
- [ ] `missing > 0` and vector backend consulted → exactly one stderr line,
      e.g. `[warn] vector recall: model 'X' covers 42/512 active learnings —
      run 'task-mgr curate embed' to gap-fill`.
- [ ] `missing == 0`, or no `--query`, or zero active learnings → silent.
- [ ] Fires identically with `--allow-degraded` (DB-derived, Ollama-independent).
- [ ] `--format json` stdout is byte-identical with and without the warning.

---

## 4. Functional Requirements

### FR-001: Recall coverage warning (Layer 1)

`recall` computes `embedding_coverage(conn, resolved.model)` once per
invocation, **only when** query text engages the vector backend, and emits a
single stderr warning when `missing > 0`.

**Details:**
- Threshold: **any gap** (`missing >= 1`), not a percentage (decision:
  "silence means fully covered").
- Placement: command layer (`src/commands/recall.rs`), NOT inside
  `VectorBackend` — backends are also used by dedup/near-dup paths where the
  warning would be noise, and the command layer already owns the analogous
  reranker soft-fail warning.
- Wording names the count, the model, and the exact remedy command.

**Validation:** integration test with mixed-model rows asserts warning
presence/absence and JSON stdout purity (see Known Edge Cases).

### FR-002: `task-mgr curate switch-model <profile>` (Layer 2)

One command: validate → write → probe → report → (optionally) embed.

**Details (execution order is normative):**
1. Validate `<profile>` via `find_embedding_profile`; validate `--rerank`
   via the reranker catalog. Any validation failure aborts before writes.
2. Read `config.json` as `serde_json::Value` (abort on parse error); set
   `embeddingProfile`, remove `embeddingModel`, set `rerankerProfile` iff
   `--rerank`; write via `write_config_value_at`.
3. Probe `OllamaEmbedder::is_available()` for the new model — warn-only.
4. Report: coverage delta for the new model, per-model retained rows
   (reuse `count_rows_by_model`), retention note, dims-guard hint if the
   target model's active rows have mixed dims.
5. Embed step: `--embed-now` → run gap-fill; `--no-embed` → hint only;
   neither + TTY (`stdin` AND `stderr`, not auto mode) → `ui::prompt`
   `[y/N]` defaulting to No; neither + non-TTY → hint only.

**Validation:** unit tests per step (config round-trip, raw-key removal,
idempotent re-switch, malformed-config abort); mockito for probe/embed paths
(pattern: `src/commands/curate/tests.rs::mock_ollama`).

### FR-003: Documentation alignment

`docker/SETUP.md` ("After changing the embedding profile" + failure-mode
table), `scripts/recall-stack-up.sh` hints, and
`src/commands/curate/CLAUDE.md` all point at `switch-model` as the canonical
flow; gap-fill/`--force`/`--prune-stale` remain documented as the underlying
primitives. Root `CLAUDE.md` command table refreshes via
`task-mgr enhance agents` (generated block).

---

## 5. Non-Goals (Out of Scope)

- **Auto-pulling the Ollama model** (`ollama pull` orchestration) — Reason:
  model serving is owned by the docker stack / operator; a 3s probe + hint is
  the right boundary. The stack script already handles weights.
- **Automatic pruning on switch** — Reason: retention IS the v21 feature;
  deleting is destructive and stays behind the explicit
  `--prune-stale --yes` two-step.
- **Reranker A/B or side-by-side model comparison** — Reason: high effort,
  demo-grade value today.
- **Auto re-embed daemon / file-watcher** — Reason: task-mgr is a CLI, not a
  service; lazy + warning covers the need.

---

## 5.5. Low-Value / High-Effort Areas (Explicit Cuts or Deferrals)

| Area / Capability | Why the value is low relative to cost | Rough effort cost | Recommended action |
| ----------------- | ------------------------------------- | ----------------- | ------------------ |
| Configurable coverage-warn threshold (`recallCoverageWarnBelow`) | Decision settled on any-gap; a knob adds config surface, docs, tests for a number nobody tunes | Low-Med | **Cut** (revisit only if warning proves noisy in practice) |
| Estimated embed time/cost in the coverage delta | Requires batch-size × latency modeling; count of missing learnings already conveys scale | Med | **Cut** for v1 — print the missing count only |
| `switch-model` driving the docker stack (`recall-stack-up.sh` invocation) | Couples the CLI to a repo script + docker; script is already self-serve | Med-High | Defer; print the script name in the probe-failure hint instead |

---

## 6. Technical Considerations

### Affected Components

- `src/learnings/embeddings/mod.rs` — add `embedding_coverage` +
  `EmbeddingCoverage` (CONTRACT-001).
- `src/commands/recall.rs` — coverage warning (FR-001), near the existing
  reranker soft-fail warn.
- `src/commands/curate/mod.rs` — `curate_switch_model` + `SwitchModelParams`
  / `SwitchModelResult` (types.rs), text formatter (output.rs).
- `src/cli/commands.rs` — `CurateAction::SwitchModel { profile, rerank,
  embed_now, no_embed }` with `conflicts_with`.
- `src/main.rs` — dispatch arm (mirrors the Embed arm's config resolution).
- `src/loop_engine/config_io.rs` — reused as-is (`write_config_value_at`);
  any shared read-modify-write helper lands here.
- Docs: `docker/SETUP.md`, `scripts/recall-stack-up.sh`,
  `src/commands/curate/CLAUDE.md`, root `CLAUDE.md` (enhance block).

### Dependencies

- Internal: v21 multi-model storage (`e1f4f6d`), embedding/reranker profile
  catalogs, `ui::prompt`, `IsTerminal` gating precedent.
- External: none new. mockito (dev) for tests.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| -------- | ---- | ---- | -------------- |
| **A. Warning in command layer + dedicated `switch-model` subcommand** | Warning fires exactly where a human reads it; no library-side UI (CONTRACT-LOG-001 clean); switch reuses catalogs + config_io; testable per step | One new subcommand to document | **Preferred** |
| B. Warning inside `VectorBackend::score_candidates` | Fires for every consumer automatically | Spams dedup/near-dup internal paths; library emits product UX (violates LOG-001 layering); fires per backend call not per invocation | Rejected |
| C. No new command — extend `curate embed` with `--switch <profile>` | Smallest CLI surface | Conflates "change config" with "do embedding work"; flag soup (`--switch --rerank --status --prune-stale --yes`); poor discoverability for THE headline workflow | Rejected |
| D. Config write via typed `ProjectConfig` serialization | Type-safe | **Silently drops every config key the struct doesn't model** (models/routing/blackouts…) — disqualifying | Rejected |

**Selected Approach**: A. Command-layer warning + dedicated subcommand,
config writes through raw `serde_json::Value` + `write_config_value_at`.

**Phase 2 Foundation Check**: the `embedding_coverage` contract costs ~an hour
now and becomes the shared substrate for any future coverage surface
(`doctor` check, `stats` line, status dashboards) — clear 1:10 trade.
Interactive-prompt gating copies a proven pattern rather than inventing one.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| ---- | ------ | ---------- | ---------- |
| Config write drops unrelated keys (typed-struct serialization sneaks in) | High — silently destroys models/routing config | Med | Data Flow Contract above names the trap; round-trip test with a maximal config fixture is a required acceptance criterion |
| Prompt hangs autonomous runs (loop agents, `--print`, CI) | High — stuck loop iteration | Med | TTY + auto-mode gate copied from `ensure_default.rs:57`; explicit non-TTY test; `--no-embed`/`--embed-now` escape hatches |
| Warning noise: persistent gap prints on every recall in a loop | Low-Med — log clutter, operator fatigue | Med | Single line, stderr-only; acceptable by design (nag until fixed); revisit threshold knob only if real-world noise reported (see 5.5) |

### Security Considerations

- No secrets touched; config values are profile ids validated against static
  catalogs (no arbitrary string written from user input except rejected-early
  ids).
- Config path stays inside `.task-mgr/` (db_dir-derived); no new file-write
  surface.

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --------------- | --------- | ----------------- | --------------- | ------------ |
| `embeddings::embedding_coverage` | `(conn: &Connection, model: &str)` | `EmbeddingCoverage { model, total_active, embedded, missing }` | `TaskMgrError` (DB) | none (read-only) |
| `curate::curate_switch_model` | `(conn: &Connection, db_dir: &Path, params: SwitchModelParams)` | `SwitchModelResult { profile_id, previous_profile, rerank_profile, removed_raw_model, coverage, rows_by_model, probe_ok, embedded: Option<EmbedResult> }` | `TaskMgrError` (validation / config parse / IO) | writes `config.json`; optional embed (Ollama + DB writes) |
| CLI `task-mgr curate switch-model <PROFILE> [--rerank <PROFILE>] [--embed-now \| --no-embed]` | clap | exit 0 | exit ≠0 on validation/parse error | as above |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --------------- | ----------------- | ------------------ | --------- | --------- |
| `recall` CLI stderr | reranker warn only | + coverage warn line | No (stderr additive) | none; see Consumers for snapshot-test review |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --------- | ----- | ------ | ---------- |
| `src/commands/recall.rs` (whole pipeline) | gains pre-retrieval coverage check | OK | command-layer only; single call |
| loop-engine iterations invoking `task-mgr recall` | parse stdout; stderr shown in logs | OK | warning is stderr; stdout contract untouched |
| `tests/` CLI snapshot tests asserting recall stderr | may assert exact stderr bytes | NEEDS REVIEW | audit `tests/` for recall stderr assertions before implementation; update fixtures in same task |
| `docker/SETUP.md` / `recall-stack-up.sh` readers | follow doc flow | OK | FR-003 rewrites hints to `switch-model` |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --------- | ------- | ---------------- | --------------------- |
| `VectorBackend` in recall vs dedup/near-dup | recall = user-facing; dedup = internal pre-filter | identical, silent | ONLY recall warns; dedup/near-dup stay silent (they tolerate partial coverage by design) |
| `curate embed` vs `switch-model`'s embed step | primitive vs workflow | n/a | switch-model's embed is EXACTLY gap-fill (no `--force`, no prune); primitives remain independently documented |
| probe failure vs validation failure | availability vs correctness | n/a | validation aborts pre-write; probe failure is post-write warn-only |

### Inversion Checklist

- [x] All callers identified and checked? (recall pipeline, curate dispatch,
      config readers via `read_project_config`)
- [x] Routing/branching decisions that depend on output reviewed? (loop
      engine consumes recall stdout only; stderr additive)
- [ ] Tests that validate current behavior identified? (recall stderr
      snapshot audit — flagged NEEDS REVIEW above; do first in implementation)
- [x] Different semantic contexts for same code discovered and documented?
      (Semantic Distinctions table)

### Documentation

| Doc | Action | Description |
| --- | ------ | ----------- |
| `docker/SETUP.md` | Update | "After changing the embedding profile" + failure-mode row point at `switch-model` |
| `scripts/recall-stack-up.sh` | Update | `--list-profiles` / config hints name `switch-model` |
| `src/commands/curate/CLAUDE.md` | Update | switch workflow section; coverage-warning contract |
| root `CLAUDE.md` | Update | command-reference row (regenerated via `task-mgr enhance agents`) |

---

## 7. Open Questions

- [ ] Should `switch-model` also accept raw catalog `ollama_model` strings
      (auto-mapping to the profile, mirroring `resolve_embedding`'s raw-match
      behavior), or profile ids only? (Default assumption: **profile ids
      only** — the raw escape hatch stays a hand-edit.)
- [ ] Exact auto-mode detection for the prompt gate: TTY check alone, or also
      honor the project `permissionMode: auto` config the way
      `ensure_default.rs` does? (Default assumption: **mirror
      `ensure_default.rs` exactly**.)

---

## Appendix

### Related Documents

- Commit `e1f4f6d` — v21 multi-model embedding coexistence + audit fixes
- `src/commands/curate/CLAUDE.md` — embedding/reranker config narrative
- `~/.claude/docs/task-mgr-best-practices.md` — post-PRD flow

### Glossary

- **Gap-fill**: `curate embed` default mode — embeds only active learnings
  missing a vector under the ACTIVE model.
- **Coverage**: `embedded / total_active` for one model; `missing` is the
  complement.
- **Profile**: catalog entry (id, ollama model, dims, prefixes) in
  `embeddings/profiles.rs` / `reranker/profiles.rs`; SSoT for validation.
