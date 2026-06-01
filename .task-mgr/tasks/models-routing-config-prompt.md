# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Provider/Model Routing Config CLI** for **task-mgr**.

## Problem Statement

task-mgr routes tasks to providers/models via three config surfaces — `reviewModel` (the reviewer), `fallbackRunner` (overflow/RuntimeError escape), and `primaryRunner.byTaskType`/`byIdPrefix` (class routing). Today only `defaultModel` is settable via `task-mgr models set-default`; `reviewModel` and `fallbackRunner` are **hand-edited JSON only**, and there's **no way to see the full routing table**. This work adds `models set-review-model`/`unset-review-model` + `set-fallback`/`unset-fallback` subcommands and a routing-table view in `models show`.

**Config-first principle** (the design this enables): class/role routing lives in config (`reviewModel`/`primaryRunner`/`fallbackRunner`), which applies to runtime-spawned tasks the prompt-generator never sees; per-task `model` stamps are reserved for genuine one-offs (a rung-1 stamp blocks config from rerouting that task). These CLI setters write the *config* surface, they never stamp tasks.

---

## Non-Negotiable Process (Read Every Iteration)

1. **Internalize quality targets** — read `qualityDimensions`.
2. **Plan edge-case handling** — for each `edgeCases`/`failureModes` entry, decide handling before coding.
3. **Pick an approach** — only for `high`/`modifiesBehavior` tasks, name the one alternative you rejected.

After coding, the scoped quality gate is your critic — run it.

---

## Priority Philosophy

PLAN → PHASE 2 FOUNDATION → FUNCTIONING CODE → CORRECTNESS → CODE QUALITY → POLISH. Handle `Option`/`Result` explicitly (no `unwrap()` in production).

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals
- **A config setter that deserializes to a typed struct and reserializes — it silently DROPS unknown keys (`additionalAllowedTools`, `embeddingModel`, `ollamaUrl`, …). Setters MUST round-trip via `serde_json::Value` and mutate only the target key.**
- Error messages that don't identify the bad value or the allowed set
- A validation/probe that diverges from the runtime resolver (binary resolution must mirror exactly)

---

## Global Acceptance Criteria

- Rust: no warnings in `cargo check` / `cargo clippy -- -D warnings`
- Rust: scoped tests pass with `cargo test -p task-mgr <module>`; `cargo fmt --check` passes
- No literal model strings outside `src/loop_engine/model.rs` (read the `OPUS_MODEL`/`SONNET_MODEL` constants; `tests/no_hardcoded_models.rs` guards this)
- **Config writes preserve ALL pre-existing keys** (round-trip via `serde_json::Value`, never a typed reserialize)

---

## Task Files + CLI (context economy)

**Never read or edit `tasks/*.json` directly.** Everything per-task comes from `task-mgr next`; everything global is in this prompt file.

```bash
PREFIX=$(jq -r '.taskPrefix' .task-mgr/tasks/models-routing-config.json)
task-mgr next --prefix $PREFIX --claim
```

| Need | Command |
| --- | --- |
| Claim next task | `task-mgr next --prefix $PREFIX --claim` |
| Inspect a task | `task-mgr show $PREFIX-TASK-ID` |
| Recall learnings | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Spawn a fix (review only) | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

Progress log: `.task-mgr/tasks/progress-$PREFIX.txt` — tail the most recent section, append after each task:
```bash
tac .task-mgr/tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
```

---

## Your Task (every iteration)

1. Resolve prefix + claim: `task-mgr next --prefix $PREFIX --claim`. If none eligible → `<promise>BLOCKED</promise>` with the reason.
2. Tail the latest progress section (skip on first iteration).
3. `task-mgr recall --for-task <TASK-ID>`. Do NOT Read learnings files or CLAUDE.md in full — relevant excerpts are below.
4. Verify branch is `feat/models-routing-config`.
5. Think: assumptions, edge cases, cross-module data access (consult Data Flow Contracts / grep real call sites).
6. Implement one task (code + tests together).
7. Run the scoped quality gate (below). Fix before committing.
8. Commit `feat: <TASK-ID>-completed - [Title]`.
9. Emit `<task-status><TASK-ID>:done</task-status>`.
10. Append a tight progress block ending with `---`.

---

## Quality Checks

Per-iteration (scoped) — **pipe to a temp file and grep in the SAME command (CLAUDE.md §3):**
```bash
cargo test -p task-mgr models 2>&1 | tee /tmp/mrc-test.txt | tail -3 && grep "FAILED\|error\[" /tmp/mrc-test.txt | head -10
cargo clippy -- -D warnings 2>&1 | tee /tmp/mrc-clippy.txt | tail -3 && grep "^error" /tmp/mrc-clippy.txt | head -10
```
Scope to the touched module (`models`, `init`, `project_config`). Do NOT run the full workspace suite during normal iterations.

Full gate (REFACTOR-001 / REVIEW-001):
```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test 2>&1 | tee /tmp/mrc-full.txt | tail -5 && grep "FAILED\|error\[" /tmp/mrc-full.txt | head -20
```
If the command-reference table is gen-docs-managed, run `cargo run --bin gen-docs` and confirm `gen-docs -- --check` is green.

---

## Common Wiring Failures (REVIEW-001 reference)

- New subcommand added to the clap enum but not dispatched to a handler → wire the match arm.
- Config setter writes via typed reserialize → drops unknown keys (the load-bearing bug).
- Probe-after-write → a missing-binary config persists and fails at next loop startup; probe BEFORE write.
- `models show` recomputes routing with bespoke logic instead of reading the real `ProjectConfig`.

