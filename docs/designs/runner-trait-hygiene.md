# LlmRunner Trait Hygiene: Eliminating Provider-Cleanup Drift

## Overview

`task-mgr` recently grew from a single-provider runner (Claude only) to a multi-provider runner (Claude + Grok, with the `LlmRunner` trait in `src/loop_engine/runner.rs` introduced by the `feat-grok-fallback-runner` PRD). The migration preserved every existing capability and added clean static dispatch (`enum RunnerKind` + `dispatch` free fn) — but it inherited one footgun verbatim: the per-call `cleanup_title_artifact: bool` opt-in flag that controls a Claude Code 2.1.110 workaround.

The footgun made itself visible in 2026-05 when a user opened an interactive `claude` session and found ~4,500 orphan `<uuid>.jsonl` ai-title metadata stubs accumulated across `~/.claude/projects/*/` over months of loop runs. Root cause: of 8 production spawn sites, 5 had opted into the cleanup flag at some point and 3 had never been touched. The accumulation was invisible — no error, no warning, no observability surface.

A focused /spike (2026-05-19) traced the same class of bug one provider deeper. The `grok` CLI has no `--no-session-persistence` equivalent at all and writes a *directory* of artifacts per session (`~/.grok/sessions/<percent-encoded-cwd>/<uuid>/` with `summary.json`, `resources_state.json`, `signals.json`, `plan_mode.json`, optionally `rewind_points.jsonl`, plus a per-cwd shared `prompt_history.jsonl`). The current `GrokRunner::spawn` in `runner.rs:489` destructures `cleanup_title_artifact: _` — silently ignoring the flag because there's no `--session-id` flag on `grok` to honor it. As drafted, every Grok-fallback iteration will leak a full session directory.

The bug class is more general than either provider:

> **Wrapper-internal workaround for an upstream-CLI quirk leaking through the wrapper's API surface as an opt-in flag with an unsafe default.**

Three principles violated:
1. **Side-effect ownership is in the wrong place.** The runner creates the artifact, but cleanup is delegated to the caller via a flag. Every new caller has to know about, and remember to opt into, a CLI-specific quirk that is none of their business.
2. **Unsafe default is the path of least resistance.** `..Default::default()` makes adding a new `RunnerOpts` call site cheap and ergonomic. When the default is wrong for production, every new site is silently wrong. The failure mode is invisible.
3. **No production caller exercises both branches.** The flag has one correct value in production (`true`). The 5 sites that opted in didn't make a judgment call; the 3 that didn't weren't expressing a preference. The flag is dead config that masks a defect.

The fix is structural: cleanup belongs to the runner abstraction, not to the caller. With the `LlmRunner` trait now in place, the natural extension is a `cleanup_session` method that each provider implements correctly for its own artifact shape. While we're inside the trait surface anyway, several other hygiene items become low-marginal-cost: capability discovery, error-taxonomy unification, a `RunnerArgs` builder, a `RunnerSession` newtype with RAII cleanup. The full hygiene pass is laid out below as a five-phase plan, each phase its own PRD.

## Goals

1. **Fewer footguns**: Adding a new provider or a new spawn site should not allow a silent artifact leak. The default behavior of the trait surface is correct.
2. **Cleanup ownership at the trait**: The function that creates a side-effect owns its disposal. The engine does not need to know that Claude 2.1.110 writes a stub or that Grok writes a directory; it asks the runner to clean up after itself.
3. **Cross-provider parity**: Capabilities the engine branches on (effort, stream-json, session-id, thinking tokens, permission modes) are discoverable from the runner, not hard-coded per provider in the engine.
4. **Greppable workarounds**: When Anthropic or xAI ships an upstream fix, dropping the workaround is a one-grep operation, not a multi-file archaeology dig.
5. **Incremental, verifiable progress**: Five hygiene items broken into five PRD-sized phases that each land with their own review point, tests, and rollback story.
6. **Preserved safety**: Every existing behavior — fallback-runner overflow rung, auth-failure short-circuit, slot-merge resolver, permission-mode mapping, the `cleanup_title_artifact_sync` `NotFound`-as-silent-success arm — survives or is replaced by something demonstrably stronger.

## Current-State Audit

The Phase 1 PRD's acceptance criteria depend on an honest inventory of where each affected piece lives today. Categories are deliberately separated because their semantics differ.

