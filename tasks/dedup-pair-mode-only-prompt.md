# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Dedup pair-mode-only with full pair coverage** for **task-mgr**.

## Problem Statement

`task-mgr curate dedup` already records distinct-pair verdicts into `dedup_dismissals` so re-runs skip them. PR #7 shipped a complementary opt-in `--pair-mode` sketch (`build_pair_judgment_prompt`, `DedupBatchInput.candidate_pairs: Option<Vec<...>>`) alongside the legacy cluster-prompt path. Two coverage leaks remain even with `--pair-mode` enabled:

1. **Sub-batching gap.** When an embedding cluster of N > `max_cluster_batch` is sliced via `batch.chunks(7)`, candidate pairs spanning two slices (e.g., `(ids[6], ids[7])` for N=9) end up in no batch. The LLM never sees them; they never get recorded; every subsequent run re-considers them.
2. **Transitive-cluster ambiguity.** Today's candidate pairs are `C(cluster, 2)` of a transitive union-find component. Pairs A–C where `A~B`, `B~C`, but `cosine(A, C) < 0.65` are sent to the LLM even though the embedding pre-filter already considers them dissimilar.

This effort removes the cluster-prompt path entirely, makes pair-judgment the only mode, and replaces item-chunking with pair-batching so every cosine-≥-threshold candidate pair lands in at least one LLM batch and gets recorded once. Outcome: re-runs with no new learnings and no merges dispatch zero LLM calls — because every pair the system would consider is already in `dedup_dismissals` (or was retired via merge). The `OllamaEmbedder` concrete dependency moves behind a new `Embedder` trait so a second provider can drop in without touching dedup orchestration. The `--pair-mode` flag is removed (was opt-in sketch only; one release of life).

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use that can't be reasonably extended (the Embedder trait IS justified because it's named scaffolding for additional providers)
- Error messages that don't identify what went wrong or what config to inspect
- Catch-all error handlers that swallow context
- Cross-batch candidate pairs that silently end up in no LLM batch (the leak this PR exists to fix)
- `unwrap()` in production paths — use `map_err`/`?` with `TaskMgrError` variants
- Editing `tasks/*.json` directly (use `task-mgr add --stdin` / `<task-status>` tags)
- Reading `tasks/learnings.md` or `tasks/long-term-learnings.md` directly (use `task-mgr recall`)

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: All tests pass with scoped `cargo test` (full suite at REVIEW-001)
- Rust: `cargo fmt --check` passes
- No breaking changes to existing APIs unless explicitly required by this PR's scope (which DOES require breaking changes to dedup CLI and embedder construction)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/dedup-pair-mode-only.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` — priority auto-computed; DB + PRD JSON updated atomically                                                    |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level field that's not surfaced per-task (rare), pull it with `jq`, never a full Read:

```bash
jq '.globalAcceptanceCriteria' tasks/dedup-pair-mode-only.json
```

### Files you DO touch

| File                                       | Purpose                                                                    |
| ------------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/dedup-pair-mode-only-prompt.md`     | This prompt file (read-only)                                               |
| `tasks/progress-$PREFIX.txt`               | Progress log — **tail** for recent context, **append** after each task     |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration. Two targeted patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on, or a dependsOn task)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing; never crash on absent files.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/dedup-pair-mode-only.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes` — everything you need. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section. If `task-mgr next` listed a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log. Skip entirely on the first iteration.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, or the task touches a file that's likely documented there, `grep` for the relevant term and read only the surrounding lines:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```
   The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, Key Context, and the CLAUDE.md excerpts that matter here) are already embedded in **this prompt file**. Prefer it over re-reading source docs.