---

## Review Tasks

| Review | Priority | Spawns | Focus |
| --- | --- | --- | --- |
| REFACTOR-001 | 98 | `REFACTOR-FIX-xxx` (50-97) | DRY (shared Value-round-trip core), complexity, coupling |
| REVIEW-001 | 99 | `FIX-xxx`/`WIRE-FIX-xxx` (50-97) | subcommands reachable, key-preservation, probe-before-write, docs, full suite green |

Spawn fixes:
```sh
echo '{"id":"FIX-001","title":"...","description":"From REVIEW-001: ...","rootCause":"<file:line>","exactFix":"...","verifyCommand":"...","acceptanceCriteria":["..."],"priority":60,"touchesFiles":["..."]}' | task-mgr add --stdin --depended-on-by REVIEW-001
```
Do NOT put a `model` on spawned fixups — `primaryRunner.byIdPrefix` config routes them (config-first).

---

## Progress Report Format

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence]
Files: [paths]
Learnings: [1-3 one-line bullets]
---
```

---

## Stop / Blocked

- **Stop**: before `<promise>COMPLETE</promise>` verify all tasks `passes: true`, no new tasks pending, REVIEW-001 green on the full suite.
- **Blocked**: document in progress, create a clarification task (priority 0), emit `<promise>BLOCKED</promise>`.

---

## Key Learnings (from task-mgr recall)

- **[3108]** A startup binary probe must mirror the runtime resolver AND check the executable bit — paired invariant. The setters' grok-binary probe must use the SAME resolver (`resolve_and_verify_grok_binary` / `check_review_model_binary`) as the loop startup probe.
- **[3109]** Stderr-sniff / validation strings need a maintenance runbook + negative-control tests; error messages must name the bad value + allowed set.
- **[1454]** task-mgr DB state can diverge from the PRD JSON in worktrees — config (`.task-mgr/config.json`) is per-worktree and gitignored; setters operate on the project (`.task-mgr/config.json`) or user (`$XDG_CONFIG_HOME/task-mgr/config.json`) path resolved by the loader.

---

## CLAUDE.md Excerpts (only what applies)

- **`task-mgr models` subcommand**: `list`/`set-default`/`unset-default`/`show`. Config precedence (highest→lowest): explicit task `model` → `difficulty==high` → PRD `defaultModel` → project `.task-mgr/config.json defaultModel` → user `$XDG_CONFIG_HOME/task-mgr/config.json defaultModel` → none. `difficulty==high` always escalates to OPUS_MODEL. This task adds `set-review-model`/`set-fallback` + a routing view; update this section.
- **reviewModel** routes review-class tasks (`CODE-REVIEW-*`, `MILESTONE-FINAL`, `REVIEW-*`) as a **late override** (beats stamped per-task model). It's a plain model string → provider inferred via `provider_for_model` (so `grok-build` → Grok runner).
- **fallbackRunner (Grok)**: opt-in `.task-mgr/config.json` block (`enabled`/`provider`/`model`/`cliBinary`/`runtimeErrorThreshold`); fires at overflow rung 4 / RuntimeError. v1 contract: `provider` must be `grok`. Startup binary check runs before iteration 1 when enabled.
- **Logging (CONTRACT-LOG-001)**: CLI data → `ui::emit_data` (stdout); internal diagnostics → `tracing`. The `models show` routing view is CLI data → `ui::emit_data`.
- **gen-docs**: model IDs live only in `src/loop_engine/model.rs`; `cargo run --bin gen-docs` regenerates doc tables; CI runs `gen-docs -- --check`.

---

## Data Flow Contracts

```
ProjectConfig                                  // src/loop_engine/project_config.rs
  .review_model: Option<String>                // reviewModel
  .fallback_runner: Option<FallbackRunnerConfig> { enabled, provider, model, cli_binary, runtime_error_threshold }
  .primary_runner: Option<PrimaryRunnerConfig> { by_task_type: HashMap<String,RunnerSpec>, by_id_prefix: HashMap<String,RunnerSpec>, claude_fallback_model, runtime_error_threshold }
  RunnerSpec { provider: String, model: String }

// Setters (FEAT-001) MUST mutate the raw JSON, not the typed struct:
let mut v: serde_json::Value = serde_json::from_str(&existing)?;   // existing file or {}
v["reviewModel"] = json!(model_id);   // or v.as_object_mut().remove("reviewModel") to clear
std::fs::write(path, serde_json::to_string_pretty(&v)?)?;          // all other keys preserved

// Provider label for `models show` / validation:
provider_for_model(Some("grok-build")) == Provider::Grok           // src/loop_engine/model.rs:83
// grok review/fallback model → probe via resolve_and_verify_grok_binary / check_review_model_binary (project_config.rs:376/430)

// Config path resolution: reuse the loader's project/user path helpers (src/db/path.rs + project_config loader); do NOT hardcode.
```

---

## Feature-Specific Checks

- **Key-preservation is the load-bearing invariant** — every setter test must pre-seed unrelated keys (`additionalAllowedTools`, `embeddingModel`) and assert they survive.
- **Probe before write** — never persist a config that references a missing grok binary.
- **Explicit empty states in `models show`** — print `(unset)`/`disabled`/`(no routes)` so the operator can't confuse 'unset' with 'not printed'.
- **v1 fallback provider = grok only** — reject other providers with a clear message (matches the in-flight codex-runner PRD's config validation).

---

## Important Rules

- ONE task per iteration; commit after each; keep CI green; read before writing; minimal changes.
- Branch: **feat/models-routing-config**
