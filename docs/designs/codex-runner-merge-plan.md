# Codex Runner — Consolidated Merge Plan

Synthesizes 4 independent reviews of `feat/codex-runner` (V1) vs
`feat/codex-runner-support-v2` (V2). Status: **plan for approval**, not yet executed.

## Decision (confirmed)

- **Deliverable:** plan first → execute on approval.
- **Base branch:** **V2** (`feat/codex-runner-support-v2`) — lean, 1 commit, reviewable.
  Port V1's hardened safety pieces onto it.
- **models-routing-config:** **split into its own PR/branch** — it is a separate
  feature (~half of V1's diff) and must not ride in on Codex support.

### Why V2 base (not trimmed V1)
All 4 reviews agree the *correct internals* live in V1 and the *correct scope/config
shape* lives in V2. The two routes converge on the same end state, but V2-base is far
cleaner git-wise: V2 is one squashed commit vs. disentangling V1's ~40 loop commits +
a second bundled PRD. Review 1 preferred trimming V1; reviews 2/3/4 preferred V2-base.
We follow the majority because the disentangling cost is the deciding practical factor.

### Facts (verified)
- V1: 130 files, +11,243 / −1,822 (src+tests). Bundles codex + models-routing-config.
- V2: 43 files, +2,586 / −261 (src+tests). Codex only, 1 commit.

## End state
Codex as a third `RunnerKind` (config-only routing via `primaryRunner` +
`provider:"codex"`, never inferred from model strings), with:
- V2's **provider-only** config (blank model allowed for Codex) — KEEP.
- V1's **hardened `protected_state.rs`** — PORT.
- V1's **stdin writer thread** — PORT.
- V1's **structured auth detection** (`[Error:]`-only) — PORT.
- V1's **batch-run preflight parity** — PORT.
- V1's **invariant scanners + recovery/hint tests** — PORT (+ adapt to V2 source).
- A **stub-binary stream e2e** that *both* branches lack — ADD.
- models-routing-config — OUT (separate branch).

---

## Branch / commit sequence

Work branch: `feat/codex-runner-merged` cut from `feat/codex-runner-support-v2`.

```
git worktree add ../task-mgr-worktrees/feat-codex-runner-merged -b feat/codex-runner-merged feat/codex-runner-support-v2
```

Commit plan (one logical change each, so the reviewer sees exactly what changed):

1. `port: hardened protected_state.rs from V1 (integrity SQLite + symlink defense)`
2. `fix(codex): writer-thread stdin to avoid large-prompt deadlock`
3. `fix(codex): structured auth detection — match [Error:] lines only`
4. `fix(codex): batch-run preflight + binary probe parity`
5. `port(tests): codex recovery + provider-hint threading + invariant scanners`
6. `test(codex): stub-binary stream e2e (CodexStreamFormat against --json)`
7. `chore: protected_state call-site + config adaptation glue` (folded into 1/4 if small)

Each commit must compile + pass scoped tests before the next.

---

## File-by-file port list

Source line refs are from the **review text** (V1 = feat-codex-runner worktree).
Confirm exact lines at execution time (`git grep`), since they drift.

### 1. `src/loop_engine/protected_state.rs` — REPLACE V2 with V1 (reviews 1,2,3,4)
- **Action:** copy V1's hardened module over V2's 181-line version.
- **V1 source:** `protected_state.rs:18` (SQLite integrity), `:244` (symlink/inode
  removal + parent containment). V1 ≈1,404 lines.
- **V2 removed:** byte-snapshot of `tasks.db`/`-wal`/`-shm` (`protected_state.rs:31`)
  and `fs::write(path, bytes)` symlink-unsafe restore (`:82`).
- **Why:** (a) byte-compare of WAL/SHM false-halts on *legitimate* `task-mgr
  show/list/recall` DB access by a Codex agent (review 3 — the headline correctness
  bug); use `PRAGMA quick_check` + `schema_version` regression on a **read-only**
  handle, fatal-on-corruption, no byte-restore of a live WAL. (b) symlink-swap can
  redirect restore writes outside the task tree (review 1) — detect inode/symlink
  change, remove symlink before restore, canonicalize+confine parent to `tasks_dir`.
- **Adaptation (precise — confirmed via diff):** V1 API is `Snapshot::take(db_dir,
  tasks_dir, kind) -> Option`, `take_unconditional(...)`, `verify_and_restore() ->
  VerifyOutcome{Clean|Reverted|FatalSqliteCorruption}`, plus
  `runner_requires_state_guard(kind)->bool`. V2 API is
  `ProtectedTaskStateSnapshot::capture(db_dir) -> TaskMgrResult`,
  `verify_and_restore_text() -> TaskMgrResult<()>`, no gate fn. **V2 has THREE call
  sites** (V1 had two): `iteration.rs:493/554`, `wave_scheduler.rs:1190/1218`,
  **`slot.rs:197/257`** (V1 lacks slot.rs — slot is V2's parallel path; the ported V1
  module must serve it too). Two viable adaptations:
  - (a) Port V1 module verbatim and adapt 3 call sites to the `take`/`VerifyOutcome`
    API (richer operator visibility — Reverted lists symlink swaps). Preferred.
  - (b) Keep V2's `capture`/`verify_and_restore_text` *signatures* but swap the bodies
    for V1's integrity+symlink internals (smaller call-site churn). Acceptable if (a)
    balloons.
  Decide at execution based on which keeps the diff cleaner. `tasks_dir` is a new
  required input vs V2's `capture(db_dir)` — thread it through at each call site.
- **No Cargo.toml change:** V1 uses `rusqlite::{Connection, OpenFlags}` + `MetadataExt`
  (std). Both already available in V2; only imports change.

### 2. `src/loop_engine/runner.rs` — Codex stdin writer thread (review 1)
- **Action:** replace V2's synchronous stdin write (V2 `runner.rs:1090`, writes before
  draining stdout) with V1's writer-thread pattern (V1 `runner.rs:1157`): spawn a
  thread that writes the prompt to child stdin and `drop`s it, while the parent enters
  `drive_stream` to drain stdout concurrently.