4. **Verify branch** — `git branch --show-current` matches `feat/dedup-review-followup`. Switch if wrong. This branch already has two commits ahead of `main` (`b0701db` review polish for #7; `bec3290` `--pair-mode` sketch). The work in this PRD continues on top.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section or grep 2-3 existing call sites. Never guess key types from variable names.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — one rejected alternative with a one-line reason is enough.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). Fix failures before committing; never commit broken code.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:` as appropriate).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON.

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---` so the next iteration's tail works.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` already picks: eligible tasks (`passes: false`, deps complete), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority. You don't pick — you claim what it returns.

---

## Behavior Modification Protocol (FEAT-001, FEAT-002, FEAT-003 declare `modifiesBehavior: true`)

When a task declares `modifiesBehavior: true`:

1. Read the specific callers/consumers named in the task description.
2. Decide per-caller: `OK` (proceed), `BREAKS` (split the task into per-context subtasks via `task-mgr add --stdin`, then `task-mgr skip` the original with reason "split into …"), or `NEEDS_REVIEW` (verify manually before implementing).
3. If multiple call sites need different handling (e.g., `VectorBackend` vs `try_embed_*` for the embedder trait), the plan already split them — don't re-split.

**Pre-resolved consumer impacts for this PRD:**

- **FEAT-001** (`Embedder` trait): callers migrated to `make_embedder` factory: `try_embed_learning`, `try_embed_learnings_batch`, `curate_embed`. `VectorBackend` stays concrete `OllamaEmbedder` (out of scope; `TaskMgrError::OllamaUnreachable` naming).
- **FEAT-002** (cluster fn signature change + prompt rename + cluster-prompt deletion): orchestrator in `mod.rs` and tests in `tests.rs` are the only consumers; both updated in-task.
- **FEAT-003** (`--pair-mode` removal + DedupBatchInput collapse + hard-require embeddings): CLI, `main.rs`, `types.rs`, orchestrator, and tests. No external callers of `DedupBatchInput` (it's `struct`-private to `mod.rs`).

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **REVIEW-001** runs the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (FEAT / FIX / REFACTOR-FIX tasks)

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
# Pipe through tee so the same command produces both a tail summary and grep results — never run twice.
cargo fmt --check
cargo check 2>&1 | tee /tmp/check.txt | tail -3 && grep -E "^error|^warning" /tmp/check.txt | head -10
cargo clippy -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -3 && grep -E "^error|^warning" /tmp/clippy.txt | head -10

# Scope tests to the touched module. For curate work:
cargo test --lib commands::curate:: 2>&1 | tee /tmp/test.txt | tail -5 && grep -E "FAILED|^error" /tmp/test.txt | head -10

# For embeddings work (FEAT-001):
cargo test --lib learnings::embeddings:: 2>&1 | tee /tmp/test.txt | tail -5 && grep -E "FAILED|^error" /tmp/test.txt | head -10
```

Scoping heuristic: start from `touchesFiles`. For each Rust file, run `cargo test --lib <module path>`. If you can't determine the scope confidently, widen to `cargo test --lib` (whole library, still cheaper than `cargo test`).

**Do NOT** run the entire workspace test suite (`cargo test` with no filter) during regular iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

These tasks run the **full, unscoped** suite on a clean checkout and must finish green:

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

If ANY test fails — including pre-existing failures that predate this change — REVIEW-001 fixes them. Default: **attempt every failure**, even ones that look out-of-scope. Trunk-green is the invariant this mechanism exists to protect.

Pragmatic escape hatch: if there are **more than ~12 failures AND they're all clearly unrelated to this work**, triage:

1. Fix everything attributable to this change's diff, inline in the REVIEW-001 commit.
2. For the remaining unrelated failures: spawn a single `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID.

Below the ~12-failure threshold, just fix them.

---

## Common Wiring Failures (REVIEW-001 reference)

New code must be reachable from production — REVIEW-001 verifies. Most common misses for this PRD specifically:

- `Embedder` trait defined but `make_embedder` factory not called from `try_embed_*`, `curate_embed`, or the new dedup hard-error path → unused-import warning is the smoke
- `embedding_provider` field added to `ProjectConfig` but not read by callers → `Option<String>` deserializes silently, no error, but the field never affects behavior
- `pack_pair_batches` defined and tested but `curate_dedup` still calls `batch.chunks(max_cluster_batch)` → grep for `chunks(max_cluster_batch)` in `src/commands/curate/mod.rs` returns hits = bug
- `--pair-mode` removed from CLI/types/main but `DedupBatchInput.candidate_pairs` still `Option<...>` → wrap not collapsed, dead `match` arm in worker
- Helper functions `dismissed_pairs_within`, `prune_batch_to_judgable`, `finalize_dedup_batch`, `build_standard_batches`, `unjudged_pairs_within`, `is_fully_dismissed` deleted but their tests not removed → `unresolved import` errors in tests
- CLAUDE.md updates name removed symbols → grep `is_fully_dismissed` in CLAUDE.md returns hits = stale doc

---

## Review Tasks

REFACTOR-001 and REVIEW-001 spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

### What each review looks for

| Review         | Priority | Spawns (priority)                  | Focus                                                                                                   |
| -------------- | -------- | ---------------------------------- | ------------------------------------------------------------------------------------------------------- |
| REFACTOR-001   | 98       | `REFACTOR-FIX-xxx` (50-97)         | DRY, complexity, coupling, clarity, pattern adherence, orphan `#[allow(dead_code)]` after deletions    |
| REVIEW-001     | 99       | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) | Idioms, security, error handling, no `unwrap()`, `qualityDimensions` met, wiring reachable, full-suite green, smoke runs pass |

Use the **rust-python-code-reviewer** agent when reviewing code. Document findings in the progress file. If a specific prior iteration produced something ugly and you don't want to wait for REFACTOR-001, invoke `/simplify` on that touchpoint directly — don't file a dedicated review task just for it.

### Spawning follow-up tasks

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 60,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

`--depended-on-by` wires the new task into REVIEW-001's `dependsOn` AND syncs the PRD JSON atomically — don't edit the JSON yourself. Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it **tight** — future iterations tail this.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block. If your entry is longer than ~25 lines, compress it — a future iteration has to read this.

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw, auto-appended). **Do not Read those files directly** during a loop iteration — they grow unboundedly. Instead:

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval of learnings scored for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries when recall is sparse

