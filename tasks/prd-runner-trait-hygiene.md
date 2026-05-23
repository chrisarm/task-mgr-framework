# PRD: LlmRunner Trait Hygiene — Phase 1: cleanup_session + FakeRunner

**Type**: Refactor + Feature (introduces trait surface, removes opt-in field)
**Priority**: P2 (Medium) — addresses ongoing user-visible session-artifact leakage; not blocking
**Author**: Claude Code
**Created**: 2026-05-19
**Status**: Draft

> **Design context.** This PRD is Phase 1 of the five-phase roadmap documented in `docs/designs/runner-trait-hygiene.md`. The plan it implements is `~/.claude/plans/ultrathink-what-would-a-toasty-wozniak.md`. Both should be read alongside this PRD. Phases 2–5 (capability discovery, error taxonomy, args builder, RunnerSession RAII) are explicitly out of scope here.

---

## 1. Overview

### Problem Statement

The `LlmRunner` abstraction at `src/loop_engine/runner.rs:222` (introduced by the `feat-grok-fallback-runner` PRD) exposes a per-call opt-in flag `cleanup_title_artifact: bool` at `runner.rs:166` that controls a Claude Code 2.1.110 workaround. The flag defaults to `false`. Of 8 production spawn call sites, 5 had opted in; 3 (the main coding-iteration sites at `engine.rs:656`, `engine.rs:2587`, and `prd_reconcile.rs:672`) had never been touched. Over months of automated loop runs, **~4,500 orphan `<uuid>.jsonl` ai-title metadata stubs accumulated across `~/.claude/projects/*/`** — invisible until a user opened an interactive `claude` session and saw their resume picker drowning in 128-byte stubs.

A /spike on 2026-05-19 (`tasks/progress-5ba153a7.txt`) traced the same bug class one provider deeper. The `grok` CLI has no `--no-session-persistence` equivalent and writes a *directory* of artifacts per session at `~/.grok/sessions/<percent-encoded-cwd>/<uuid>/`. `GrokRunner::spawn` at `runner.rs:489` currently destructures `cleanup_title_artifact: _` and silently ignores it. As implemented today, every Grok-fallback iteration will leak a full session directory — strictly worse than Claude's single-file leak.

### Background

The bug class is: **wrapper-internal workaround for an upstream-CLI quirk leaking through the wrapper's API surface as an opt-in flag with an unsafe default.** Three principles are violated: side-effect ownership is in the wrong place (the runner creates the artifact but delegates cleanup to the caller); the unsafe default is the path of least resistance (`..Default::default()` makes adding a new site silently wrong); no production caller exercises both branches (the flag is dead config that masks a defect, plus it silently no-ops on Grok).

The fix is structural: cleanup belongs to the runner abstraction. The `LlmRunner` trait already exists. Adding one method (`cleanup_session`) and having the engine call it unconditionally post-spawn eliminates the bug class for both providers and prevents recurrence when a third provider is added. The `FakeRunner` seam introduced in this phase is the foundation the subsequent four phases (capability discovery, error taxonomy, args builder, RAII session tracking) will reuse for their integration tests.

Relevant prior learnings consulted (`task-mgr recall`):
- **[2847]** — deterministic UUID for safe cleanup in shared directories (the pattern this PRD generalizes from Claude-only to trait-level).
- **[1614]** — best-effort cleanup must not fail the parent operation (motivates non-fatal banner on `cleanup_session` errors).
- **[1626]** — *opt-in cleanup flag threaded through spawn_claude signature*: this PRD **supersedes this learning** as a pattern to avoid.
- **[1617]** — do NOT reuse `cleanup_ghost_sessions` for `~/.claude/projects/` cleanup (preserved invariant — that helper targets a different directory).
- **[2674]** — detached threads don't survive parent CLI exit; sync cleanup only (preserved invariant).
- **[2891]** — extract common subprocess scaffolding immediately when adding the second agent implementation (validated approach for the trait).
- **[2956]** — `RunnerKind` enum dispatch keeps allocation-free; no `Box<dyn LlmRunner>` on the hot path (preserved invariant).
- **[2939]** — multi-step visibility widening for internal refactoring (pattern applied to promoting `cleanup_title_artifact_sync` from `claude.rs` to `runner.rs`).
- **[2919]** — integration test mirrors unit test shape for consistency (guides `tests/runner_cleanup.rs` layout).

### Intended Outcome

After Phase 1 lands, neither provider can silently leak session artifacts. The `cleanup_title_artifact` field does not exist. Every spawn call site stops mentioning the flag. The dispatch boundary calls `cleanup_session` for every iteration, scoped to the exact (session_id, cwd) tuple created by that spawn. Failures emit a single banner line and continue.

---

## 2. Goals

### Primary Goals

- [ ] Add `fn cleanup_session(&self, session_id: Uuid, cwd: &Path) -> Result<(), TaskMgrError>` to the `LlmRunner` trait with a no-op default impl.
- [ ] Implement `ClaudeRunner::cleanup_session` correctly (delete `~/.claude/projects/<dash-encoded-cwd>/<uuid>.jsonl`; `NotFound` is silent success).
- [ ] Implement `GrokRunner::cleanup_session` correctly (delete `~/.grok/sessions/<percent-encoded-cwd>/<session-uuid>/` recursively; never touch the shared per-cwd `prompt_history.jsonl`).
- [ ] Remove `cleanup_title_artifact: bool` from `RunnerOpts` and all 8 production + 3 test call-site instances.
- [ ] `dispatch` calls `runner.cleanup_session(session_id, cwd)` unconditionally post-spawn. The session id flows via a new `RunnerResult.session_id: Option<Uuid>` field.
- [ ] Cleanup failures emit a single banner line `[cleanup warn] <provider>: <summary> (<path>)` and continue (non-fatal). `CLEANUP_WARN_ONCE` rate-limits to one banner per process.
- [ ] `FakeRunner` test seam under `#[cfg(test)]` supports configurable artifact-creation side-effects.
- [ ] Integration test at `tests/runner_cleanup.rs` proves artifact-present-post-spawn / artifact-absent-post-cleanup for both fake-Claude and fake-Grok artifact shapes.
- [ ] `WORKAROUND(claude-code-2.1.110-session-stub)` and `WORKAROUND(grok-cli-no-persistence-off)` comment markers exist exactly where each cleanup lives, so future upstream-fix removal is a one-grep operation.