- **Why:** large prompts exceeding the pipe buffer deadlock — parent blocks on write
  while Codex blocks on output.
- **Scope guard:** touch only the Codex spawn path; do not refactor the Claude/Grok
  paths.

### 3. `src/loop_engine/runner.rs` (+ `stream.rs`) — structured auth detection (review 3)
- **Action:** replace V2 `contains_codex_auth_failure` (substring scan over
  `format!("{stderr}\n{conversation}")`) with V1
  `codex_conversation_indicates_auth_failure`, which matches auth markers ONLY on
  `[Error: …]` lines emitted from `type:"error"`/`type:"turn.failed"` events. Scanning
  stderr for auth strings is fine; scanning the assistant transcript is the bug.
- **Why:** a task *about* auth (output quoting "401 unauthorized") gets misclassified as
  a Codex auth crash → `crash_counts_as_task_failure == false` → silently doesn't count
  toward the consecutive-failure ladder = failure-masking.
- **Bring V1's negative-control test** (AC #8: a model reply quoting "HTTP 401" must NOT
  trip detection) — see test port §5.

### 4. Batch preflight parity (reviews 1, 3)
- **Action:** route `batch run` through the same `preflight_validate_and_probe`
  (V1 `project_config.rs:648`) that `loop run` uses, so the Codex binary probe fires on
  the batch path too. V2 `batch.rs:503` only reads project config.
- **Startup probe:** keep it **route-gated** — only fires when a `primaryRunner` spec
  names `provider:"codex"` (no PATH probe for pure-Claude projects). V1 encapsulates in
  `project_config.rs` (`check_codex_primary_binary`); V2 wires `check_codex_runner_binary`
  via `main.rs`. Either placement is fine; pick one and ensure **both loop+batch** hit it.

### KEEP from V2 (do NOT overwrite with V1)
- **Provider-only Codex config** — `{ "provider":"codex" }` with blank/absent model is
  valid (V2 `project_config.rs:468`). V1 wrongly makes `RunnerSpec.model` mandatory
  (V1 `:683`/`:683`). Non-Codex routes still require a model.