Record your own learnings with `task-mgr learn` so they're indexed for future recall. Don't append directly to those files.

**Write concise learnings** (1-2 lines each):

- GOOD: "`temps::chrono::Timezone` accessed via full path, not temps_core"
- BAD: "The temps crate exports Timezone from temps::chrono module, so when using it..."

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL tasks have `passes: true`
2. Verify no new tasks were created in final review
3. Verify REVIEW-001 passed with full suite green

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in the progress file
2. Create clarification task via `echo '{...}' | task-mgr add --stdin --depended-on-by <blocked-task>` (priority 0)
3. Output:

```
<promise>BLOCKED</promise>
```

---

## Reference Code

**Existing pair-mode sketch (commit `bec3290`)** — work BUILDS ON this, not replaces it from scratch:

- `build_pair_judgment_prompt` at `src/commands/curate/dedup.rs:~210–266` is the body that gets renamed to `build_dedup_prompt` in FEAT-002. The prompt format (CANDIDATE PAIRS enumeration, item body restricted to referenced learnings, UNTRUSTED guard, boundary delimiter) is already correct — preserve it.
- `DedupBatchInput.candidate_pairs: Option<Vec<(i64, i64)>>` at `src/commands/curate/mod.rs:~565–574` is the field that gets collapsed in FEAT-003. The worker's `match &batch_input.candidate_pairs` at `~620–630` becomes a single call.
- `unjudged_pairs_within` at `src/commands/curate/mod.rs:~1466–1478` is the existing un-judged-pair filter. The orchestrator's `dismissals.contains(p)` inline filter in FEAT-003 replaces it.

**Existing dismissal pipeline (DO NOT modify the body)**:

- `compute_dismissal_pairs` at `src/commands/curate/mod.rs:~1587–1628` — `unordered_pairs(batch_ids) − cluster_internal_pairs − prior_merged_touched` with `merge_map` rewrites. Body stays; rustdoc updated in FEAT-003 to reflect that `batch_ids` now denotes the items in a packed pair batch (may include cosine-<-threshold pairs).
- `record_dismissals` at `src/commands/curate/mod.rs:~1401` — `INSERT ... ON CONFLICT (id_lo, id_hi) DO NOTHING`, transactional per chunk. Unchanged.
- `parse_dedup_response` + `validate_clusters` at `src/commands/curate/dedup.rs:~276+` — same `Vec<RawDedupCluster>` response shape. Transitive cluster grouping over the candidate-pair subset still works (LLM may return `{A,B,C}` from candidates `(A,B), (B,C)` — merge is correct, stale `(A,C)` dismissal is inert per CLAUDE.md retired-id contract).