| Concern | Site(s) | Semantics |
|---|---|---|
| **A. `LlmRunner` trait** | `runner.rs:222` | `pub(crate) trait LlmRunner: Send + Sync`; method `spawn`; no `cleanup_session`. |
| **B. `RunnerOpts` struct** | `runner.rs:142–170`, derives `Default` at line 141 | The opt-in flag `cleanup_title_artifact: bool` lives at line 166. |
| **C. `ClaudeRunner` impl** | `runner.rs:242–470` | Owns the inline `cleanup_session_id: Option<Uuid>` injection at line 327 and the post-wait `cleanup_title_artifact_sync(uuid, working_dir)` call at line 422. |
| **D. `GrokRunner` impl** | `runner.rs:471–668` | Destructures `cleanup_title_artifact: _` at line 489. No cleanup logic; the field is silently no-op. |
| **E. `dispatch` free fn** | `runner.rs:877` | Static-dispatch entry point used by every engine call site. Currently has no post-spawn hook. |
| **F. Production opt-in sites** (`cleanup_title_artifact: true`) | `engine.rs:656` (slot iter), `engine.rs:2587` (sequential iter), `prd_reconcile.rs:672`, `curate/enrich.rs:247`, `curate/mod.rs:638`, `progress.rs:328`, `merge_resolver.rs:260`, `learnings/ingestion/mod.rs:115` | 8 sites, of which the first 3 were added in a 2026-05 tactical patch after the leak surfaced; the other 5 are pre-existing. |
| **G. Cleanup helper** | `claude.rs:756` (`pub(crate) cleanup_title_artifact_sync`) | Pure helper, called only by `runner.rs:422` and a small handful of `claude.rs` tests. `NotFound` from `remove_file` is silent success. |
| **H. Warn-once gate** | `claude.rs:739` (`static CLEANUP_WARN_ONCE: AtomicBool`) | File-private. Currently used only by the helper. |
| **I. CWD encoding helper** | `claude.rs:51` (decl), `claude.rs:41` (rustdoc) (`pub(crate) encoded_cwd_dir`) | Pure fn. Produces `~/.claude/projects/<dash-encoded-cwd>/`. No Grok counterpart exists. |
| **J. Tests on the silent-ignore contract (Grok)** | `tests/grok_runner_unit.rs` (3 sites) | Verify `GrokRunner::spawn` does not panic and does not inject `--session-id` when the flag is true. The contract ceases to exist post-Phase-1. |
| **K. Tests on the `cleanup_title_artifact_*` flag (Claude)** | `claude.rs:2948` (false-omits), `claude.rs:2959` (true-injects), `claude.rs:3020` (preserves-bystander), `claude.rs:3087` (deletes-target-preserves-bystander), `claude.rs:3121` (skips-when-home-unset), `claude.rs:3138` (missing-target-silent) | Six tests; two are flag-conditional (2948, 2959), four exercise the helper directly. |
| **L. Error taxonomy (current)** | `runner.rs` plus ad-hoc stderr-sniffing (`GROK_AUTH_FAILURE_SUBSTRINGS`, `stderr_contains_auth_failure`) | Each runner does its own pattern matching; no shared `RunnerError` enum. |
| **M. Flag-mapping table** | `tasks/grok-fallback-runner.json:587` (prose notes in the PRD JSON) | The claude↔grok flag-mapping is documented in PRD prose, not in typed code. Drift hazard. |

This audit becomes Phase 1's literal checklist. Every row that is touched in Phase 1 is named in the PRD. Rows touched in later phases are named in those PRDs and referenced back here.

## Proposed Refactorings

Five hygiene items, each scoped to a single PRD. They are ordered by foundational dependency, not by ease.

### 1. `cleanup_session` Contract + `FakeRunner` (Phase 1 — Highest Foundational ROI)

Add a single method to `LlmRunner`:

```rust
fn cleanup_session(
    &self,
    session_id: Uuid,
    cwd: &Path,
) -> Result<(), TaskMgrError> {
    Ok(())  // default: providers without headless artifacts are no-ops
}
```

Each provider implements correctly for its own artifact shape:

- **`ClaudeRunner::cleanup_session`** — promotes the existing `cleanup_title_artifact_sync` helper (`claude.rs:756`) into `runner.rs` as a runner-private fn. Deletes `~/.claude/projects/<dash-encoded-cwd>/<uuid>.jsonl`. `NotFound` is silent success.
- **`GrokRunner::cleanup_session`** — pre/post-spawn directory diff captures the new `<session-uuid>` inside `GrokRunner::spawn`; cleanup deletes `~/.grok/sessions/<percent-encoded-cwd>/<session-uuid>/` recursively. Never touches the shared per-cwd `prompt_history.jsonl`.
- The `cleanup_title_artifact: bool` field is removed from `RunnerOpts` entirely. The 8 production call sites + 3 Grok-runner-unit test sites get the field stripped. No per-site opt-in survives.
- `dispatch` (`runner.rs:877`) calls `runner.cleanup_session(session_id, cwd)` unconditionally post-spawn. The session id flows via a new `RunnerResult.session_id: Option<Uuid>` field. Cleanup failures are non-fatal and emit a single banner line of shape `[cleanup warn] <provider>: <summary> (<path>)`, rate-limited by the existing `CLEANUP_WARN_ONCE` static (visibility lifted to `pub(crate)` so both providers share the gate).
- `FakeRunner` is added under `#[cfg(test)]` so an integration test at `tests/runner_cleanup.rs` can drive `dispatch` end-to-end with each provider's artifact shape simulated, asserting the artifact is present after spawn and absent after dispatch returns.

`WORKAROUND(claude-code-2.1.110-session-stub)` and `WORKAROUND(grok-cli-no-persistence-off)` comment markers are placed at the exact lines where each cleanup lives. Future upstream-fix removals become a one-grep change.

**Why this is foundational**: It eliminates the bug class for both providers at once and establishes the post-spawn hook pattern that the RAII phase (item 5) will refine. Without item 1 there is nothing to RAII-ify.

### 2. Capability Discovery + Enforcement (Phase 2)

Introduce a typed capability surface so the engine can ask "can this runner do X?" instead of hard-coding provider-specific branches.

```rust
pub enum RunnerCapability { SessionId, Effort, StreamJson, ThinkingTokens, PermissionMode }

impl LlmRunner {
    fn supports(&self, _cap: RunnerCapability) -> bool { false }
}
```

Each runner overrides `supports` for the capabilities it has. The engine consults `runner.supports(cap)` at the dispatch boundary; if a `RunnerOpts` field encodes a capability the runner doesn't support, `dispatch` refuses with a typed error rather than letting the unsupported flag silently drop. This turns a class of "oops, Grok doesn't have that flag" integration bugs from silent stderr lines into hard errors at the dispatch boundary.

The initial enum set tracks exactly what the engine already branches on today; new capabilities are added when the engine needs a new branch.

### 3. Error Taxonomy Unification (Phase 3)

Each runner currently encodes its failure signals differently: Claude returns `TaskMgrError::IoError` plus a small ladder of stderr-sniffing in the engine; Grok adds `GROK_AUTH_FAILURE_SUBSTRINGS` and the `stderr_contains_auth_failure` helper. The engine branches on a mix of exit codes and substring matches.

Introduce a shared enum:

```rust
pub enum RunnerError {
    Auth, RateLimit, Overflow, Timeout, BinaryNotFound,
    ParseError, NetworkError, PermissionDenied, Other(String),
}
```

Each runner maps its stderr/exit-code signals into the same variants. The engine matches on variants instead of probing stderr in multiple places. The fallback-runner overflow rung and the auth-short-circuit hook (currently expressed as ad-hoc stderr matchers in `engine.rs`) become typed dispatches.

This is its own PRD because every error-handling call site in the engine has to migrate, and the call-site count is larger than item 2's.

### 4. Flag-Mapping Consolidation via `RunnerArgs` Builder (Phase 4)

The claude↔grok flag-mapping table lives today as prose notes in `tasks/grok-fallback-runner.json:587`. It is correct, but it is documentation, not code — and documentation drifts. Introduce a typed builder:

```rust
pub trait LlmRunner {
    fn args_builder(&self) -> Box<dyn RunnerArgsBuilder>;
}
```