- Consider porting V1's **strict provider parser** (`parse_config_provider` →
  `Result`, rejects "openai"/"codex-cli"/"groq" typos) over V2's `Option`-returns-None
  parser (silent fall-through to Claude). Reviews 1 & 3 both flag V2's lenient parser as
  a typo footgun. **Decision needed at execution** — recommend porting strict parser
  (small, high value), but it interacts with provider-only routing so test carefully.

### resolve_effective_runner ergonomics (review 2) — DECISION
- V2 uses `impl Into<EffectiveRunnerInput>` + **unconditional** `From<Option<&str>>`,
  so a bare `Some("model")` silently sets `provider_hint:None` (a Codex→Claude misroute
  vector). V1 gates `From<Option<&str>>` behind `#[cfg(test)]` and adds the
  `no_bare_option_resolve_effective_runner.rs` textual scanner.
- **Recommended:** adopt V1's posture — gate the bare conversion to `#[cfg(test)]`, make
  production call sites pass explicit `EffectiveRunnerInput { model, provider_hint }`,
  and port the scanner. This is required for the scanner test to pass anyway (see §5).

---

## 5. Tests to port / adapt

Port from V1 (`tests/`):
- `codex_recovery.rs` (~496 lines) — Codex RuntimeError does NOT promote to Grok;
  `Ok((None,None))` escalation; fails task rather than mis-escalating.
- `codex_provider_hint_threading.rs` (~265) — hint survives resolution; cleared by a
  `reviewModel` override.
- `codex_runner_overrides_invariant.rs` (~91) — greps `src/` to ensure no production
  code inserts `RunnerKind::Codex` into `runner_overrides` (recovery channel must never
  carry Codex, bypassing the route-gated probe).
- `no_bare_option_resolve_effective_runner.rs` (~411) — textual scanner enforcing
  explicit `EffectiveRunnerInput`; asserts a minimum call-site count.
- Negative-control auth test (in the recovery/auth test file).

**Scanner adaptation risk (review 2/4):** both scanners encode assumptions about V2's
*source shape*. Against V2 as-is:
- `codex_runner_overrides_invariant.rs` — should PASS if V2 never inserts Codex into
  overrides (V2 keeps Codex out of the fallback ladder; verify).
- `no_bare_option_resolve_effective_runner.rs` — will **FAIL** against V2 until we adopt
  V1's explicit-input posture and fix call sites (see resolve_effective_runner decision).
  This is the coupling that forces the ergonomics change above.

Keep V2's existing additions (they're fine): `provider_routing.rs`,
`runner_capability_contract.rs`, `fallback_config.rs`.

## 6. New test — stub-binary stream e2e (the gap BOTH branches share, reviews 3,4)
- Add a fake `codex` binary (echo-script harness — reuse the existing grok/claude
  echo-script test harness pattern) emitting realistic `--json` JSONL
  (`item.started`/`item.completed`/`turn.failed`) and assert `CodexStreamFormat`
  extracts the final `agent_message`. Guards against a Codex CLI schema bump silently
  breaking output extraction (no version negotiation in the parser).

---

## models-routing-config split (out of this PR)
V1-only files to EXCLUDE from the Codex branch and move to a separate
`feat/models-routing-config` branch (enumerate precisely at execution via
`git diff main...feat/codex-runner --stat -- src`):
- `src/commands/models/handlers.rs` (+733), `src/config_io.rs` (+86),
  `src/cli/user_config.rs` (+247), `cli/commands.rs` models subcommands (+72),
  `model.rs` review-model / set-fallback / routing-table CLI additions,
  `.task-mgr/tasks/models-routing-config*.{json,md}`.
- The two new task files already in the main repo working tree
  (`models-routing-config.json` + `-prompt.md`) belong to that separate effort.

---

## Verification gate (per CLAUDE.md §3 — tee + grep, one shot)
After each commit, in the merged worktree:
```bash
cargo build 2>&1 | tee /tmp/build.txt | tail -3 && grep -E "^error" /tmp/build.txt | head
cargo test codex 2>&1 | tee /tmp/t.txt | tail -5 && grep -E "FAILED|error\[" /tmp/t.txt | head
cargo clippy -- -D warnings 2>&1 | tee /tmp/c.txt | tail -3 && grep "^error" /tmp/c.txt | head
```
Final gate: full `cargo test` green + `cargo clippy -D warnings` clean.

