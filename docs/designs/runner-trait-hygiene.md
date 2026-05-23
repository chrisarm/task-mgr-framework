# Runner Trait Hygiene — Design Document

Multi-phase effort to make `LlmRunner` a safe, typed, and auditable boundary
for adding new runner backends and capability-asymmetric fields.

## Motivation

`LlmRunner` had two structural footguns that compounded as new fields were
added to `RunnerOpts`:

1. **Silent destructure pattern** — runners that don't support a field use
   `field: _,` in their `spawn` destructure. The option is accepted, silently
   discarded, and the caller has no way to detect the no-op.
2. **Identity-dressed-as-capability** — branches like
   `if effective_runner != RunnerKind::Claude` in `engine.rs` sometimes encode
   capability checks (PTY support, cleanup hooks) behind a provider-identity
   test. Adding a third runner forces re-auditing every such branch to decide
   whether it should stay identity-gated or migrate.

Both footguns compound: a new field added to `RunnerOpts` must be plumbed
through both runners and every branch that cares about it — with no compile-
time forcing function to ensure completeness.

## Phase 1 — Session cleanup trait method

**Merged**: PR #11 (feat: Grok fallback runner) — **Note on branch lineage**

PR #11 introduced `GrokRunner` with `cleanup_title_artifact: _` as a silent
destructure (the pattern this Phase was designed to fix). On `main`, a
subsequent commit (fe10bc8) removed the `cleanup_title_artifact` field from
`RunnerOpts` and refactored it to an unconditional `LlmRunner::cleanup_session`
trait method. This `feat/runner-capability-enforcement` branch was forked from
PR #11 **before** that cleanup landed and was never rebased; as a result,
`cleanup_title_artifact` remains a `RunnerOpts` field on this branch's lineage.
Phase 2 (below) closes the gap by adding `TitleArtifactCleanup` to the typed
capability surface and enforcing it at dispatch, rather than relying on the
field removal that never arrived.

Also introduced `FakeRunner` for unit tests, replacing direct `spawn` calls in
test code.

## Phase 2 — RunnerCapability enum + dispatch enforcement

**Merged**: `feat/runner-capability-enforcement` branch (this PRD)

Introduced a typed capability surface so future capability-asymmetric fields
cannot regress to the silent-destructure anti-pattern:

- `RunnerCapability` enum — exhaustive, `pub(crate)`. Every variant represents
  one axis of runner asymmetry. Adding a variant is a compile error in every
  production `supports()` impl simultaneously.
- `LlmRunner::supports` — default `false` (fail-closed). Production runners
  implement with exhaustive matches; no wildcard arms allowed.
- `enforce_capabilities` + `CHECKS` registry — single source of truth mapping
  `RunnerOpts` fields to `RunnerCapability` variants. `dispatch` calls
  `enforce_capabilities` before the spawn match; any non-default capability-
  driven field on an unsupporting runner returns
  `TaskMgrError::UnsupportedRunnerCapability` before any subprocess launches.
- `FakeRunner::supports_fn` — per-capability test injection seam added to
  `FakeRunner` for unit-testing dispatch enforcement without real subprocesses.
- Removed `use_pty: _` from `GrokRunner::spawn`. Added `TitleArtifactCleanup`
  capability variant covering `cleanup_title_artifact` (Phase 1's removal of this
  field never landed on this branch — see Phase 1 note above). A grep lint in
  `tests/` fails CI if `<field>: _,` ever reappears for a capability-driven field.
- Audit of all 10 production `RunnerKind` match sites: all KIND-CORRECT (no
  migration needed — see retrospective below).

### Phase 2 roadmap row

| Phase | Status | Summary |
|---|---|---|
| Phase 1 | ✅ complete (main) | Session cleanup trait method; `cleanup_title_artifact` field removal landed on `main` only (not absorbed on this branch — Phase 2 covers it via `TitleArtifactCleanup` capability) |
| **Phase 2** | ✅ **complete** | `RunnerCapability` enum + `dispatch` enforcement + audit |
| Phase 3 | planned | Error taxonomy: structured `RunnerError` variants, retry policy per variant |
| Phase 4 | planned | Args builder: typed `RunnerArgs` replaces ad-hoc flag strings in spawn |
| Phase 5 | planned | RAII session tracking: `RunnerSession` guard owns subprocess lifetime |

### Phase 2 retrospective

**PRD landed**: `feat/runner-capability-enforcement` merged with all FEAT/FIX
tasks complete and the full test suite green.

**Audit results** (ANALYSIS-001 inventory):

- 10 production `RunnerKind` match sites audited across `runner.rs`,
  `engine.rs`, `overflow.rs`, and `display.rs`.
- 10 KIND-CORRECT: every site is a genuine provider-identity check, not a
  hidden capability discriminant.
- 0 CAPABILITY-MISLABELED: no sites qualified for migration.
- 0 sites migrated: the "migrate at least one CAPABILITY-MISLABELED branch"
  clause in the PRD was conditional — with zero qualifying sites, FEAT-006
  reduced to documenting the inventory and confirming classifications. This is
  the correct outcome: the engine's runner discrimination was already honest.

**Forward note for Phase 3**: The error taxonomy work (Phase 3) should build
on `TaskMgrError::UnsupportedRunnerCapability` as the model — a structured
error with `runner_kind`, `capability_name`, and `field_name` fields that
include enough context to identify the offending call site without log
correlation. The `RunnerCapability` enum is the natural discriminant for
per-variant retry policies.