Each runner exposes a `RunnerArgsBuilder` that the engine calls without knowing per-provider flag names. The builder asserts at compile time that every `RunnerOpts` field with a matching capability has a corresponding flag mapping, eliminating the "we added a field but forgot to wire it for the second provider" class.

### 5. RAII Session Tracking via `RunnerSession` Newtype (Phase 5)

After items 1–4 land, the explicit cleanup call inside `dispatch` is the last remaining "engine forgot to call cleanup" failure mode. Replace it with a `RunnerSession` newtype that owns the `session_id` and triggers `cleanup_session` on `Drop`. The engine holds a `RunnerSession`; when the iteration ends and the value is dropped, cleanup fires automatically without any explicit dispatch-site call.

This is sequenced last because (a) it depends on items 1 + 3 (cleanup contract + error taxonomy) being stable, and (b) introducing `Drop`-based cleanup while the surrounding code is mid-surgery would interact badly with the merge-back and overflow-recovery paths that already do their own DB transactions and stderr emissions around the iteration boundary.

## Synergy and Sequencing Analysis

### Synergistic Clusters

**Cluster A — Provider Hygiene Foundation (Items 1 + 5)**: Strongly coupled. Item 1 establishes the cleanup contract and the explicit dispatch-site call; item 5 replaces the explicit call with RAII. Item 5 cannot be sensibly designed before item 1 ships because the `Drop` impl needs the trait method to exist. They are deliberately *not* bundled because the explicit-call shape is a useful intermediate that we want in production for at least one release cycle to expose any cross-provider edge cases (parallel-slot concurrency, interactive-session collision, etc.) before adding `Drop` semantics on top.

**Cluster B — Engine ↔ Runner Surface (Items 2 + 3 + 4)**: Spiritually synergistic (all three reduce "engine has to know provider internals" hazards) but technically independent. Each operates on a different surface (capabilities, errors, args) and can be reviewed independently.

- Item 2 (capabilities) has the smallest blast radius and reads first.
- Item 3 (error taxonomy) is the largest call-site migration of the three.
- Item 4 (args builder) cleanly depends on item 2 — without `supports`, the builder cannot statically check that every capability has a flag mapping.

So Cluster B's natural order is 2 → 3 → 4, but 2 and 3 are commutative if needed.

### Recommended Phasing for Smooth Transition

**Phase 1 — `cleanup_session` + `FakeRunner` (foundational, ships first)**
- Item 1 only.
- The current tactical patch on `main` (`cleanup_title_artifact: true` added at 3 sites in 2026-05) is removed as part of this phase. No tactical fix survives.
- Heavy emphasis on the integration test at `tests/runner_cleanup.rs` — this is the safety net for every later phase.
- Goal: zero new artifact leaks after this PRD lands, and a `FakeRunner` seam that subsequent phases will reuse.

**Pre-Phase-1 coverage gates** (mandatory before extraction begins):
- Unit tests on `encoded_cwd_dir` (Claude) and `grok_encoded_session_dir` (Grok, new) — round-trip equality against observed on-disk paths.
- Snapshot test on the cleanup-warn banner format — operators may grep for `[cleanup warn]` at some future date; the prefix is contract.
- The existing `claude.rs:2948+` tests on the flag conditional pass under the migration plan (deletion + rename) without leaving orphan asserts.

**Phase 2 — `RunnerCapability` + enforcement (small surface, can follow Phase 1 immediately)**
- Item 2 only.
- Depends on Phase 1 only for the `FakeRunner` seam (capability tests use `FakeRunner` variants).
- Lands as a typed boundary at `dispatch`; existing engine branches that hard-code "if Claude do X else Y" get migrated to `if runner.supports(cap)` opportunistically — full migration not required for Phase 2 to ship.

**Phase 3 — `RunnerError` taxonomy (larger call-site migration; sequence after Phase 2 has stabilized)**
- Item 3.
- Each engine error-handling site is migrated; the fallback-runner overflow rung and auth-short-circuit get typed dispatches.
- This is the phase most likely to surface latent engine behavior — schedule extra review.

**Phase 4 — `RunnerArgs` builder (cleanest with capabilities in place)**
- Item 4.
- Depends on Phase 2.
- The drift-prone prose flag-mapping table is replaced by typed code.