**Existing OllamaEmbedder (FEAT-001 wraps in a trait)**:

- `pub struct OllamaEmbedder` at `src/learnings/embeddings/mod.rs:28–176` — methods `new`, `is_available`, `embed`, `embed_batch`, `model`, `base_url`. All five `Embedder` methods map directly. `base_url()` stays accessible on the concrete struct for `VectorBackend`'s `TaskMgrError::OllamaUnreachable` construction.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this task list. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

- **[1414]** Embedding pre-filter (cluster by cosine before LLM dedup) reduces API calls — this PR refines the pattern, doesn't replace it
- **[1833]** Dismissal memory short-circuits LLM calls on re-runs — this PR closes the remaining sub-batching coverage gap that prevented zero-LLM re-runs
- **[1837]** `compute_dismissal_pairs` MUST exclude `cluster_internal_pairs` (LLM-grouped duplicates) from dismissals; merged source IDs MUST be rewritten via `merge_map` so dismissals reference surviving IDs — body unchanged in this PR
- **[2690]** Always normalize pairs to canonical `(min, max)` form via `normalize_pair`; v18 CHECK constraint backstops at schema level — `pack_pair_batches` MUST emit pairs in canonical form
- **[1818]** Dismissal CRUD pattern: `load_dismissals` + `record_dismissals` + `clear_dismissals` (was: + `is_fully_dismissed`, deleted in FEAT-003) — keep the three survivors as a cohesive trio
- **[1819]** Edge-case test matrix for combinatorial logic: empty / singleton / two-item / transitive / boundary — apply to every new helper (`pack_pair_batches`, `cluster_by_embedding_similarity` updated return, `make_embedder`)
- **[1401]** `ProjectConfig` pattern: `Option<String>` fields with `unwrap_or_else(|| DEFAULT.to_string())` at use sites; serde camelCase rename for JSON — `embedding_provider` follows this exactly
- **[1944]** HTTP client error handling consistency: OllamaEmbedder uses 3s connect + 30s read timeouts; keep this discipline when implementing `Embedder` trait for Ollama
- **[1918]** mockito mocks Ollama `/api/embed` HTTP endpoints — useful for the trait-dispatch test in FEAT-001
- **[1822]** When adding a new schema migration: matching v18 dedup_dismissals approach (4 specific tests: table exists, index exists, down migration, idempotent) — NOT applicable here; this PR adds no schema
- **[1420]** REVIEW-001 must grep for `WIRE-FIX`-style issues: new helpers reachable from production entry points; uncommitted wiring is the #1 failure mode
- **[1456]** Direct string keys preferred over hashing — pair `(i64, i64)` tuple as HashSet key is the canonical shape here, not a derived hash
- **[1426]** When CLAUDE.md describes config behavior, update it in the same PR (FEAT-004 in this PRD)

---

## CLAUDE.md Excerpts (only what applies to this change)

These bullets were extracted from `CLAUDE.md` for the subsystems this change touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md`.

### Embedding / Ollama Configuration (lines ~291–315)

```
`curate embed` generates local embeddings via Ollama for the dedup pre-filter. Configure in `.task-mgr/config.json`:
{
  "ollamaUrl": "http://localhost:11435",
  "embeddingModel": "hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0"
}

- Default URL: http://localhost:11435 (docker-compose remap)
- Default model: hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0 (1024 dimensions)
- Schema: Migration v15 adds learning_embeddings table (BLOB storage, little-endian f32)
- Graceful degradation: curate dedup works without Ollama [THIS CHANGES — FEAT-003 makes it hard-error]
```

### Dedup Dismissal Memory (lines ~374–418) — CURRENT STATE (this PR rewrites it)

```
`curate dedup` persists pairs the LLM has already examined and found distinct in the
dedup_dismissals table (migration v18: composite PK (id_lo, id_hi) plus CHECK (id_lo < id_hi),
plus idx_dedup_dismissals_hi). Subsequent runs skip batches whose every C(N,2) pair is already
dismissed [THIS FRAMING CHANGES — pair-batching, not batch-level skip].

