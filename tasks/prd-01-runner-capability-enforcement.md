# PRD: LlmRunner Trait Hygiene — Phase 2: `RunnerCapability` + Dispatch Enforcement

**Type**: Enhancement (introduces typed capability surface; converts silent flag-drops into hard errors)
**Priority**: P2 (Medium) — small blast radius; high foundational ROI for Phases 3-5
**Author**: Claude Code
**Created**: 2026-05-19
**Status**: Draft

> **Design context.** This PRD is Phase 2 of the five-phase roadmap documented in `docs/designs/runner-trait-hygiene.md` (§"Capability Discovery + Enforcement (Phase 2)"). Phase 1 (`cleanup_session` + `FakeRunner`) has merged. Phases 3-5 (error taxonomy, args builder, RAII session tracking) are explicitly out of scope. Boundary contract with the parallel coherence-refactoring `Engine Orchestration Boundaries` PRD is in §6.

---

## 1. Overview

### Problem Statement

The `LlmRunner` trait has two production implementations (`ClaudeRunner` at `src/loop_engine/runner.rs:240`, `GrokRunner` at `runner.rs:469`). Each runner's `spawn` method **silently destructures fields it does not support** with the `_` pattern:

```rust
// runner.rs:492 (GrokRunner::spawn)
let RunnerOpts {
    // ...
    use_pty: _,           // Claude-only PTY workaround; silently ignored on Grok
    // ...
} = opts;
```

This is a known footgun documented in the parent design doc:

> "Each runner does its own pattern matching ... ad-hoc stderr matchers ... silent stderr lines."

And in inline comments at the destructure sites:

> `cleanup_title_artifact: _` (pre-Phase-1, now removed) — *"silently ignored — no flag emitted, no post-run cleanup."*
> `use_pty: _` — *"PTY workaround is Claude-specific (Node.js line-buffering). Out of scope for v1; grok uses plain pipes."*

The pattern repeats: any future `RunnerOpts` field that one runner supports and another doesn't becomes a silent no-op at the call site, with no compile-time or runtime signal to the caller. When a third runner is added, the same shape will leak.

The engine has its own version of the problem. `engine.rs:5044` reads:

```rust
if effective_runner != RunnerKind::Claude {
    return Ok((escalated, None));
}
```

This hard-codes "the Grok runtime-error fallback hook only fires when the current runner is Claude" — but the underlying intent is "this hook only applies to providers that have a documented Opus-equivalent + fallback-target relationship," which is a *capability*, not a *kind*. As soon as a third runner is added with similar semantics, this branch silently breaks.

The bug class is: **provider-specific capability differences that the engine encodes as `RunnerKind` matches or that runners encode as silent `_` destructures**. Both shapes scale poorly past two providers.

### Background

Per `docs/designs/runner-trait-hygiene.md` §"Phase 2", the fix is a typed capability surface:

```rust
pub enum RunnerCapability {
    SessionId,        // can inject --session-id <uuid> for cleanup scoping
    Effort,           // honors --effort flag
    StreamJson,       // supports --output-format stream-json (or equivalent)
    ThinkingTokens,   // supports extended thinking tokens (forward-looking)
    PermissionMode,   // supports a permission-mode flag taxonomy
    Pty,              // honors use_pty (Claude-only today; isatty(1) shim)
    DisallowedTools,  // honors --disallowedTools / --disallowed-tools
}

impl LlmRunner {
    fn supports(&self, _cap: RunnerCapability) -> bool { false }
}
```

Each runner overrides `supports` for what it has. The engine consults `runner.supports(cap)` at the dispatch boundary; if a `RunnerOpts` field encoding a capability is set AND the runner doesn't support the capability, `dispatch` refuses with a typed error rather than letting the field silently drop inside the runner.

Engine branches that today match on `RunnerKind` migrate to `runner.supports(cap)` where the underlying intent is capability-driven. Branches that genuinely care about provider identity (e.g., provider-specific telemetry tags, auth-failure variant detection) keep their `RunnerKind` match — capability vs. kind is a deliberate distinction.

Relevant prior learnings consulted (`task-mgr recall`):
- **[2891]** — extract common subprocess scaffolding immediately when adding the second agent implementation. The same logic applies one level up: extract capability checks immediately rather than per-runner `_` destructures.
- **[2956]** — `RunnerKind` enum dispatch keeps allocation-free; no `Box<dyn LlmRunner>` on the hot path. This PRD preserves that — `supports` is a method on the existing static-dispatch path.
- **[1626]** (superseded by Phase 1) — opt-in cleanup flag threaded through spawn_claude signature. Same anti-pattern; `RunnerCapability` is the structural fix.
- Phase 1 `FakeRunner` seam (`runner.rs::tests` and `tests/runner_cleanup.rs`) — capability tests reuse this seam for `FakeRunner` variants that mock per-capability support.

### Intended Outcome

After this PRD lands:

- `RunnerCapability` enum exists, named, and exhaustive over the capability-driven fields in `RunnerOpts` today.
- `LlmRunner::supports(&self, RunnerCapability) -> bool` is a trait method with a `false` default; both `ClaudeRunner` and `GrokRunner` override for their actual capabilities.
- `dispatch(...)` at `runner.rs:877` validates that every `RunnerOpts` field encoding a capability the runner doesn't support is unset, returning a new typed error (`TaskMgrError::UnsupportedRunnerCapability { runner_kind, capability, hint }`) if not.
- The `_` destructure pattern in `GrokRunner::spawn` for `use_pty: _` is gone — either the field is rejected at `dispatch` (because Grok doesn't `supports(Pty)`) or Grok grows real PTY support (out of scope for this PRD).
- At least one `engine.rs` branch that currently hard-codes `RunnerKind::Claude` migrates to `runner.supports(<capability>)` where the intent is genuinely capability-driven. (Conservative: one explicit migration in this PRD; more in follow-up PRDs.)
- A new test in `tests/runner_capability.rs` exercises every capability against every runner via `FakeRunner` variants and confirms the dispatch refusal contract.

---

## 2. Goals

### Primary Goals

- [ ] Eliminate silent `_` destructures of unsupported `RunnerOpts` fields. Either the field is supported (and consumed) or the runner refuses at the dispatch boundary.
- [ ] Establish `RunnerCapability` as a typed surface that Phases 3-5 (error taxonomy, args builder, RAII) build on. Phase 4 in particular asserts at compile time that every `RunnerOpts` field with a matching capability has a flag mapping — that assertion needs `RunnerCapability` to exist.
- [ ] Make adding a new runner (Phase 6+: any future provider) cheaper. The new runner declares its capabilities, gets dispatch enforcement for free, and only needs to wire the supported fields.

### Success Metrics

- Zero `_` destructures of capability-driven `RunnerOpts` fields in `runner.rs` after this PRD lands (verified by grep lint test).
- `RunnerCapability` covers every `RunnerOpts` field that has a known provider asymmetry today: `Pty` (Claude-only), `StreamJson` (both), `Effort` (both), `PermissionMode` (both), `DisallowedTools` (both), `SessionId` (Claude-only today; Grok joined in Phase 1's `cleanup_session` work without a `--session-id` flag — see §6 Approaches for how to model this).
- `dispatch` returns `TaskMgrError::UnsupportedRunnerCapability` for at least one new test case per (runner × unsupported capability) pair.
- At least one `engine.rs` `RunnerKind`-match branch migrates to `runner.supports(cap)` with no behavior change.
- The `FakeRunner` test seam from Phase 1 is reused for capability tests; no new test-only runner abstraction is introduced.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Dispatch enforcement is fail-closed.** If a `RunnerOpts` field is set and the runner doesn't `supports` the capability, dispatch returns `Err(UnsupportedRunnerCapability { ... })` BEFORE spawning a subprocess. No subprocess is spawned with an unsupported configuration.
- **`supports` is total.** Every `RunnerKind` × `RunnerCapability` pair has an explicit `true`/`false` answer. The default `false` on the trait is a fallback for forward-compatibility; both production runners override and decide each capability deliberately.
- **Capability semantics match the upstream CLI.** `StreamJson` for Claude means `--verbose --output-format stream-json`; for Grok means `--verbose --output-format streaming-json`. The capability name abstracts the spelling difference (Phase 4 will codify the spelling difference; this PRD just declares both runners support the capability).
- **No regression in observable behavior** for currently-correct call sites. Every loop run that worked pre-PRD runs the same post-PRD because today's call sites don't set unsupported fields on the wrong runner. The dispatch refusal only fires for NEW or FUTURE call sites that would have silently broken.
- **Provider-specific code paths that genuinely depend on `RunnerKind` (not capability) stay as `RunnerKind` matches.** Example: Grok auth-failure detection (`stderr_contains_auth_failure` + `GROK_AUTH_FAILURE_SUBSTRINGS`) is Grok-specific stderr-sniffing, not a capability the dispatch boundary should abstract. The migration in FR-005 is opportunistic, not blanket.

### Performance Requirements

- **`supports(cap)` is a constant-time match.** No allocation, no hashmap. Implementations are `match cap { ... }` over the enum.
- **Dispatch enforcement adds at most one capability iteration per spawn.** The check is a loop over the (at most ~7) capability variants, each touching one `RunnerOpts` field. No measurable hot-path cost.
- **Zero allocation in the new error variant's hot path.** `UnsupportedRunnerCapability` uses `&'static str` for `capability` and `RunnerKind` (Copy) for `runner_kind`. The `hint` field is a borrowed `&'static str`.

### Style Requirements

- **`RunnerCapability` is a non-exhaustive enum (`#[non_exhaustive]`) so adding a capability later is not a breaking change** for downstream `match` sites. Production matches are `match cap { Foo => ..., Bar => ..., _ => unreachable!("new variant must be handled") }` or, preferred, exhaustive matches that fail to compile when a variant is added (forcing the maintainer to handle it).
- **Default `supports` returns `false`.** A new runner that forgets to override `supports` opts out of every capability — the safest default. The trait doc comments this explicitly.
- **No `.unwrap()` or `.expect()` in dispatch enforcement.** The capability validation builds a `Result` directly.
- **Imports stay narrow:** no `use crate::loop_engine::runner::*` in tests or production.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Caller sets `RunnerOpts { use_pty: true, .. }` and dispatches to `RunnerKind::Grok` (no Pty support) | Today this silently drops the flag (`use_pty: _` in `GrokRunner::spawn`) — the call appears to succeed but PTY behavior is absent | Post-PRD: `dispatch` returns `Err(UnsupportedRunnerCapability { runner_kind: Grok, capability: "Pty", hint: "Grok uses plain pipes; remove use_pty or dispatch to Claude" })` BEFORE spawning |
| Caller sets `RunnerOpts::default()` and dispatches to either runner | Today succeeds; default values represent "no opinion" for capability-driven fields | Post-PRD: succeeds unchanged. The check is only triggered when a capability-driven field is set to a non-default value (e.g., `use_pty: true`, `effort: Some("...")`, `stream_json: true`). Default fields are no-op. |
| `RunnerCapability::ThinkingTokens` is declared but no `RunnerOpts` field references it today | The variant is forward-looking (Phases 3-5 may add a `thinking_tokens` field) | Both runners declare `supports(ThinkingTokens) = false` until a field exists. The variant is reserved; no dispatch check fires because no field encodes it. |
| Both `ClaudeRunner` and `GrokRunner` declare `supports(StreamJson) = true` but emit different flag strings (`stream-json` vs `streaming-json`) | The capability surface abstracts the spelling difference; Phase 4 (`RunnerArgs` builder) codifies the mapping | This PRD: both return `true` from `supports`; the inside-the-runner branching keeps emitting the right flag string. Phase 4's job to centralize the mapping. |
| Test code constructs a `FakeRunner` with a specific capability set | Capability tests need this for (runner × capability) coverage | `FakeRunner` is extended to carry a `supports_fn: fn(RunnerCapability) -> bool` (or equivalent) so tests can simulate any capability matrix. The Phase 1 `FakeRunner` is the seam. |
| Caller passes `Some("auto")` for `effort` to a runner that supports `Effort` but rejects the value `"auto"` | Capability check passes (the field is set + supported); the value-level validation is the runner's concern | Capability enforcement is field-presence, not value-validity. Value-level rejection (e.g., "this effort level is unknown to Grok") happens inside the runner's `spawn`, returns an error variant other than `UnsupportedRunnerCapability`. |
| Engine's `engine.rs:5044` branch (`if effective_runner != RunnerKind::Claude`) gates the Grok runtime-error fallback hook | The intent is "this hook fires when the current runner is Claude AND we've exhausted Opus" — `RunnerKind::Claude` IS the right check here (it's identity, not capability) | Stay as `RunnerKind` match. The PRD's opportunistic migration (FR-005) picks a different branch where the intent IS capability-driven. |
| Two capabilities are mutually exclusive (e.g., a future `Pty` vs. `RawPipe` distinction) | Not relevant today | Out of scope. If it arises, model as separate capabilities; let the runner decide via `supports`. |

---

## 3. User Stories

### US-001: Maintainer adds a new `RunnerOpts` field that one runner supports

**As a** task-mgr maintainer adding a hypothetical `--reasoning-mode` flag that only Claude supports
**I want** the dispatch boundary to reject any call that sets the flag for a Grok task
**So that** the field cannot become a silent no-op

**Acceptance Criteria:**
- [ ] The maintainer adds a new `RunnerCapability::ReasoningMode` variant and a new field to `RunnerOpts`.
- [ ] `ClaudeRunner::supports(ReasoningMode) = true`; `GrokRunner::supports(ReasoningMode) = false` (default fallback).
- [ ] `dispatch` checks the field; setting it on a Grok call returns `Err(UnsupportedRunnerCapability { runner_kind: Grok, capability: "ReasoningMode", hint: <maintainer-provided> })`.
- [ ] No `_` destructure in `GrokRunner::spawn` is added.

### US-002: Reviewer audits an engine branch that hard-codes a runner kind

**As a** code reviewer
**I want** to be able to tell whether a `RunnerKind::Claude` match in `engine.rs` is intent-correct (provider identity) or intent-wrong (capability dressed as identity)
**So that** the bug class doesn't recur as a third runner is added

**Acceptance Criteria:**
- [ ] Every existing `RunnerKind::Claude` / `RunnerKind::Grok` match in `src/` is audited (per FR-005); each is annotated as either KIND (intent-correct) or CAPABILITY-MISLABELED (should migrate).
- [ ] At least one CAPABILITY-MISLABELED branch is migrated to `runner.supports(cap)` in this PRD.

### US-003: Future Phase 4 (RunnerArgs builder) author depends on the capability surface

**As the** Phase 4 author
**I want** `RunnerCapability` to already exist so I can statically assert that every field with a matching capability has a flag mapping
**So that** Phase 4 doesn't have to introduce both the capability surface AND the args builder in one PRD

**Acceptance Criteria:**
- [ ] `RunnerCapability` is `pub(crate)` (or `pub` if `RunnerArgs` will live in a sibling module that needs it).
- [ ] The variant set covers every capability-driven field in `RunnerOpts` today.
- [ ] The trait method `supports` is overridable per-runner.

---

## 4. Functional Requirements

### FR-001: Introduce `RunnerCapability` enum

Add to `src/loop_engine/runner.rs`:

```rust
/// Capabilities a runner can declare. Used by `dispatch` to validate at
/// the boundary that a `RunnerOpts` field encoding a capability is only
/// set when the runner supports the capability.
///
/// `#[non_exhaustive]` so future variants don't break downstream matches.
/// Production runners use exhaustive matches without a wildcard arm so the
/// compiler forces them to handle every variant.
///
/// Every variant in this enum corresponds to an enforced row in the
/// `enforce_capabilities` checks table. Declarative-only variants are
/// an anti-pattern (half-state: declared but never gated); reintroduce
/// SessionId / PermissionMode / ThinkingTokens only when there is a
/// `RunnerOpts` field to gate.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RunnerCapability {
    /// `--effort <level>` flag.
    Effort,
    /// `--output-format stream-json` (or provider-specific equivalent).
    StreamJson,
    /// `use_pty` PTY-master output redirection (Claude-only today).
    Pty,
    /// `--disallowedTools` / `--disallowed-tools` flag.
    DisallowedTools,
}
```

**Validation:**
- The enum has every variant listed above.
- Tests in `runner.rs::tests` cover the trait method via `FakeRunner`.

### FR-002: Add `supports` to `LlmRunner` with `false` default

Add to the `LlmRunner` trait at `runner.rs:222`:

```rust
pub(crate) trait LlmRunner: Send + Sync {
    /// Spawn the runner's CLI...
    fn spawn(...) -> TaskMgrResult<RunnerResult>;

    /// Whether this runner supports `cap`. Default: false. Override per-runner.
    /// Used by `dispatch` to refuse calls that set a `RunnerOpts` field
    /// encoding an unsupported capability before any subprocess is spawned.
    fn supports(&self, _cap: RunnerCapability) -> bool {
        false
    }
}
```

**Validation:**
- A `FakeRunner` that does not override `supports` returns `false` for every capability.
- Both `ClaudeRunner` and `GrokRunner` override `supports`.

### FR-003: `ClaudeRunner` and `GrokRunner` override `supports`

```rust
impl LlmRunner for ClaudeRunner {
    // ... existing spawn ...
    fn supports(&self, cap: RunnerCapability) -> bool {
        match cap {
            RunnerCapability::SessionId => true,
            RunnerCapability::Effort => true,
            RunnerCapability::StreamJson => true,
            RunnerCapability::ThinkingTokens => false, // forward-looking
            RunnerCapability::PermissionMode => true,
            RunnerCapability::Pty => true,
            RunnerCapability::DisallowedTools => true,
        }
    }
}

impl LlmRunner for GrokRunner {
    // ... existing spawn ...
    fn supports(&self, cap: RunnerCapability) -> bool {
        match cap {
            RunnerCapability::SessionId => false,  // grok has no --session-id flag
            RunnerCapability::Effort => true,
            RunnerCapability::StreamJson => true,
            RunnerCapability::ThinkingTokens => false,
            RunnerCapability::PermissionMode => true,
            RunnerCapability::Pty => false,  // plain pipes only
            RunnerCapability::DisallowedTools => true,
        }
    }
}
```

**Validation:**
- A unit test asserts every (runner × capability) pair returns the expected value.
- Matches are exhaustive — adding a new variant to `RunnerCapability` causes both impls to fail to compile until updated.

### FR-004: `dispatch` enforces capability presence

At `runner.rs:877`, modify `dispatch` to validate `RunnerOpts` against the runner's `supports` before delegating to `spawn`:

```rust
pub fn dispatch(
    kind: RunnerKind,
    prompt: &str,
    permission_mode: &PermissionMode,
    opts: RunnerOpts<'_>,
) -> TaskMgrResult<RunnerResult> {
    let runner: &dyn LlmRunner = match kind {
        RunnerKind::Claude => &ClaudeRunner,
        RunnerKind::Grok => &GrokRunner,
    };

    // Capability validation: every RunnerOpts field that encodes a capability
    // must be supported by the runner OR be at its default ("no opinion").
    enforce_capabilities(runner, kind, &opts)?;

    match kind {
        RunnerKind::Claude => ClaudeRunner.spawn(prompt, permission_mode, opts),
        RunnerKind::Grok => GrokRunner.spawn(prompt, permission_mode, opts),
    }
}

fn enforce_capabilities(
    runner: &dyn LlmRunner,
    kind: RunnerKind,
    opts: &RunnerOpts<'_>,
) -> TaskMgrResult<()> {
    // For each (capability, "is the corresponding field set non-default?") pair:
    let checks: &[(RunnerCapability, bool, &'static str)] = &[
        (RunnerCapability::Pty, opts.use_pty, "use_pty"),
        (RunnerCapability::StreamJson, opts.stream_json, "stream_json"),
        (RunnerCapability::Effort, opts.effort.is_some_and(|e| !e.is_empty()), "effort"),
        (RunnerCapability::DisallowedTools, opts.disallowed_tools.is_some_and(|d| !d.is_empty()), "disallowed_tools"),
        // PermissionMode is always set (the type doesn't have a "no opinion"
        // variant), so it's not a per-call enforcement target — both runners
        // declare support and the spawn body branches on the enum.
        // SessionId is consumed by cleanup_session post-spawn, not RunnerOpts directly.
        // ThinkingTokens has no field today.
    ];

    for (cap, field_is_set, field_name) in checks {
        if *field_is_set && !runner.supports(*cap) {
            return Err(TaskMgrError::UnsupportedRunnerCapability {
                runner_kind: kind,
                capability_name: capability_name(*cap),
                field_name,
            });
        }
    }
    Ok(())
}

fn capability_name(cap: RunnerCapability) -> &'static str {
    match cap {
        RunnerCapability::SessionId => "SessionId",
        RunnerCapability::Effort => "Effort",
        RunnerCapability::StreamJson => "StreamJson",
        RunnerCapability::ThinkingTokens => "ThinkingTokens",
        RunnerCapability::PermissionMode => "PermissionMode",
        RunnerCapability::Pty => "Pty",
        RunnerCapability::DisallowedTools => "DisallowedTools",
    }
}
```

**Details:**
- The static `checks` table is the single registry of "field encoding a capability" mappings. Adding a new capability-driven field to `RunnerOpts` requires adding a row here AND a `RunnerCapability` variant AND updating both runners' `supports`.
- `PermissionMode` is excluded from per-call enforcement because the type doesn't have a "no opinion" variant. If a runner ever stops supporting it, the check pattern changes; for now both runners support it and dispatch unconditionally.
- `SessionId` is excluded because it's consumed by `cleanup_session` post-spawn (Phase 1), not by a `RunnerOpts` field. A future field would re-trigger the enforcement.

**Validation:**
- `tests/runner_capability.rs` exercises every check and asserts:
  - (Claude × `use_pty: true`) → `Ok` (Claude supports Pty)
  - (Grok × `use_pty: true`) → `Err(UnsupportedRunnerCapability { ... capability_name: "Pty", field_name: "use_pty" })`
  - (Claude × `stream_json: true`) → `Ok`
  - (Grok × `stream_json: true`) → `Ok`
  - (Both × `RunnerOpts::default()`) → `Ok`

### FR-005: New error variant `TaskMgrError::UnsupportedRunnerCapability`

Add to the project's error enum (likely `src/errors.rs` or wherever `TaskMgrError` is defined):

```rust
#[error("runner {runner_kind:?} does not support capability {capability_name} (field {field_name:?} was set)")]
UnsupportedRunnerCapability {
    runner_kind: crate::loop_engine::runner::RunnerKind,
    capability_name: &'static str,
    field_name: &'static str,
},
```

**Details:**
- The error is fail-closed at dispatch; the engine treats it as a non-recoverable spawn failure (same shape as `BinaryNotFound`-class errors today).
- Operators see a clear stderr line: `runner Grok does not support capability "Pty" (field "use_pty" was set)`. Includes enough context to identify the offending call site.

**Validation:**
- The error variant exists, is `#[error]`-annotated for `thiserror`, and round-trips correctly in `Display`.

### FR-006: Opportunistic engine migration — at least one `RunnerKind` match → `supports`

Audit every existing `RunnerKind::Claude` / `RunnerKind::Grok` match in `src/` (see initial inventory from `grep -n "RunnerKind::Grok\|RunnerKind::Claude" src/loop_engine/engine.rs` — ~15 sites). For each, classify:

- **KIND-CORRECT** (provider identity): the branch reads "if this is Claude, use Claude-specific stderr-sniffing / telemetry tag / fallback target". Stays as `RunnerKind` match. Annotate with a one-line comment `// kind-correct: provider identity, not capability`.
- **CAPABILITY-MISLABELED** (capability dressed as kind): the branch reads "if this is Claude, do X" where X is actually "if this runner supports capability Y, do X". Migrate to `runner.supports(cap)`.

This PRD migrates **at least one** CAPABILITY-MISLABELED branch and annotates the audit results inline. Migrating all is the work of Phases 3-5.

**Initial candidate for migration (committed during PRD review):**
- `engine.rs:5044` — `if effective_runner != RunnerKind::Claude { return Ok((escalated, None)); }` in `escalate_task_model_if_needed_inner`. The intent appears KIND-CORRECT (the Grok runtime-error fallback hook fires from Claude only because Grok is the fallback target — promoting Grok to "fallback for itself" is nonsense). **Recommendation: stays as `RunnerKind` match, annotated KIND-CORRECT.**
- The first CAPABILITY-MISLABELED branch to migrate is identified during the FR-005 audit. If none qualify cleanly, the PRD ships with the audit annotations only and the migration is deferred to Phase 3-5 PRDs.

**Validation:**
- A document in `docs/designs/runner-trait-hygiene.md` (or a new appendix) lists every audited site with its classification.
- At least one CAPABILITY-MISLABELED migration ships (if any qualify), with a test that asserts pre/post behavior is identical for both Claude and Grok cases.
- The PRD is honest about cases where the audit found no clean CAPABILITY-MISLABELED branches — the capability surface is still valuable for Phase 4 even if the engine has zero existing migrations.

### FR-007: Remove `_` destructure of `use_pty` in `GrokRunner::spawn`

Once FR-004 lands, `GrokRunner::spawn` never sees a `RunnerOpts` with `use_pty: true` — dispatch refuses earlier. The `use_pty: _,` line at `runner.rs:492` can be removed from the destructure (the field is no longer accessed inside the function).

**Choice point:** keep the explicit `use_pty: _,` line as documentation that the field exists but is unused, OR remove it. **Recommendation: remove it.** The capability enforcement is the explanation now; the destructure line was only there because Rust forces explicit destructure of unmatched fields when using `..` shorthand is undesirable for clarity. Once the capability surface is the source of truth, the destructure noise is redundant.

**Validation:**
- A grep lint test verifies that no `<field>: _,` pattern exists in any `LlmRunner::spawn` impl for fields that map to a `RunnerCapability` and are unsupported by that runner.

### FR-008: `FakeRunner` capability extension

The Phase 1 `FakeRunner` in `tests/runner_cleanup.rs` (or `runner.rs::tests`) is extended so tests can configure which capabilities the fake supports:

```rust
#[cfg(test)]
pub(crate) struct FakeRunner {
    // ... existing fields ...
    pub supports_fn: fn(RunnerCapability) -> bool,
}

#[cfg(test)]
impl LlmRunner for FakeRunner {
    // ... existing spawn ...
    fn supports(&self, cap: RunnerCapability) -> bool {
        (self.supports_fn)(cap)
    }
}
```

**Validation:**
- A test in `tests/runner_capability.rs` constructs a `FakeRunner` with `supports_fn: |_| false` and asserts dispatch refuses every capability-driven field.
- Another constructs a `FakeRunner` with `supports_fn: |_| true` and asserts dispatch accepts every field.

### FR-009: `WORKAROUND` comment markers

The Phase 1 PRD established `WORKAROUND(<provider>-<short-issue>)` markers so future upstream-fix removal is one-grep. This PRD does NOT remove or alter those markers. New code added in this PRD (the capability enum, `supports` implementations, the `enforce_capabilities` helper) introduces no new workarounds — they're typed surfaces, not quirks-around-quirks.

If during implementation a workaround is discovered that the capability surface could replace, the discovery is documented in an Open Question (§7) for review, not silently merged.

**Validation:**
- A grep for `WORKAROUND(` in `src/loop_engine/runner.rs` returns the same set of markers pre- and post-PRD.

---

## 5. Non-Goals (Out of Scope)

- **Phase 3 (Error Taxonomy Unification).** The `RunnerError` enum and the engine's stderr-sniffing migration are a separate PRD. This PRD only adds `UnsupportedRunnerCapability` to the error type.
- **Phase 4 (RunnerArgs Builder).** The compile-time assertion that every capability has a flag mapping is Phase 4. This PRD provides the capability surface; the builder consumes it later.
- **Phase 5 (RunnerSession RAII).** The Drop-based cleanup contract is Phase 5; this PRD does not introduce `Drop` semantics.
- **Migrating every `RunnerKind` match in the engine.** FR-005 audits all; this PRD migrates at most one as an opportunistic example. Bulk migration is for Phases 3-5.
- **Adding new runner capabilities.** ThinkingTokens is declared but no field encodes it yet; the variant is reserved. No new `RunnerOpts` field is added in this PRD.
- **Touching `src/loop_engine/engine.rs` beyond the one opportunistic migration AND beyond what FR-005's audit annotations require.** The parallel coherence-refactoring `Engine Orchestration Boundaries` PRD is carving that file; this PRD stays narrow.
- **Replacing `RunnerKind` with capability-based dispatch.** `RunnerKind` is the right shape for static dispatch and for genuine provider identity. The capability surface complements it, doesn't replace it.
- **Removing `PermissionMode` from the per-call API.** PermissionMode is always required, both runners support it, and the type encodes the answer. It's not a capability gate.

---

## 6. Technical Considerations

### Affected Components

| File | Change |
| --- | --- |
| `src/loop_engine/runner.rs` | Adds `RunnerCapability` enum, `supports` trait method, `enforce_capabilities` helper, dispatch wiring. Removes `use_pty: _,` from `GrokRunner::spawn` destructure (FR-007). |
| `src/errors.rs` (or wherever `TaskMgrError` lives) | Adds `UnsupportedRunnerCapability` variant. |
| `src/loop_engine/engine.rs` | At most one branch migrates from `RunnerKind` match to `runner.supports(cap)`. Audit-annotation comments added to every existing match. |
| `tests/runner_capability.rs` (NEW) | Per-capability test matrix, dispatch refusal contract test, `FakeRunner` extension. |
| `tests/runner_cleanup.rs` | If `FakeRunner` lives here, extend with `supports_fn` field. |
| `src/loop_engine/CLAUDE.md` | Add a short subsection under "LLM runner dispatch" pointing at `supports` + `RunnerCapability` and the dispatch enforcement contract. |
| `docs/designs/runner-trait-hygiene.md` | Add a brief "Phase 2 retrospective" appendix once this PRD lands. |

### Dependencies

- **Phase 1 PRD MUST be merged** (it is, per the current branch state — `8cc50ff5-MILESTONE-FINAL` is `done`).
- No new external crates.
- `thiserror` is already in use for `TaskMgrError`.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| **A. Single capability enum + per-runner `supports`** (this PRD) | One typed surface; static dispatch unchanged; small diff; sets up Phase 4 cleanly | Requires per-runner exhaustive `match` updates when a variant is added — but that's the intended forcing function | **Preferred** |
| **B. Per-capability marker traits** (e.g., `trait SupportsPty: LlmRunner {}`, runners `impl SupportsPty`) | Compile-time guarantee; can't dispatch a Pty-using call to a non-Pty runner at the type level | Requires generic dispatch infrastructure or trait-object soup; conflicts with the existing `RunnerKind` static-dispatch enum; massive Phase 4/5 ripple | Rejected — too heavy for the marginal type-safety win |
| **C. `RunnerOpts` becomes provider-specific** (e.g., `ClaudeRunnerOpts`, `GrokRunnerOpts`) | No silent drops by construction; the type system rejects wrong-runner calls | Massive churn at every spawn site; breaks the unified `RunnerOpts` ergonomic that Phase 1 deliberately preserved | Rejected — fights Phase 1's choices |
| **D. Capability declared on `RunnerKind`, not on the trait** (e.g., `impl RunnerKind { fn supports(self, cap) -> bool { match self { ... } } }`) | No dyn dispatch; pure compile-time | `FakeRunner` (test seam) can't override; can't introduce a "third runner that mimics Claude's capabilities" in tests | Rejected — defeats the FakeRunner seam |

**Selected Approach**: **A. Single enum + trait method**. Rationale:
1. Smallest blast radius. The `LlmRunner` trait already exists; adding one method is one diff site per impl.
2. Reuses the `FakeRunner` seam from Phase 1. No new test abstraction.
3. The static-dispatch `RunnerKind` enum path stays the hot path (`dispatch` matches on `kind` and calls the concrete runner directly). The `&dyn LlmRunner` used for `enforce_capabilities` is a brief borrow with zero allocations and one indirect call — negligible.
4. Sets up Phase 4 cleanly: the args builder iterates `RunnerCapability` variants.

**Phase 2 Foundation Check**: Approach A costs ~0.5 days more than "just remove the silent `_` destructures and hope" but unlocks Phases 3-5 cleanly. Without `RunnerCapability`, Phase 4's compile-time flag-mapping assertion needs to invent the capability surface AND the args builder in one PRD — easily 2-3x the work. 1:5 ratio at minimum, plus the runtime safety win. Take the trade.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Dispatch enforcement breaks a real production call site that today relies on silent drop | High — would break the loop immediately | Low — today's call sites don't set `use_pty: true` for Grok (Grok is always-fallback so it never gets a Claude-only flag set) | Pre-PRD audit: grep every `dispatch(...)` call site for non-default capability-driven fields; assert they only target runners that support the field. The Phase 1 work already removed `cleanup_title_artifact`, the other Claude-only candidate. |
| The `non_exhaustive` annotation makes downstream matches harder to maintain | Low — only production impl-blocks match on the enum, and they're exhaustive by design (no wildcard arm) | Low | Document the convention in the trait rustdoc; reviewers reject wildcard arms in production runner impls. |
| Confusion between "capability not supported" (dispatch error) and "value invalid" (runner error) | Medium — operators may misdiagnose | Medium | The error message includes both `capability_name` and `field_name`; the runner's value-validation error includes the value. Different shapes, different stderr signatures. Document in `CLAUDE.md`. |
| Phase 4 author finds the variant set incomplete and has to extend the enum | Low — adding variants is non-breaking (`#[non_exhaustive]`) | Medium — forward-looking design is imperfect | Phase 4 PRD adds variants as needed; both production runners must update their exhaustive `supports` matches. No backwards-compat shim needed. |
| Opportunistic engine migration (FR-005) picks the wrong branch and changes observable behavior | Medium | Low — the PRD audits before migrating; tests cover pre/post | Each migration has a unit test asserting Claude and Grok both produce the same behavior post-migration as pre-migration. |
| Coordinated edits with the parallel coherence-refactoring `Engine Orchestration Boundaries` PRD cause merge conflicts | Medium — both touch `engine.rs` | Medium | Boundary contract (this section's last subsection); whichever PRD merges first leaves clear seams; the second rebases cleanly. |

### Security Considerations

- No new attack surface. The capability surface is internal — it doesn't expose new inputs.
- Dispatch enforcement is fail-closed (refuses on unsupported capability), which is the safe direction. The new error variant is a strict subset of "the call would have proceeded silently"; it cannot widen what runs.
- The `field_name` in the error message is `&'static str` (from the `enforce_capabilities` table), not user input, so no injection vector.

### Public Contracts

#### New Interfaces

| Module/Symbol | Signature | Returns | Side Effects |
| --- | --- | --- | --- |
| `RunnerCapability` | `pub(crate) enum RunnerCapability { ... }` with `#[non_exhaustive]` | N/A (type) | None |
| `LlmRunner::supports` | `fn supports(&self, cap: RunnerCapability) -> bool` | `bool` (default `false`) | None |
| `dispatch` (modified) | `pub fn dispatch(kind, prompt, perm, opts) -> TaskMgrResult<RunnerResult>` | `Ok(RunnerResult)` or `Err(UnsupportedRunnerCapability)` or whatever the runner returns | Validates capabilities before spawn; no new IO |
| `TaskMgrError::UnsupportedRunnerCapability` | `{ runner_kind: RunnerKind, capability_name: &'static str, field_name: &'static str }` | N/A (variant) | None |

#### Modified Interfaces

| Module/Symbol | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `LlmRunner` (trait) | one method (`spawn`) | two methods (`spawn`, `supports`) | No — `supports` has a default impl | None for downstream implementors; both production impls override |
| `dispatch` (function) | `pub fn dispatch(...) -> TaskMgrResult<RunnerResult>` | same shape; adds `enforce_capabilities` step before the match | No — signature unchanged | None |

### Data Flow Contracts

**N/A** — no cross-module data structure access patterns. `RunnerCapability` and `RunnerOpts` are consumed within `src/loop_engine/runner.rs` exclusively (plus tests). The error variant flows through `TaskMgrError` like any other variant.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `src/loop_engine/runner.rs:877` (`dispatch`) | All production spawn paths | NEEDS REVIEW — confirms no current caller sets a capability-driven field on a runner that doesn't support it | Pre-PRD grep audit (per the Risks table); existing tests + dogfood gate catch any regression |
| `src/loop_engine/engine.rs` (~15 `RunnerKind` match sites) | Engine dispatch + recovery branches | OK if KIND-CORRECT (no change); NEEDS REVIEW if CAPABILITY-MISLABELED | FR-005 audit; opportunistic migration with regression test |
| `tests/runner_cleanup.rs` (Phase 1 integration tests) | FakeRunner construction | NEEDS REVIEW — must add `supports_fn` field to constructor calls | FR-008; update existing FakeRunner test sites; default `supports_fn: |_| true` for tests that don't care |
| Any test file that constructs `RunnerOpts` directly | Direct field-set | OK — unchanged | None |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| `GrokRunner::spawn` `use_pty: _` destructure | Pre-PRD: silently drops the field | Field is dropped without warning; PTY behavior absent on Grok | Post-PRD: never reached because `dispatch` refuses earlier. The destructure line is removed (FR-007). |
| `engine.rs:5044` `if effective_runner != RunnerKind::Claude` | Pre-PRD: Grok-fallback hook only fires when current runner is Claude | Same intent: the hook promotes Claude → Grok; Grok → Grok is nonsense | Post-PRD: stays as `RunnerKind` match, annotated `// kind-correct: identity, not capability` |
| Dispatch with `RunnerOpts::default()` | Pre-PRD: succeeds for any runner | All capability fields at default = no opinion | Post-PRD: succeeds unchanged. Enforcement only fires on non-default capability-driven fields. |

### Inversion Checklist

- [ ] Every existing `RunnerKind::Claude` / `RunnerKind::Grok` match site audited and annotated KIND-CORRECT or CAPABILITY-MISLABELED?
- [ ] Every capability-driven `RunnerOpts` field has a corresponding `RunnerCapability` variant AND a row in the `enforce_capabilities` checks table?
- [ ] Both production runners' `supports` matches are exhaustive (compile error if a new variant is added)?
- [ ] `FakeRunner` extension preserves backward compatibility for existing Phase 1 tests (default `supports_fn` accepts everything)?
- [ ] Pre-PRD audit confirms no production call site today would be rejected by the new enforcement?
- [ ] Coordinated with parallel engine-carve PRD on touched lines in `engine.rs`?

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/loop_engine/CLAUDE.md` | Update | Add "Capability surface" subsection under "LLM runner dispatch" — explains `RunnerCapability` + `supports` + dispatch enforcement |
| `docs/designs/runner-trait-hygiene.md` | Update | Add Phase 2 retrospective appendix; check off Phase 2 in the roadmap |
| Rustdoc on `RunnerCapability`, `LlmRunner::supports`, `enforce_capabilities` | Create | Explain the surface, the fail-closed contract, the registry-table pattern |
| `CLAUDE.md` (project root) | No change | No public-CLI changes |
| `tasks/prd-01-runner-capability-enforcement.md` (this file) | N/A | Source of truth for the PRD |

---

## 7. Open Questions

- [ ] FR-005 audit result: does any `RunnerKind` match in `engine.rs` qualify as CAPABILITY-MISLABELED with a clean migration? Default: assume zero, ship the audit annotations + capability surface only. **Resolve during implementation by completing the audit; document findings.**
- [x] **Should `PermissionMode` be a `RunnerCapability` variant?** *(RESOLVED 2026-05-20, per architect review R4)*: **No.** Both runners always support it; making it a variant creates a half-state where the variant exists but never enforces anything. Drop from this PRD's variant set; reintroduce only when there is an enforced field.
- [x] **Should `SessionId` be a variant?** *(RESOLVED 2026-05-20, per architect review R5)*: **No.** It is consumed by `cleanup_session` (Phase 1 trait method), not by a `RunnerOpts` field. Same half-state risk as PermissionMode. Drop from this PRD; if a Phase 3/4 RunnerOpts field arises, add the variant then.
- [x] **Should `ThinkingTokens` be a variant?** *(RESOLVED 2026-05-20, per architect review R3)*: **No.** No `RunnerOpts` field encodes it today. Deferred to Phase 3 if/when a thinking-tokens field is added.
- [ ] Should `enforce_capabilities` use the static `checks` table (current design) or be auto-derived via a macro / proc-macro from `RunnerOpts` field attributes? **Default: static table.** For 4 enforced variants today, the static table is right. Phase 4's `RunnerArgs` builder may want a richer derive; revisit then.
- [ ] Does Phase 4 want `RunnerCapability` to be `pub` (re-exported) or `pub(crate)` (internal-only)? **Default: `pub(crate)`** — promote to `pub` in Phase 4 if needed. Cheaper to widen later than to shrink.
- [ ] Should the `non_exhaustive` annotation be at the enum or at variants? **Default: at the enum.** Resolve at code review.

---

## Appendix

### Related Documents

- `docs/designs/runner-trait-hygiene.md` — parent design document (Phase 2 section)
- `docs/designs/coherence-refactoring.md` — sibling effort; see "Boundary Contract with Runner Trait Hygiene Effort"
- `tasks/prd-runner-trait-hygiene.md` (Phase 1) — established the trait surface this PRD extends
- `tasks/prd-02-engine-orchestration-boundaries.md` — parallel coherence-refactoring effort; same `engine.rs` edit surface
- `src/loop_engine/CLAUDE.md` — subsystem design notes; "LLM runner dispatch" subsection updated by this PRD

### Boundary Contract with `Engine Orchestration Boundaries` PRD

Both PRDs touch `src/loop_engine/engine.rs`. The rules:

- **This PRD** touches at most one branch in `engine.rs` (the opportunistic FR-005 migration) plus comment annotations on the `~15` `RunnerKind` match sites. The annotations are one-line comments and create no new module boundaries.
- **`Engine Orchestration Boundaries` PRD** carves `engine.rs` into orchestrator/iteration/wave_scheduler/slot modules. The `RunnerKind` match sites move into the new modules but their semantics are byte-identical.
- **First-to-merge wins.** If this PRD merges first, the carve picks up the annotated sites and migrates them along with the carve. If the carve merges first, this PRD audits the sites in their new locations.
- **Coordination**: each PRD lists the other as a "review for overlap" stakeholder.
- **No new shared state.** This PRD introduces `RunnerCapability` and `supports`; the carve doesn't need to know about either (it's a structural refactor, not a semantic one).

### Glossary

- **Capability**: an LLM-runner feature the caller may want to use (PTY output, stream-json mode, effort levels, etc.). Different from "kind" (provider identity).
- **Kind**: which provider's CLI is invoked (`Claude` or `Grok` today). `RunnerKind` is the static-dispatch discriminant.
- **Dispatch boundary**: `runner.rs:877 dispatch(...)`. The single point every spawn flows through.
- **Fail-closed**: the safe direction for capability enforcement — refuse rather than proceed silently.
- **KIND-CORRECT** / **CAPABILITY-MISLABELED**: classification labels for `RunnerKind` match sites in the FR-005 audit.