**Phase 5 — `RunnerSession` RAII (last; supersedes Phase 1's explicit cleanup call)**
- Item 5.
- Depends on Phases 1 + 3.
- Lands as the final hygiene phase once the explicit-call shape has soaked for at least one or two production loop cycles.

**Why this order is smooth**:
- Phase 1 fixes the user-visible leak first.
- Each subsequent phase is independently reviewable.
- No phase invalidates the safety layers built for parallel-slot execution, overflow recovery, or auth detection; the new abstractions are required to preserve them (the tests enforce it).
- The RAII jump is held until last so the system is fully type-safe before introducing `Drop` semantics on top of an iteration boundary that already does substantial DB and stderr work.

## How This Document Becomes the Basis for Smaller PRD Unit Efforts

This design is deliberately **not** a single PRD. It is the parent narrative for five sequential PRDs.

Expected consumption path:

1. Review and ratify this document.
2. For Phase 1: a focused PRD ("LlmRunner Trait Hygiene — Phase 1: cleanup_session + FakeRunner") generated from this document plus the prior planning artifact at `~/.claude/plans/ultrathink-what-would-a-toasty-wozniak.md`.
3. For Phases 2–5: each gets its own short plan + `/prd` run + PRD, referencing the relevant section of this document as design context. The roadmap table in §"Proposed Refactorings" is the source of truth for what each phase covers.
4. Each PRD produces its own `tasks/<slug>.json` + prompt file and runs through the normal task-mgr loop.
5. After each landed phase, the relevant learnings are recorded via `task-mgr learn` and this document is lightly updated (or a "retrospective" appendix is added) before the next phase begins.

This keeps individual efforts inside the 5–15 task range that the loop engine handles well, while the shared design doc prevents the phases from drifting.

## Risks and Mitigations

- **Risk**: Phase 1 changes Claude artifact behavior — even though the byte-identical-output property holds (the same `cleanup_title_artifact_sync` helper is called for every Claude spawn), the call moves from inside `ClaudeRunner::spawn` to `dispatch`.
  - **Mitigation**: The integration test at `tests/runner_cleanup.rs` drives `dispatch` end-to-end with `FakeRunner` simulating Claude's single-file shape; assertion on artifact-present-post-spawn + artifact-absent-post-cleanup catches any wiring break. The existing `claude.rs:3087+` tests on the helper itself continue to pass unchanged after the helper is promoted (only the import path changes).

- **Risk**: Grok pre/post-spawn directory diff is racy if a concurrent interactive `grok` session opens a session in the same cwd between the snapshot and post-listing.
  - **Mitigation**: Each loop slot runs in its own worktree (distinct cwd → distinct encoded session dir). A parallel-slot integration test confirms the snapshot-diff is monotonic per-cwd. For the "user opens interactive grok in a worktree mid-loop" case, the diff must be defensive: pick the entry whose timestamp is closest to spawn-end, not just "the new entry."

- **Risk**: `cleanup_session` failures spray banner lines and overwhelm long loop output.
  - **Mitigation**: `CLEANUP_WARN_ONCE` rate-limits to one banner per process. Subsequent failures stay silent. Mirrors the existing pattern at `claude.rs:739+` precisely.

- **Risk**: Future phases (capability enforcement, error taxonomy) tighten the dispatch contract and surface latent engine bugs that were previously masked by ad-hoc handling.
  - **Mitigation**: This is feature, not bug. Each tightening phase has its own review; the FakeRunner seam from Phase 1 makes regression tests cheap to add.

- **Risk**: The "permanent workaround" markers (`WORKAROUND(claude-code-2.1.110-...)` / `WORKAROUND(grok-cli-no-persistence-off)`) become outdated stubs when upstream ships fixes, but nobody notices.
  - **Mitigation**: Each marker carries a "remove when fixed" comment plus a `grep -rn "WORKAROUND(...)" src/` smoke test mentioned in the relevant phase's verification block. CI does not enforce removal — discovery is by maintenance-window grep.

## Invariants That Must Be Preserved

- `LlmRunner` is `Send + Sync` (currently enforced at `runner.rs:222`) — required for parallel-slot dispatch.
- `cleanup_title_artifact_sync`'s `NotFound`-as-silent-success arm — required so the day Anthropic ships `--no-session-persistence` honor, the cleanup degrades to a no-op without log spam.
- `prompt_history.jsonl` (per-cwd, shared across Grok sessions) is **never** deleted by any cleanup path — required so interactive Grok history survives loop runs.
- The `RunnerResult` shape is `pub`, but only in-tree consumers exist (integration tests via the legacy `claude::ClaudeResult` alias) — adding fields is non-breaking; removing or renaming requires audit.
- Static dispatch via `RunnerKind` enum match (no `Box<dyn LlmRunner>` allocation on the hot path) — established by the Grok PRD; future phases must not regress to dynamic dispatch on every spawn.
- The `<task-status>` side-band tag contract, the overflow ladder rungs (1–5), and the auth-failure short-circuit all continue to work bit-identically post-refactor — every phase has a regression test demonstrating it.
- Permission-mode mappings (`auto`, `dontAsk`, `bypassPermissions`, `plan`) preserve their current semantics across providers — currently documented in the Grok PRD's flag-mapping table; Phase 4 codifies them.
- The five layers of parallel-slot cascade defenses (synthetic shared-infra slot, buildy-prefix heuristic, ephemeral overlay, consecutive-merge-fail halt, stale-ephemeral hygiene) are untouched — this work is orthogonal.

## Boundary Contract with Coherence Refactoring Effort

This effort runs in parallel with the broader Coherence Refactoring design at `docs/designs/coherence-refactoring.md`. The two plans target different primary concerns (provider-abstraction hygiene vs. task-lifecycle and orchestration ownership) but share the same high-risk edit surface: the iteration spawn + immediate post-processing window inside `src/loop_engine/engine.rs` (both the sequential `run_iteration` path and the slot/wave paths).

The detailed coordination contract, including the non-interference rule for the thin post-`dispatch` `cleanup_session` hook, ownership split during the overlap, module-layout implications for `runner.rs` as an existing peer, and cross-effort handling of the dogfood concurrency gate and `<task-status>` failure-semantics risks, lives in the §"Boundary Contract with Runner Trait Hygiene Effort" section of the coherence document.

For readers of this document, the practical implications are:

- The explicit `cleanup_session(...)` call introduced inside `dispatch` in Phase 1 must remain a **thin, single-purpose provider-cleanup hook**. No status, lifecycle, or reconciliation logic is added at that layer by either effort.
- Edits to the raw iteration skeletons in `engine.rs` that will later be carved by the coherence work must leave clear seams so the second effort does not require a second rewrite.
- The module-layout decision for `TaskLifecycle` (and future orchestration modules) in the coherence Phase 1 PRD(s) will treat `runner.rs` as an established peer; `runner` stays the narrow provider-abstraction layer.
- The `<task-status>` side-band tag contract and per-task partial-failure tolerance (explicitly called out as invariants in this document) are inputs to the TaskLifecycle centralization work. The transition shadow test harness defined in the coherence pre-Phase 1 gates is the shared verification mechanism.
- The dogfood N-iteration live-loop exit gate chosen for the coherence Cluster A will be scheduled in awareness of the soak expectations for this effort's Phase 1.

The two hygiene projects are complementary and can proceed without blocking each other provided the boundary defined in the coherence document is respected. When either Phase 1 PRD reaches code review, the other effort is listed as a "review for overlap" stakeholder.

## Next Steps

1. Review and ratify this document (especially the phasing — is item 5 last the right call, or should it be folded into item 1?).
2. Land Phase 1 via the PRD generated from `~/.claude/plans/ultrathink-what-would-a-toasty-wozniak.md` (target file: `tasks/prd-runner-trait-hygiene.md`).
3. After Phase 1 ships, soak in production loops for at least one cycle; gather any cross-provider edge-case learnings before Phase 2 begins.
4. Author Phase 2 (capabilities) plan + PRD using this document's §1.2 as design context.
5. Continue through Phases 3–5 on the schedule decided at review time.
6. After each phase lands, append a short retrospective section here and record the concrete learnings via `task-mgr learn` so future efforts inherit the knowledge.

This document is intended to be the stable context for a sequence of smaller, well-scoped PRD efforts rather than a single heroic rewrite. The bet is that pulling the cleanup workaround into the trait — and using the same effort to harden the rest of the trait surface — will make every subsequent provider integration cheaper and safer than adding the third one would otherwise be.