### Success Metrics

- **Zero new leaks** (empirical, post-merge): a small synthetic loop run after this PRD lands produces zero new files in `~/.claude/projects/<encoded-cwd>/` and zero new directories in `~/.grok/sessions/<encoded-cwd>/` other than the per-cwd `prompt_history.jsonl` (whose growth is intentional).
- **Zero residual flag**: `grep -rn "cleanup_title_artifact" src/ tests/` → 0 hits after Phase 1.
- **Greppable workarounds**: `grep -rn "WORKAROUND(claude-code-2.1.110-session-stub)\|WORKAROUND(grok-cli-no-persistence-off)" src/` → 2 hits (one per provider; the cleanup site).
- **No regression**: existing `cargo test --lib loop_engine` + `cargo test --test runner_trait_dispatch --test grok_runner_unit --test grok_runner_integration` remain green.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **`NotFound` is silent success** in both providers' `cleanup_session`. The day Anthropic or xAI ships an upstream fix, the cleanup must degrade to a silent no-op, not log spam. Preserves the existing `cleanup_title_artifact_sync` behavior at `claude.rs:1162`.
- **Cleanup deletes ONLY the exact (session_id, cwd) tuple this spawn created.** Never enumerate-and-sweep. A wrong impl that sweeps the whole session dir would delete concurrent interactive sessions the user opened during a long loop run.
- **Grok's per-cwd shared `prompt_history.jsonl` is NEVER deleted.** It accumulates across sessions and survives loop runs by design. Cleanup is scoped to the per-session subdirectory only.
- **Encoding helpers are pure functions with round-trip tests** verifying they produce the actual observed on-disk paths. Claude uses `-`-encoded cwd (e.g. `-home-chris-...-mw-support`); Grok uses `%`-percent-encoded cwd (e.g. `%2Fhome%2Fchris%2F...%2Fmw-support`).
- **Cleanup runs synchronously after the child exits.** Per learning [2674], detached threads don't survive parent CLI exit. The cleanup is in the same call stack as `dispatch`.
- **Cleanup runs unconditionally for headless runs**, regardless of whether the runner-impl reported success or failure on `spawn`. A spawn failure may still have produced an artifact.

### Performance Requirements

- **Cleanup is sub-millisecond per iteration** (one `remove_file` for Claude; one `remove_dir_all` for Grok). No measurable regression vs the current code path.
- **Pre/post-spawn directory snapshot for Grok is a single `read_dir` of `~/.grok/sessions/<encoded-cwd>/`** — milliseconds even with hundreds of historical sessions. Done inside `GrokRunner::spawn`, before/after the child wait.
- **`CLEANUP_WARN_ONCE` swap is wait-free** (single `AtomicBool::swap` with `Relaxed` ordering, as today at `claude.rs:739+`).

### Style Requirements

- **No `.unwrap()` in any cleanup path.** Use `match` or `?` on `Result`. Errors are returned to the caller (`dispatch`) which handles the banner.
- **Comments explain WHY, never WHAT.** The two `WORKAROUND(...)` markers carry the explanation; the surrounding code is self-evident.
- **No detached threads, no `tokio::spawn`, no async cleanup.** Sync only.
- **Reuse `encoded_cwd_dir` at `claude.rs:51`** for the Claude artifact path; do not re-implement the dash-encoding.
- **Follow the existing pattern at `runner.rs:425, :599`** for `RunnerResult` field-by-name construction when adding `session_id`.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|---|---|---|
| `NotFound` on the artifact path (artifact never written, e.g. Claude finally honors `--no-session-persistence`) | Future-proofing; must not start log-spamming the day upstream fixes the bug | `cleanup_session` returns `Ok(())` silently; no banner emitted |
| Cleanup fails with `PermissionDenied` (e.g. misconfigured `~/.claude` mount, read-only FS) | Real environments have surprising FS permissions | First failure emits one banner line; `CLEANUP_WARN_ONCE` suppresses subsequent failures in the same process |
| Grok session-id discovery finds zero new entries (spawn failed before grok wrote anything) | Spawn errors precede artifact creation | `RunnerResult.session_id = None`; `dispatch` skips cleanup for this iteration |
| Grok session-id discovery finds two new entries (concurrent user opened interactive grok in same cwd) | Real concurrency hazard | Pick the entry with `mtime` closest to spawn-end (last-write-wins heuristic); document the heuristic on `GrokRunner::spawn` |
| `RunnerOpts.working_dir = None` | Some callers don't pin cwd; subprocess inherits parent cwd | `cleanup_session` uses `std::env::current_dir()` as fallback, mirroring `cleanup_title_artifact_sync` line 1154 today |
| Parallel slots running concurrently | Loop engine runs N slots in distinct worktrees | Distinct worktree cwds → distinct encoded session dirs → no cross-slot interference (verified by integration test) |
| `cleanup_title_artifact_sync` test fixtures using a fake `$HOME` | Existing tests at `claude.rs:3121, :3138` exercise this | Tests continue to pass after the helper is promoted to `runner.rs`; only import paths change |
| Worktree path with whitespace | Auto-review path-with-whitespace guard exists (`auto_review.rs`); cleanup path must be robust | Encoding produces a deterministic path regardless of whitespace; `remove_file` / `remove_dir_all` handle the literal path |

---

## 3. User Stories

### US-001: Operator stops accumulating resume-picker clutter

**As an** operator who uses `claude` interactively
**I want** automated loop runs to leave no orphan `<uuid>.jsonl` stubs in my resume picker
**So that** opening an interactive Claude session shows me my actual conversations, not 4,500 stubs of dead loop iterations.

**Acceptance Criteria:**
- [ ] After one full loop run, `find ~/.claude/projects/<encoded-current-cwd>/ -name '*.jsonl' -newer <run-start-marker>` returns 0 results.
- [ ] After one full loop run that uses Grok fallback, `find ~/.grok/sessions/<percent-encoded-current-cwd>/ -mindepth 1 -newer <run-start-marker>` returns only the `prompt_history.jsonl` (whose growth is intentional).

### US-002: Future provider integrator inherits the discipline

**As an** engineer adding a third LLM provider to task-mgr
**I want** the cleanup contract to be obvious from the trait definition
**So that** I don't have to discover the cleanup quirk by accident months later, or copy-paste a known-broken pattern from `GrokRunner::spawn`.