## Resolved decisions (confirmed by Chris, 2026-05-31)
1. Strict provider parser (port from V1) — **YES**. Port `parse_config_provider`
   (`Result`, rejects unknowns) over V2's `parse_runner_provider` (`Option`→Claude).
2. resolve_effective_runner explicit-input posture (#[cfg(test)] gate + explicit
   call sites) — **YES** (required by the scanner test). V2's `engine.rs` currently
   has an UNGATED `From<Option<&str>>`; gate it and fix any bare-Option production
   call site (suspect: `reactions/pre_spawn.rs:125` — verify at execution).
3. Cross-provider fallback for Codex (Codex→Claude) — **WIRE IT NOW** (new design;
   see "Codex→Claude fallback design" below). Neither V1 nor V2 implemented this.

## Codex→Claude fallback design (NEW — neither branch has it)
Today both branches: a Codex non-auth crash → `recovery.rs` returns `Ok((None,None))`
→ no escalation → task auto-blocks (Codex is deliberately kept OUT of the overflow
ladder + out of `runner_overrides`). The `codex_runner_overrides_invariant.rs` scanner
enforces "never insert `RunnerKind::Codex` into runner_overrides."

Proposed wiring (compatible with that invariant — we insert **Claude**, not Codex):
- **Trigger:** at the recovery site that currently returns `Ok((None,None))` for a
  Codex runtime failure, instead insert a `runner_overrides[task_id] = Claude`
  promotion (re-run the task on Claude) — bounded to ONE promotion per task to avoid
  ping-pong. The override carries a Claude model (project-resolved default per
  difficulty: OPUS for `high`, else default), NOT a gpt-*/codex model.
- **Auth failures stay blocking:** `CodexAuthFailure` is a config/credential problem,
  not a task problem. Falling back on auth failure would mask operator misconfig, so
  auth failures continue to auto-block (and stay non-counting). **← sub-decision, see
  Open question F1.**
- **Opt-in vs always-on:** gate behind config so existing Codex projects don't silently
  change behavior. Reuse the spec shape: add `fallbackToClaude: bool` (default false)
  on the Codex `primaryRunner` spec. **← sub-decision, see Open question F2.**
- **Invariant test update:** `codex_runner_overrides_invariant.rs` stays valid (still
  no Codex in overrides). Add a NEW test asserting a failed Codex task promotes to
  `RunnerKind::Claude` in `runner_overrides` exactly once.
- **Commit:** insert as commit 4b (`feat(codex): Codex→Claude fallback on runtime
  failure`) after batch-preflight, before the test-port commit.

## Newly discovered risks (from the deep diff — not in the reviews)
- **R1 — Codex stream schema field name divergence (BLOCKER for correctness).**
  V1 parses `item.get("item_type")`; V2 parses `item.get("type")` inside `item`.
  They cannot both be right against the real `codex exec --json` output. V2 ALSO emits
  BOTH `ToolUse` + `ToolResult` on `item.completed` (duplicate ToolUse vs the
  `item.started` emit). **Action:** capture one real `codex exec --json` transcript and
  confirm the actual field name + event shape BEFORE finalizing `stream.rs`. Do not
  blind-copy V1's parser over V2's. Add the stub-binary e2e (commit 6) using the
  confirmed schema so a CLI bump can't silently break extraction.
- **R2 — KEEP V2's transient-backend check for Codex.** V2 added
  `is_transient_backend(stderr) → TransientBackend` on the Codex path (correct, because
  V2 pipes Codex stderr; V1 did NOT pipe it so V1 only had it for Grok). Do NOT regress
  this when porting runner.rs pieces from V1.
- **R3 — Auth marker set differs.** V1 has 10 markers (incl. "invalid bearer",
  "not authenticated", "missing-bearer", bare "401"/"unauthorized"); V2 has 6. When
  porting V1's structured `[Error:]`-only matcher, also bring V1's fuller marker list.
```