- Pair normalization: normalize_pair() canonicalizes (a, b) to (min, max); v18 CHECK backstops
- Narrow conflict suppression: INSERT ... ON CONFLICT (id_lo, id_hi) DO NOTHING (not INSERT OR IGNORE)
- Multi-row INSERT: chunks of 256 pairs (512 params) per round-trip
- When dismissals recorded: after a successful LLM batch, every C(N,2) pair from the batch
  minus (a) pairs the LLM grouped as duplicates and (b) pairs whose IDs were retired by a
  strictly earlier batch [SEMANTICS PRESERVED; batch_ids meaning shifts to packed-pair-batch items]
- Merge-map rewrite: {A,B}→N retires source IDs; recorded pairs are rewritten via per-batch
  merge_map so dismissals reference surviving merged ID, not retired source
- When NOT recorded: dry_run=true OR LLM error (can't trust a batch whose result we never got)
- Forcing re-examination: --reset-dismissals clears the table
- DedupResult.clusters_skipped: serde default=0 [STAYS reserved for future use, always 0 post-refactor]
- No foreign keys to learnings — rows for retired learnings are inert and harmless
```

### Loop CLI Cheat Sheet (lines ~69–80)

```
- Add task: echo '{"id":"X-FIX-001",...}' | task-mgr add --stdin
- Link into milestone: append --depended-on-by MILESTONE-ID
- Mark status: emit <task-status>TASK-ID:done</task-status>
- Permission guard: loop iterations deny Edit/Write on tasks/*.json
- Never edit tasks/*.json directly — use CLI and tags above
```

### Learning Creation Chokepoint (lines ~213–235)

```
All production code paths that create learnings must go through LearningWriter
(src/learnings/crud/writer.rs). Construction → record/push_existing inside a transaction →
flush(conn) AFTER the transaction commits. Never flush inside a rusqlite::Transaction.
[NOT directly relevant to this PRD — dedup uses LearningWriter via curate_dedup's existing path; no changes needed.]
```

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments.

### Candidate pair flow: embeddings → cluster → batch → LLM → dismissal

```
1. Load: load_all_active_embeddings(conn, embed_model)
   Returns: Vec<LearningEmbedding> where LearningEmbedding { learning_id: i64, embedding: Vec<f32> }
   Source: src/learnings/embeddings/mod.rs:280

2. Cluster: cluster_by_embedding_similarity(&emb_pairs, emb_threshold)
   Returns (post-FEAT-002): EmbeddingClusterResult {
       clusters: Vec<Vec<i64>>,           // each inner Vec sorted ascending, outer sorted by c[0]
       candidate_pairs: Vec<(i64, i64)>,  // sorted ascending, normalized (lo < hi), no transitive A-C
   }
   Source: src/commands/curate/dedup.rs:73 (signature changes in FEAT-002)

3. Filter by dismissals: candidate_pairs.iter().filter(|p| !dismissals.contains(p))
   dismissals type: HashSet<(i64, i64)>, all entries canonical (lo < hi)
   Source: load_dismissals at src/commands/curate/mod.rs:~1300

4. Project pairs to clusters: HashMap<i64, usize> (id → cluster_idx), bucket pairs accordingly
   Each (lo, hi) pair: cluster_of[lo] == cluster_of[hi] always (pairs only exist within a cluster's union-find)

5. Pack into batches: pack_pair_batches(cluster_items, cluster_pairs, max_items)
   Returns: Vec<(Vec<DeduplicateLearningItem>, Vec<(i64, i64)>)>
   Coverage invariant: union(batches[i].1) == input cluster_pairs (set equality)
   Item bound: batches[i].0.len() <= max_items always

6. Build DedupBatchInput (post-FEAT-003): { items, candidate_pairs }
   Note: no Option<>, no already_judged_distinct field, no pair_mode bool

7. Worker: build_dedup_prompt(&items, &candidate_pairs, threshold)
   Always — no match on prompt mode. Single call path.

8. LLM response: Vec<RawDedupCluster> (unchanged shape)
   Parsed via parse_dedup_response → validate_clusters

9. Record dismissals: compute_dismissal_pairs(batch_ids, cluster_internal_pairs, prior_merged_ids, merge_map)
   batch_ids = items.iter().map(|i| i.id).collect() (union of items in the packed batch)
   cluster_internal_pairs = the pairs the LLM said are duplicates (canonical (lo, hi))
   Result: pairs to insert via record_dismissals
   Source: src/commands/curate/mod.rs:~1587 — body unchanged; rustdoc updated to reflect new batch_ids meaning
```

**Critical contracts:**

- Every pair throughout is `(i64, i64)` with `lo < hi` (normalize_pair canonical form). `HashSet::contains` works directly; no string conversion, no MD5.
- `dismissals` is loaded ONCE at the top of `curate_dedup` and threaded as `&HashSet<(i64, i64)>` to the filter step. Workers do NOT touch the DB.
- `pack_pair_batches` output's `Vec<(i64, i64)>` per batch is a slice of the filter step's output — NOT recomputed. Pass it through.
- `compute_dismissal_pairs` receives `batch_ids` (the items in the packed batch, not the candidate_pairs themselves). Its body computes `unordered_pairs(batch_ids)` internally; recording covers all `C(items.len(), 2)` pairs minus internal duplicates minus prior-merged-touched. Under pair-batching this may include cosine-<-threshold pairs that share a batch with a candidate pair — this is correct (they ARE distinct by the embedding pre-filter).

### Embedder factory flow

```
1. Read config: read_project_config(&dir) → ProjectConfig { ollama_url: Option<String>, embedding_model: Option<String>, embedding_provider: Option<String> (NEW in FEAT-001) }
   Source: src/loop_engine/project_config.rs

2. Resolve: let provider = config.embedding_provider.unwrap_or_else(|| "ollama".to_string());
            let endpoint = config.ollama_url.unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
            let model = config.embedding_model.unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string());

3. Construct: make_embedder(&provider, &endpoint, &model) → TaskMgrResult<Box<dyn Embedder>>
   For "ollama" → Box::new(OllamaEmbedder::new(endpoint, model))
   For unknown → Err(TaskMgrError::ConfigError(format!("unknown embedding provider: {}", provider)))

4. Use: embedder.is_available()?, embedder.embed_batch(&texts)?, etc.

Callers using make_embedder (FEAT-001):
- try_embed_learning           src/learnings/embeddings/mod.rs:~352
- try_embed_learnings_batch    src/learnings/embeddings/mod.rs:~402
- curate_embed Ollama wiring   src/commands/curate/mod.rs:~1175–1199

Caller staying concrete OllamaEmbedder (out of scope):
- VectorBackend                src/learnings/retrieval/vector.rs:60–95
```

---

## Feature-Specific Checks

Beyond the global quality gate, this PRD requires these specific verifications at REVIEW-001:

1. **Coverage invariant smoke**: spin up a test DB with 9 active learnings whose embeddings put them in one transitive cluster with all 36 pairs at cosine ≥ 0.65. Run `task-mgr curate dedup` with a mock LLM returning `[]`. Assert `SELECT COUNT(*) FROM dedup_dismissals` is at least 36 for those IDs (may be higher due to cosine-<-threshold incidentals, never lower).
2. **Zero-LLM re-run**: immediately re-run on the same DB. Assert stderr contains "No candidate pairs to examine" and `spawn_claude` is never called.
3. **Hard-error path**: drop the `learning_embeddings` table contents. Run `task-mgr curate dedup`. Assert non-zero exit with `ConfigError` message naming `task-mgr curate embed` AND `embeddingProvider`.
4. **Flag rejection**: `task-mgr curate dedup --pair-mode` returns non-zero with clap's "unexpected argument" error.
5. **`pack_pair_batches` coverage unit test**: input 12 sparse pairs over 9 items, max=7. Compute the union of all output batches' candidate_pairs. Assert set equality with input.
6. **Determinism**: call `pack_pair_batches` twice with bytewise-identical input. Assert outputs are bytewise-identical (use `assert_eq!` on the full `Vec`).
7. **No leftover symbols**: `grep -rn "is_fully_dismissed\|finalize_dedup_batch\|prune_batch_to_judgable\|dismissed_pairs_within\|build_standard_batches\|unjudged_pairs_within\|build_pair_judgment_prompt\|pair_mode" src/` returns zero hits in production code (test/comment occurrences only — and ideally zero of those too).

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- Work on the correct branch: **feat/dedup-review-followup**
- This branch already has two commits ahead of `main` (`b0701db`, `bec3290`) — the work in this PRD continues on top. Do NOT rebase those out.