**Acceptance Criteria:**
- [ ] The `LlmRunner` trait's `cleanup_session` method has rustdoc documenting the contract (target identification, idempotency, `NotFound`-as-success, banner behavior).
- [ ] The default impl is no-op so a provider with no headless artifact (e.g. a future cloud API runner) requires zero cleanup code.
- [ ] Both existing impls (`ClaudeRunner`, `GrokRunner`) provide concrete reference implementations the new impl can model on.

### US-003: Maintainer ships an upstream-fix removal cleanly

**As a** maintainer when Anthropic or xAI fixes their session-persistence bug
**I want** to remove the workaround in one commit, in one place
**So that** I'm not chasing per-call-site flag removals or doing multi-file archaeology.

**Acceptance Criteria:**
- [ ] `grep -rn "WORKAROUND(claude-code-2.1.110-session-stub)" src/` returns exactly the lines that need to be deleted for the Claude fix.
- [ ] `grep -rn "WORKAROUND(grok-cli-no-persistence-off)" src/` returns exactly the lines that need to be deleted for the Grok fix.
- [ ] After workaround removal, the `cleanup_session` default no-op handles the "no artifact to clean" case automatically.

---

## 4. Functional Requirements

### FR-001: Add `cleanup_session` to `LlmRunner` trait

**Description.** Extend the trait at `src/loop_engine/runner.rs:222` with a `cleanup_session` method that has a no-op default impl.

**Signature:**
```rust
fn cleanup_session(
    &self,
    session_id: Uuid,
    cwd: &Path,
) -> Result<(), TaskMgrError> {
    Ok(())
}
```

**Validation.** A unit test in `runner.rs::tests` confirms the default impl returns `Ok(())` for a no-arg test runner that doesn't override the method.

### FR-002: Implement `ClaudeRunner::cleanup_session`

**Description.** Implement on `ClaudeRunner` at `runner.rs:242+`. Delete `~/.claude/projects/<dash-encoded-cwd>/<uuid>.jsonl`.

**Details:**
- Promote the `cleanup_title_artifact_sync` helper from `src/loop_engine/claude.rs:756` into `runner.rs` as a runner-private free fn. Rename to `cleanup_claude_session_artifact` (descriptive, not prefixed with the old field name).
- `ClaudeRunner::cleanup_session` calls the promoted helper. The body is one line.
- Preserve the `NotFound`-as-silent-success arm. Other IO errors return `Err(TaskMgrError::IoError(...))`.
- Place a `WORKAROUND(claude-code-2.1.110-session-stub)` comment marker on the helper.

**Validation.** Existing tests at `claude.rs:3087`, `:3121`, `:3138` (which exercise the helper directly) pass after import-path updates. New `tests/runner_cleanup.rs` integration test confirms artifact creation + removal end-to-end via `dispatch`.

### FR-003: Implement `GrokRunner::cleanup_session`

**Description.** Implement on `GrokRunner` at `runner.rs:471+`. Delete the session directory captured during spawn.

**Details:**
- Inside `GrokRunner::spawn`: snapshot `~/.grok/sessions/<percent-encoded-cwd>/` entry names (set of `Uuid` parses) before invoking the child. After child wait, list again; the new entry is this iteration's session id.
- Persist the captured `Uuid` via `RunnerResult.session_id: Some(uuid)` (see FR-006).
- `GrokRunner::cleanup_session(session_id, cwd)` calls `std::fs::remove_dir_all(grok_encoded_session_dir(cwd, home).join(session_id.to_string()))`.
- **Never** delete or modify `prompt_history.jsonl` at the cwd level.
- `NotFound`-as-silent-success. Other IO errors return `Err`.
- Add encoding helper `grok_encoded_session_dir(cwd: &Path, home: &Path) -> PathBuf` in `runner.rs` (NOT `claude.rs` — keep Grok logic out of the Claude module per spike decision). Encoding is `urlencoding::encode` of the absolute cwd; round-trip tests verify against the observed `%2F` separators.
- Place a `WORKAROUND(grok-cli-no-persistence-off)` comment marker on the cleanup site.

**Validation.** Round-trip unit tests on `grok_encoded_session_dir`. Integration test in `tests/runner_cleanup.rs` exercises the full dispatch flow.

### FR-004: Add `session_id: Option<Uuid>` to `RunnerResult`

**Description.** Extend `pub struct RunnerResult` at `runner.rs:102` with a new field.

**Details:**
- Position the field consistently with the existing field order (after `permission_denials` or before `timed_out` — implementer's call).
- `Option<Uuid>` so providers with no headless artifact return `None` and `dispatch` can skip cleanup.
- Update both production construction sites: `runner.rs:425` (`ClaudeRunner`) populates with the unconditionally-injected UUID; `runner.rs:599` (`GrokRunner`) populates with the captured session id from the pre/post-spawn dir diff.
- Update the test at `runner.rs:907` (`_assert_claude_result_is_runner_result`) for the new field.
- Search-and-fix: every field-by-name `RunnerResult { ... }` construction site outside of these (likely tests) gets `session_id: None` (or the actual id if applicable).

**Validation.** `cargo check` enforces — any missed construction site fails to compile.

### FR-005: Remove `cleanup_title_artifact: bool` from `RunnerOpts`

**Description.** Delete the field at `runner.rs:166`. Drop all 11 call-site instances.

**Details — production sites (8 instances of `cleanup_title_artifact: true`):**
- `src/loop_engine/engine.rs:656` (slot iteration — added by tactical patch)
- `src/loop_engine/engine.rs:2587` (sequential iteration — added by tactical patch)
- `src/loop_engine/prd_reconcile.rs:672` (added by tactical patch)
- `src/commands/curate/enrich.rs:247`
- `src/commands/curate/mod.rs:638`
- `src/loop_engine/progress.rs:328`
- `src/loop_engine/merge_resolver.rs:260`
- `src/learnings/ingestion/mod.rs:115`

**Details — test sites (3 instances in `tests/grok_runner_unit.rs`):**
- Lines 172–209 (test `grok_runner_silently_ignores_cleanup_title_artifact` and its setup).
- Line 280 (second test setup).
- Delete the rustdoc comments at lines 17, 22 that describe the silent-ignore contract.
- Replace with a single new test `grok_runner_cleanup_session_deletes_session_directory` that exercises the cleanup contract directly.

**Details — flag-conditional unit tests in `claude.rs` (under `#[cfg(test)]`):**
- Line 2948 `test_cleanup_title_artifact_false_omits_session_id`: **DELETE**. The asserted behavior (absence of `--session-id` when flag is false) ceases to exist.
- Line 2959 `test_cleanup_title_artifact_true_adds_valid_uuid_v4_session_id`: **RENAME** to `test_spawn_claude_always_injects_session_id`. Drop the flag from the body; the assertion that `--session-id <uuid-v4>` is present and positioned correctly still applies.
- Lines 3020, 3087, 3121, 3138: **KEEP**. Update imports to point at the promoted helper in `runner.rs`.

**Validation.** `grep -rn "cleanup_title_artifact" src/ tests/` returns 0 hits after Phase 1.

### FR-006: Wire `dispatch` to call `cleanup_session` unconditionally post-spawn

**Description.** Modify the `dispatch` free fn at `runner.rs:877` so that after `runner.spawn(...)` returns, it calls `runner.cleanup_session(session_id, cwd)` if `session_id.is_some()`.

**Details:**
- Source the `cwd` from `opts.working_dir` (the same value the runner used to launch the child). If `working_dir.is_none()`, use `std::env::current_dir()` (mirroring `cleanup_title_artifact_sync`'s fallback at `claude.rs:1154`).
- Cleanup runs regardless of `spawn` success/failure — a spawn that errors may still have produced an artifact.
- On `Err` from `cleanup_session`: emit a single banner line `[cleanup warn] <provider>: <summary> (<path>)` via `eprintln!`, gated by `CLEANUP_WARN_ONCE`. Then return the spawn's original result unchanged.
- The provider tag in the banner comes from `RunnerKind::Display` (or an inline match) so it reads `claude` or `grok`.
- Cleanup failure NEVER changes `dispatch`'s return value or exit code.

**Validation.** Integration test in `tests/runner_cleanup.rs` configures a `FakeRunner` to return a session_id pointing at a non-existent path (forced cleanup failure) and asserts the dispatch result is unchanged and the banner is emitted exactly once.

### FR-007: Lift `CLEANUP_WARN_ONCE` visibility for cross-provider sharing

**Description.** The static at `claude.rs:739` is currently file-private. Change visibility to `pub(crate)` (or move into `runner.rs` together with the promoted helper — implementer's choice; either works).

**Validation.** `grep -rn "CLEANUP_WARN_ONCE" src/` shows it referenced from `runner.rs` after the change.

### FR-008: Add `FakeRunner` under `#[cfg(test)]`

**Description.** In `runner.rs`, add a `#[cfg(test)]` module with a `FakeRunner` impl of `LlmRunner`.

**Details:**
- Configurable spawn output (`RunnerResult` fields settable per construction).
- Configurable artifact-creation side-effect on `spawn` (a closure or enum variant that writes a file at a configured path, simulating Claude's `<uuid>.jsonl` or Grok's directory).
- Recordable cleanup invocations (`Arc<Mutex<Vec<(Uuid, PathBuf)>>>` or similar) so tests can assert that `cleanup_session` was called with the expected arguments.

**Validation.** Used by `tests/runner_cleanup.rs` and by any future Phase 2–5 tests that need a controlled runner.

### FR-009: Integration test at `tests/runner_cleanup.rs`

**Description.** New file. Drives `dispatch` end-to-end with `FakeRunner` configured for each provider's artifact shape.

**Details:**
- Test shape mirrors `tests/runner_trait_dispatch.rs` per learning [2919]. Reuse the `CLAUDE_BINARY_MUTEX` pattern if any env-var mutation is needed (likely not, since `FakeRunner` bypasses real binaries).
- One test per provider shape: fake-Claude (single-file artifact) + fake-Grok (directory artifact). Each asserts: artifact-present-after-spawn, artifact-absent-after-dispatch returns.
- Plus one test for the cleanup-failure banner path: `FakeRunner` returns a session_id for which the artifact path doesn't exist (or simulate a permission error); assert dispatch returns Ok with the unchanged spawn result, banner emitted exactly once via `CLEANUP_WARN_ONCE`.
- Per learning [909], integration tests duplicate setup helpers rather than reaching into `pub(crate)`.

**Validation.** `cargo test --test runner_cleanup` green.

---

## 5. Non-Goals (Out of Scope)

The following are explicitly **NOT** part of this PRD:

- **Phases 2–5 of the hygiene roadmap** — capability discovery + enforcement, error taxonomy unification, `RunnerArgs` builder, `RunnerSession` RAII via `Drop`. Each gets its own plan + `/prd` run. See `docs/designs/runner-trait-hygiene.md` §"Proposed Refactorings."
- **One-shot sweep of pre-existing orphan artifacts** (~4,500 Claude stubs + any Grok session dirs accumulated before this PRD lands). Forward-looking only per user decision. Manual cleanup with `find ~/.claude/projects -maxdepth 2 -name '*.jsonl' -size -300c -delete` is available.
- **Any status / lifecycle / reconciliation logic added to the cleanup hook in `dispatch`.** The hook is and remains a thin, single-purpose provider-cleanup call. Task lifecycle centralization is the coherence-refactoring effort's job; this PRD is explicitly *not* allowed to grow that layer. See §"Boundary with Coherence Refactoring Effort" below and the reciprocal section in `docs/designs/runner-trait-hygiene.md:229`.
- **Carving or re-organizing `engine.rs` orchestration.** Phase 1 touches three engine lines (the field removal at `engine.rs:656` and `:2587`) and nothing else in that file. The coherence-refactoring Phase 1 owns the engine carve.
- **`grok sessions list --since` discovery approach.** Spike rejected in favor of pre/post directory diff (simpler, no CLI dependency, per-cwd concurrency-safe).
- **Production dry-run CLI mode using `FakeRunner`.** The seam is `#[cfg(test)]` only.
- **Refactoring other `RunnerOpts` fields** (`db_dir`, `signal_flag`, `timeout`) for similar smells. Those encode genuine per-caller policy and do not have the "always-true-in-production" shape.
- **A CI lint that detects new opt-in cleanup flags.** Removing the degree of freedom is the fix; codifying the footgun in lint config is the rejected alternative (see plan-mode round 2).
- **Generalizing `WORKAROUND(...)` as a project-wide convention.** Worth discussing later; not enforced in this PRD.

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/runner.rs` — primary file. Trait method add; two impls; field removal at line 166; field destructure update at line 489; `RunnerResult.session_id` add at line 102; `RunnerResult` field-by-name updates at lines 425 + 599 + 907; new `grok_encoded_session_dir` helper; promoted `cleanup_claude_session_artifact` helper; `FakeRunner` `#[cfg(test)]` impl; `dispatch` post-spawn hook at line 877.
- `src/loop_engine/claude.rs` — delete `cleanup_title_artifact_sync` at line 756 (promoted to `runner.rs`). Lift `CLEANUP_WARN_ONCE` visibility at line 739 to `pub(crate)` OR move with the helper. Delete the inline UUID injection `cleanup_session_id: Option<Uuid>` at `runner.rs:327` (NOT claude.rs — already lives in runner.rs per Grok PRD migration) and replace with an unconditional UUID generation. Delete the post-wait inline cleanup call at `runner.rs:422`.
- `src/loop_engine/engine.rs` — drop `cleanup_title_artifact: true` at lines 656 + 2587.
- `src/loop_engine/prd_reconcile.rs` — drop field at line 672.
- `src/commands/curate/enrich.rs:247`, `src/commands/curate/mod.rs:638`, `src/loop_engine/progress.rs:328`, `src/loop_engine/merge_resolver.rs:260`, `src/learnings/ingestion/mod.rs:115` — drop field at one site each.
- `tests/grok_runner_unit.rs` — delete 3 silent-ignore tests; add new `grok_runner_cleanup_session_deletes_session_directory`.
- `tests/runner_cleanup.rs` — **NEW** integration test.
- `src/loop_engine/claude.rs` (tests sub-module) — delete test at line 2948; rename + retarget test at line 2959; update imports at lines 3020, 3087, 3121, 3138.

### Dependencies

- External: none new. `urlencoding` crate is already in use elsewhere; reuse it for `grok_encoded_session_dir`.
- Internal: relies on the existing `LlmRunner` trait at `runner.rs:222`, `RunnerKind` enum at line 210, `dispatch` at line 877, `encoded_cwd_dir` at `claude.rs:51`, `cleanup_title_artifact_sync` at `claude.rs:756` (to be promoted), `CLEANUP_WARN_ONCE` at `claude.rs:739` (visibility to be lifted).

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| **A. Unconditional cleanup in `dispatch`, opt-in flag removed** | Eliminates the bug class structurally. Trait owns cleanup. Future providers inherit the discipline. Greppable workaround markers. | Touches every existing call site for field removal (mechanical). | **Preferred** (per plan-mode resolution and architect approval) |
| **B. Flip default to `true`, keep the flag** | Smaller diff. | Keeps dead config. Footgun survives for the next field with the same shape. Doesn't help Grok. | Rejected — half-measure |
| **C. Per-site opt-in stays; `GrokRunner` keeps silent no-op** | Zero-diff status quo. | Strictly worse Grok leak (directory vs file). Codifies the bug class. | Rejected — current state, what the PRD exists to fix |
| **D. Post-loop sweep of `~/.claude/projects/` and `~/.grok/sessions/`** | Provider-agnostic, no per-runner code. | Risks deleting interactive sessions the user opened during a long loop. Coarse. | Rejected — per-call deterministic cleanup is equally cheap and provably scoped |
| **E. RAII via `RunnerSession` newtype with `Drop`-fires-cleanup** | Eliminates "engine forgot to call cleanup" failure mode at the type level. | Larger Phase 1; introduces `Drop` semantics on a code path that already does substantial DB and stderr work around iteration boundaries; ordering with merge-back/overflow recovery is non-trivial. | **Deferred to Phase 5** (per plan-mode decision: explicit-call shape soaks first, RAII layered on top later) |

**Selected Approach**: A. Unconditional cleanup in `dispatch` with the opt-in flag removed.

**Phase 2 Foundation Check**: The explicit `dispatch`-site call is intentionally a near-term shape that will be superseded by RAII in Phase 5. The investment now lays the foundation for: (a) Phase 2 capability enforcement (the cleanup method is the first capability-aware trait method); (b) Phase 5 RAII (`RunnerSession` `Drop` impl will call the same `cleanup_session` method introduced here). Approach A costs ~0.5 day more than Approach B (per-site field removal across 11 sites), and avoids the rework cost of Approach B → A migration later (estimated 1+ day plus a second review cycle). The 1:2 ratio is below the strict 1:10 threshold but well above zero, and the bug class elimination is independently valuable.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| **Grok pre/post-spawn dir diff races with concurrent interactive `grok`** in the same cwd | Cleanup misses the right target OR deletes a user's interactive session | Med | Choose the new entry by `mtime` closest to spawn-end (last-write-wins). Parallel-slot integration test asserts disjoint cwds produce disjoint encoded session dirs. Document the heuristic on `GrokRunner::spawn`. |
| **`RunnerResult` field addition silently breaks exhaustive pattern matches** elsewhere in the codebase | Compile error in unexpected places; rebase pain | Low | `cargo check` enforces. The current code uses field-by-name construction at every site (verified by grep at `runner.rs:425, :599, :907`). |
| **Test deletions in `tests/grok_runner_unit.rs` hide future regressions** | Silent contract drift between providers; the deleted tests encode "Grok ignores the flag without panic" | Med | Replace with `grok_runner_cleanup_session_deletes_session_directory` test that exercises the NEW contract. Don't just delete; retarget. |
| **`cleanup_session` failures spam banner output during long loops** | Operator stderr drowns in `[cleanup warn]` lines | Low | `CLEANUP_WARN_ONCE` rate-limits to one banner per process, matching the existing pattern at `claude.rs:739+`. |
| **Cleanup runs after a spawn failure delete a partial artifact, masking diagnostic info** | Operator can't inspect a half-written Claude jsonl post-mortem | Low | Cleanup-failure banner gives the path, which an operator can use to inspect logs via `journalctl` or `tee` artifacts. Acceptable trade-off. |
| **Promoted helper's rename (`cleanup_title_artifact_sync` → `cleanup_claude_session_artifact`) breaks documentation or learnings references** | Stale cross-refs in `CLAUDE.md`, learnings, comments | Low | Grep for the old name across `src/`, `docs/`, `**/CLAUDE.md` and update. Add a one-line comment at the new helper noting the old name for git-archaeology. |
| **Engine-seam overlap with the in-flight coherence-refactoring effort** | Conflicting edits at `engine.rs:656` / `:2587` and the dispatch boundary cause merge churn or a second rewrite of the post-spawn hook | Med | The Boundary section above codifies the rules: cleanup hook stays single-purpose; engine field-removal edits leave clean seams (no adjacent tidying); `runner.rs` stays a peer not parent of `TaskLifecycle`. Cross-effort review-stakeholder listing on the PR. Shared transition shadow test harness (defined in the coherence pre-Phase 1 gates) verifies behavior parity. |

> **Top 3 by impact × likelihood:** the Grok race (#1), the engine-seam overlap with coherence-refactoring (#7 above), and the test-deletion regression (#3). None rate High × High — proceed, but the coherence-effort coordination requires active stakeholder review.

### Security Considerations

- **Path traversal not a concern**: cleanup targets are deterministic from (session_id, cwd, home). `session_id` is a `Uuid` (cannot contain `..`); `cwd` is an absolute path supplied by the operator or worktree-creation code. `encoded_cwd_dir` / `grok_encoded_session_dir` are pure functions over these inputs.
- **No new privileged operations**: cleanup is `remove_file` / `remove_dir_all` in the user's own home directory. The process already had write access by virtue of running there.
- **No new network calls**, no new env-var reads beyond `HOME`.

### Public Contracts

#### New Interfaces

| Module/Item | Signature | Returns (success) | Returns (error) | Side Effects |
|---|---|---|---|---|
| `LlmRunner::cleanup_session` | `fn cleanup_session(&self, session_id: Uuid, cwd: &Path) -> Result<(), TaskMgrError>` | `Ok(())` | `Err(TaskMgrError::IoError(...))` for non-`NotFound` IO errors | Deletes the provider-specific artifact for this (session_id, cwd) tuple |
| `runner::grok_encoded_session_dir` | `pub(crate) fn grok_encoded_session_dir(cwd: &Path, home: &Path) -> PathBuf` | `<home>/.grok/sessions/<percent-encoded-cwd>/` | N/A (pure fn) | None |
| `runner::cleanup_claude_session_artifact` | `fn cleanup_claude_session_artifact(session_id: Uuid, cwd: Option<&Path>) -> Result<(), TaskMgrError>` (private to `runner.rs`) | `Ok(())` on success or `NotFound` | `Err` for other IO errors | `remove_file` on the resolved path |
| `runner::FakeRunner` (`#[cfg(test)]`) | constructor takes spawn-output + artifact-creation closure + cleanup-recorder; impls `LlmRunner` | per construction | per construction | Per the configured closure |

#### Modified Interfaces

| Module/Item | Current Signature | Proposed Signature | Breaking? | Migration |
|---|---|---|---|---|
| `RunnerOpts` (`runner.rs:142`) | `{ ..., pub cleanup_title_artifact: bool, ... }` | `{ ... }` (field removed) | Yes (any caller that named the field breaks at compile) | Delete the line at every named call site (verified: 8 production + 3 test) |
| `RunnerResult` (`runner.rs:102`) | `{ exit_code, output, conversation, timed_out, completion_killed, permission_denials }` | Above + `session_id: Option<Uuid>` | Mostly no (field addition; field-by-name construction is the convention). Breaks if any code uses exhaustive struct pattern matching. | `cargo check` enforces; verified construction sites at `runner.rs:425, :599, :907` |
| `runner::dispatch` (`runner.rs:877`) | `pub fn dispatch(kind, prompt, permission_mode, opts) -> TaskMgrResult<RunnerResult>` | Same signature; new post-spawn behavior (calls `cleanup_session`) | Behavior change, signature stable | None at the call-site level; behavior tested via integration test |
| `cleanup_title_artifact_sync` (`claude.rs:756`) | `pub(crate) fn cleanup_title_artifact_sync(session_id: Uuid, working_dir: Option<&Path>)` | Promoted to `runner.rs` as private `cleanup_claude_session_artifact`; signature returns `Result<(), TaskMgrError>` instead of `()` | Yes (visibility + return type) | Only caller was `runner.rs:422` (inline). New caller is `ClaudeRunner::cleanup_session`. |
| `CLEANUP_WARN_ONCE` (`claude.rs:739`) | `static CLEANUP_WARN_ONCE: AtomicBool` (file-private) | `pub(crate) static CLEANUP_WARN_ONCE: AtomicBool` | Visibility widening only | Existing callers continue to work; new caller in `runner.rs` |

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
|---|---|---|
| **Session id from spawn → cleanup (Claude)** | `Uuid` (typed) → `RunnerResult.session_id: Option<Uuid>` → `dispatch` reads as `Option<Uuid>` → unwrap and pass to `cleanup_session` | `let result = runner.spawn(prompt, perm, opts)?;`<br>`if let Some(sid) = result.session_id {`<br>&nbsp;&nbsp;`runner.cleanup_session(sid, cwd).unwrap_or_else(|e| warn_once_banner("claude", e, path));`<br>`}` |
| **Session id from spawn → cleanup (Grok)** | Pre-spawn: `HashSet<String>` of entry names in `~/.grok/sessions/<encoded>/` → diff produces `String` → parse `Uuid::parse_str(&new_entry)?` → `Option<Uuid>` on `RunnerResult` → same dispatch path | `let before: HashSet<String> = read_dir(&grok_dir)?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();`<br>`// ... spawn child, wait ...`<br>`let after: HashSet<String> = read_dir(&grok_dir)?.filter_map(...).collect();`<br>`let new_id = after.difference(&before).filter_map(|s| Uuid::parse_str(s).ok()).next();`<br>`Ok(RunnerResult { session_id: new_id, ... })` |
| **cwd from opts → cleanup target path** | `opts.working_dir: Option<&Path>` → fallback to `std::env::current_dir()?` if `None` → passed as `&Path` to `cleanup_session` → joined with `encoded_cwd_dir` or `grok_encoded_session_dir` to produce the final `PathBuf` | `let cwd: PathBuf = opts.working_dir.map(|p| p.to_path_buf()).or_else(\|\| std::env::current_dir().ok()).ok_or(...)?;`<br>`let target = encoded_cwd_dir(&cwd, &home).join(format!("{}.jsonl", session_id));` |
| **`HOME` for path construction** | `std::env::var("HOME")` → `String` → `PathBuf::from` → passed to `encoded_cwd_dir(cwd, home)` | `let home = std::env::var("HOME").map(PathBuf::from).ok_or(...)?;` |

**Key type transitions to verify:**
- `String` (from `read_dir` entry name) → `Uuid` (via `Uuid::parse_str`). Grok session dirs are named with their Uuid; the parse must handle malformed entries (skip them).
- `Option<Uuid>` on `RunnerResult` → `Uuid` at the cleanup-call site. Use `if let Some(...)`; never `.unwrap()` (per Style Requirements).

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|---|---|---|---|
| `src/loop_engine/engine.rs:656` | Sets `cleanup_title_artifact: true` | BREAKS | Delete the line |
| `src/loop_engine/engine.rs:2587` | Sets `cleanup_title_artifact: true` | BREAKS | Delete the line |
| `src/loop_engine/prd_reconcile.rs:672` | Sets `cleanup_title_artifact: true` | BREAKS | Delete the line |
| `src/commands/curate/enrich.rs:247` | Sets `cleanup_title_artifact: true` | BREAKS | Delete the line |
| `src/commands/curate/mod.rs:638` | Sets `cleanup_title_artifact: true` | BREAKS | Delete the line |
| `src/loop_engine/progress.rs:328` | Sets `cleanup_title_artifact: true` | BREAKS | Delete the line |
| `src/loop_engine/merge_resolver.rs:260` | Sets `cleanup_title_artifact: true` | BREAKS | Delete the line |
| `src/learnings/ingestion/mod.rs:115` | Sets `cleanup_title_artifact: true` | BREAKS | Delete the line |
| `tests/grok_runner_unit.rs:182, 192, 280` | Tests silent-ignore contract | BREAKS | Delete + retarget |
| `src/loop_engine/claude.rs:2948` | Tests false-omits-session-id | BREAKS | Delete test |
| `src/loop_engine/claude.rs:2959` | Tests true-injects-session-id | NEEDS REVIEW | Rename to `test_spawn_claude_always_injects_session_id`; drop the field; the unconditional injection assertion stands |
| `src/loop_engine/claude.rs:3020` | `test_cleanup_does_not_touch_unrelated_jsonl_files` | OK | Update import path; helper now in `runner.rs` |
| `src/loop_engine/claude.rs:3087, :3121, :3138` | Helper tests | OK | Update import paths |
| `src/loop_engine/runner.rs:425` | `RunnerResult` field-by-name construction (Claude) | NEEDS REVIEW | Add `session_id: Some(cleanup_session_id)` |
| `src/loop_engine/runner.rs:599` | `RunnerResult` field-by-name construction (Grok) | NEEDS REVIEW | Add `session_id: captured_session_id` |
| `src/loop_engine/runner.rs:907` | Test alias assertion | NEEDS REVIEW | Add `session_id: None` |
| `src/loop_engine/runner.rs:327` | Inline `cleanup_session_id: Option<Uuid>` (currently lives in `runner.rs`, NOT `claude.rs` — per architect review) | BREAKS the conditional gate | Remove the `.then(...)` gate; generate UUID unconditionally |
| `src/loop_engine/runner.rs:422` | Inline `cleanup_title_artifact_sync` call | BREAKS | Delete the call; cleanup moves to `dispatch` |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
|---|---|---|---|
| `cleanup_ghost_sessions` (`claude.rs:1101`) | Cleans legacy `~/.claude/sessions/` interactive-classifier stubs | Sweeps tiny files <300 bytes from `~/.claude/sessions/` | **UNCHANGED** — different directory, different bug class, different cleanup mechanism. Per learning [1617], do NOT conflate. |
| `ClaudeRunner::cleanup_session` (NEW) | Cleans `~/.claude/projects/<encoded>/` ai-title metadata stubs per spawn | N/A (didn't exist) | Deterministic UUID-based deletion of exactly one file per spawn |
| `GrokRunner::cleanup_session` (NEW) | Cleans `~/.grok/sessions/<encoded>/<uuid>/` per spawn | N/A (didn't exist) | Recursive directory deletion of exactly one dir per spawn; per-cwd `prompt_history.jsonl` is excluded |

### Inversion Checklist

- [x] **All callers identified and checked?** Yes — 8 production + 3 test sites for the field; 6 test sites for the helper. Audit complete via architect review.
- [x] **Routing/branching decisions that depend on output reviewed?** Yes — `dispatch` is the only routing site; updated to post-call cleanup.
- [x] **Tests that validate current behavior identified?** Yes — `claude.rs:2948–3138`, `grok_runner_unit.rs:172–280`.
- [x] **Different semantic contexts for same code discovered and documented?** Yes — `cleanup_ghost_sessions` vs `cleanup_session`. Different.
- [x] **What happens if `cleanup_session` is called twice with the same `session_id`?** Idempotent — second call gets `NotFound`, which is silent success.
- [x] **What happens if `cleanup_session` is called before `spawn`?** Impossible from `dispatch` (sequenced). From a test: the artifact doesn't exist, `NotFound` is silent success.

### Boundary with Coherence Refactoring Effort

This PRD runs in parallel with the broader Coherence Refactoring design at `docs/designs/coherence-refactoring.md`. The two efforts target different primary concerns (provider-abstraction hygiene vs. task-lifecycle and orchestration ownership) but share the same high-risk edit surface: the iteration spawn + immediate post-processing window inside `src/loop_engine/engine.rs` (both `run_iteration` and the slot/wave paths). The full coordination contract lives in the §"Boundary Contract with Runner Trait Hygiene Effort" section of the coherence document; the reciprocal pointer is at `docs/designs/runner-trait-hygiene.md:229`. The practical rules for *this* PRD are:

| Rule | Applies to | Why |
|---|---|---|
| **`cleanup_session` hook stays single-purpose.** No status, lifecycle, reconciliation, or `<task-status>` logic is added inside `dispatch` post-spawn. | FR-006 implementation | Task lifecycle is the coherence effort's owned layer. Mixing concerns at the dispatch boundary would force a second rewrite. |
| **Engine field-removal edits leave clean seams.** At `engine.rs:656` and `:2587`, drop only the `cleanup_title_artifact: true` line; do NOT also tidy up adjacent code, refactor `SpawnOpts`/`RunnerOpts` construction, or extract helpers. | FR-005 implementation | The coherence Phase 1 will carve `engine.rs` along seams it has already mapped. Adjacent cleanups would conflict with that work and re-do it. |
| **`runner.rs` stays a peer of the future `TaskLifecycle` module, not a parent.** No status, reconciliation, or `run_tasks` bookkeeping seeps into the runner trait surface. | FR-001 + FR-006 trait/dispatch design | The coherence Phase 1 treats `runner.rs` as an established peer. Growing it sideways defeats that.|
| **`<task-status>` side-band tag contract and per-task partial-failure tolerance are preserved bit-identically.** The runner change does not alter how the engine consumes or aggregates per-task outcomes. | All FRs | These are inputs to TaskLifecycle centralization; the transition shadow test harness defined in the coherence pre-Phase 1 gates will be the cross-effort verification mechanism. |
| **Stakeholder review on overlap.** When this PRD reaches code review, the coherence-refactoring effort is listed as a review-for-overlap stakeholder, and vice-versa. | PR description / code review | Catches accidental dual-edits at the engine seam early. |

If during implementation a tension surfaces between these rules and a Phase 1 acceptance criterion, raise it via Open Question rather than resolving unilaterally — both efforts have stakeholders.

### Documentation

| Doc | Action | Description |
|---|---|---|
| `docs/designs/runner-trait-hygiene.md` | Already created | Parent design doc for the 5-phase roadmap (just written). |
| `src/loop_engine/CLAUDE.md` | Update | Add a brief subsection on the `cleanup_session` contract under the existing runner-related notes. Keep it under 10 lines; the design doc has the depth. |
| `src/loop_engine/runner.rs` (rustdoc) | Update | The `LlmRunner` trait rustdoc gains a paragraph on `cleanup_session`. Both impls get a `WORKAROUND(...)` comment marker. |
| `tasks/grok-fallback-runner.json` (`tasks/prd-grok-fallback-runner.md:284`) | Update during this PRD | Drop the "Grok runner silently ignores the flag" line; replace with a pointer to this PRD's contract. |
| `~/.claude/plans/ultrathink-what-would-a-toasty-wozniak.md` | Already updated | Plan file — the source of this PRD. |
| `task-mgr learn` | After Phase 1 lands | Record a learning that supersedes [1626] ("opt-in cleanup flag" pattern) — the corrected pattern is "cleanup as a trait method, called unconditionally by dispatch." |

---

## 7. Open Questions

- [ ] **GrokRunner concurrency under parallel slots:** the pre/post-spawn dir-diff happens inside `GrokRunner::spawn`. Confirmed by construction (per-slot worktree → per-slot encoded session dir), but a parallel-slot integration test should exercise it explicitly. Decide during implementation whether this lives in `tests/runner_cleanup.rs` (preferred) or `tests/grok_runner_integration.rs`.
- [ ] **`CLEANUP_WARN_ONCE` location:** `pub(crate)` widening (no relocation) vs moving into `runner.rs` alongside the promoted helper. Either works; implementer's choice based on the diff that's cleaner to review. Both providers must share a single warn-once gate.
- [ ] **Tactical patch on `main` removal sequencing:** the 3 sites in `engine.rs:656/2587` + `prd_reconcile.rs:672` carrying the tactical `cleanup_title_artifact: true` are removed as part of FR-005. Confirm during implementation that the removal commit is bundled with the field-removal commit (single bisectable point) rather than a separate "undo the patch" commit.
- [ ] **Coherence-refactoring stakeholder review timing:** when does the cross-effort review hand-off happen — at first draft PR, at "ready for review," or only on conflict? Decide before opening the PR so the coherence-effort maintainer isn't surprised. The Boundary section captures the rules; the *cadence* of cross-effort visibility is still TBD.
- [ ] **Shared transition shadow test harness availability:** the coherence pre-Phase 1 gates define a transition shadow test harness as the cross-effort verification mechanism. Confirm during implementation whether that harness is already landed and usable, or whether Phase 1 of this effort needs to defer the harness-based parity test to a follow-up.
- [x] **`grok_encoded_session_dir` placement** — RESOLVED (lives in `runner.rs`, not `claude.rs`).
- [x] **`RunnerResult` shape change** — RESOLVED (add `session_id: Option<Uuid>`, non-breaking for field-by-name).
- [x] **Phase 1 / tactical-patch relationship** — RESOLVED (no tactical fix survives; full long-term fix).

---

## Appendix

### Related Documents

- **Plan file**: `~/.claude/plans/ultrathink-what-would-a-toasty-wozniak.md` — clarifying-question record + final scope.
- **Design doc**: `docs/designs/runner-trait-hygiene.md` — 5-phase roadmap; goals; invariants; risks; §"Boundary Contract with Coherence Refactoring Effort" (line 229).
- **Parallel design doc**: `docs/designs/coherence-refactoring.md` — broader TaskLifecycle + engine-carve effort. Reciprocal §"Boundary Contract with Runner Trait Hygiene Effort" defines the coordination rules and shared test infrastructure (transition shadow test harness).
- **Spike record**: `tasks/progress-5ba153a7.txt` — 2026-05-19 /spike output that named the cross-provider bug class.
- **In-flight predecessor**: `tasks/prd-grok-fallback-runner.md` — introduced `LlmRunner` trait and `RunnerKind` enum; this PRD extends the trait surface.

### Glossary

- **Session artifact**: A file or directory the LLM CLI writes during a headless (`-p`) run for resume/title metadata purposes. For Claude 2.1.110: a single `<uuid>.jsonl` in `~/.claude/projects/<encoded-cwd>/`. For Grok: a directory `<uuid>/` in `~/.grok/sessions/<encoded-cwd>/` containing 4–5 JSON files.
- **Encoded cwd**: The on-disk directory name corresponding to a working directory. Claude encodes with `/` → `-` (e.g. `-home-chris-task-mgr`). Grok URL-encodes (e.g. `%2Fhome%2Fchris%2Ftask-mgr`).
- **Headless run**: An LLM CLI invocation with `-p PROMPT` (single-turn, non-interactive) — the only mode task-mgr uses.
- **Session id**: A `Uuid` identifying a session. For Claude, injected by us via `--session-id`. For Grok, assigned by the CLI and discovered via pre/post-spawn dir diff.
- **`WORKAROUND(...)` marker**: A comment of shape `// WORKAROUND(<provider>-<short-issue-name>):` placed adjacent to upstream-CLI workaround code so the future removal is grep-able in one shot.

---

**Next step**: Run `/tasks tasks/prd-runner-trait-hygiene.md` to generate the task breakdown.
