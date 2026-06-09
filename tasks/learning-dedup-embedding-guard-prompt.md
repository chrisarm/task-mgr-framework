# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Inline embedding guard for near-duplicate learnings** for **task-mgr**.

## Problem Statement

`curate dedup` repeatedly merges learnings whose `source_ids` sit within ~3 of each other (`[8033, 8034, 8037]`, `[8041, 8043]`, …). These tight clusters are **cross-iteration semantic duplicates created inside a single autonomous loop run**: the loop-engine auto-extraction path `extract_learnings_from_output` (src/learnings/ingestion/mod.rs) mines a learning from each iteration's output via Haiku, and when two iterations surface the *same* lesson with slightly different title wording, both rows insert. The only write-time guard today is an exact `(outcome, title)` string match (`learning_exists`), which reworded titles sail through. The post-hoc `curate dedup` LLM pass cleans them up later, but they pollute `recall` in the meantime.

**Goal:** stop the near-duplicate from being inserted in the first place by adding an inline embedding-similarity guard to the ingestion recording loop. Degrade cleanly to the existing exact-match guard when Ollama is unavailable so there is **zero behavior change offline**.

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

**Key principle for this work — asymmetric risk:** a false positive (wrongly dropping a real, distinct learning) is **unrecoverable** — there is no LLM second opinion at write time. A false negative (a dupe slips through) is cheap — `curate dedup` catches it later. So **bias toward recording**: any uncertainty (Ollama down, embed error, empty text) must record, never skip.

**Prohibited outcomes:**

- Tests that assert `try_embed_*` returns 0 or "Ollama is down" — they fail when local Ollama is actually running (learning #1513). Test the PURE cosine decision (`find_near_duplicate`) with synthetic vectors instead.
- Silently dropping a learning when the embedding call errors mid-batch — uncertainty must record, never skip.
- Bypassing the `LearningWriter` chokepoint by calling `store_embedding` inline to avoid the flush re-embed.
- Applying the new guard to the human `task-mgr learn` path or `import_learnings` — ingestion auto-extraction ONLY.
- Tests that only assert "no crash" or check a type without verifying the similarity/skip behavior.

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: Scoped tests pass with `cargo test` for the touched module
- Rust: `cargo fmt --check` passes
- No breaking changes to existing public APIs (`ExtractionResult` shape, `learning_exists` signature) unless explicitly required
- No literal Claude model strings introduced outside `model.rs` (`tests/no_hardcoded_models.rs`)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/learning-dedup-embedding-guard.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001`                                                                                                                |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) |

### Files you DO touch

| File                                                  | Purpose                                                                |
| ----------------------------------------------------- | --------------------------------------------------------------------- |
| `tasks/learning-dedup-embedding-guard-prompt.md`      | This prompt file (read-only)                                          |
| `tasks/progress-$PREFIX.txt`                          | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist).

---

## Your Task (every iteration)

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/learning-dedup-embedding-guard.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes everything you need. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — usually just the most recent section. Skip on the first iteration.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do NOT Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly. Do NOT Read `CLAUDE.md` in full — the excerpts below are authoritative; `grep -n -A 10 '<header>' CLAUDE.md` for anything more.

4. **Verify branch** — `git branch --show-current` matches `feat/learning-dedup-embedding-guard`.

5. **Think before coding** — state assumptions; for each `edgeCases`/`failureModes` entry note how it's handled; for cross-module data access grep 2-3 call sites, never guess key types.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (below — scoped tests only).

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`. Do NOT edit the JSON.

10. **Append progress** — ONE block, terminated with `---`.

---

## Behavior Modification Protocol (FEAT-002 is `modifiesBehavior: true`)

FEAT-002 changes the side effects of `extract_learnings_from_output` (it skips inserting near-duplicate learnings). The **consumer** is the loop-engine iteration pipeline (`src/loop_engine/iteration_pipeline.rs`, learning #2135), which consumes only `ExtractionResult { learnings_extracted, learning_ids }`. That struct shape is **unchanged** — consumers simply observe fewer recorded rows, which is the intended improvement. Verdict: **OK** to proceed; no caller split needed. Do confirm by grep that no caller depends on a specific extracted count.

---

## Quality Checks

### Per-iteration scoped gate (FEAT tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test learnings::embeddings        # FEAT-001
cargo test learnings::ingestion         # FEAT-002
```

Scope to the touched module. **Do NOT** run the entire workspace suite during FEAT iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

If ANY test fails — including pre-existing failures — REVIEW-001 fixes them. **Gotcha (CLAUDE.md):** mass fixture-read failures all naming a removed `…-slot-N` worktree path are stale shared-target binaries, NOT a regression — `touch tests/<binary>.rs` and rebuild before concluding anything broke.

---

## Common Wiring Failures (REVIEW-001 reference)

- New `NearDuplicateChecker` defined but the ingestion loop never constructs/calls it → guard is dead code.
- `register()` never called on accepted Unique candidates → intra-batch dupes leak.
- Tier-2 placed before Tier-1, or Tier-1 gated on Ollama → exact-match regression when Ollama down.
- New public items trigger unused warnings → call site missing.

---

## Review Tasks

| Review         | Priority | Spawns (priority)                  | Focus                                                                          |
| -------------- | -------- | ---------------------------------- | ------------------------------------------------------------------------------ |
| REFACTOR-001   | 98       | `REFACTOR-FIX-xxx` (50-97)         | DRY, complexity, coupling, clarity, pattern adherence                          |
| REVIEW-001     | 99       | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) | Idioms, security, error handling, no `unwrap()`, wiring reachable, full-suite green, docs |

Use the **rust-python-code-reviewer** agent when reviewing. Spawn follow-ups:

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

Do NOT put a `model` on spawned FIX/WIRE-FIX tasks — `primaryRunner.byIdPrefix` config routes them.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight (~10 lines):

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

---

## Learnings Guidelines

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval. Do NOT Read the learnings markdown files directly.
- Record your own with `task-mgr learn`. Keep them 1-2 lines.

---

## Stop and Blocked Conditions

### Stop Condition

Before `<promise>COMPLETE</promise>`: verify ALL tasks `passes: true`, no new tasks left open in final review, REVIEW-001 passed with full suite green. Then output `<promise>COMPLETE</promise>`.

### Blocked Condition

If blocked: document in progress file, create a clarification task via `task-mgr add --stdin --depended-on-by <blocked-task>` (priority 0), output `<promise>BLOCKED</promise>`.

---

## Reference Code

**FEAT-001 — resolve config once into LOCALS (no owned `model` field — review finding #2), then build the checker (mirror `try_embed_learning`, embeddings/mod.rs ~357-405):**

```rust
let proj = read_project_config(db_dir);
let url = proj.ollama_url.unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
let model = proj.embedding_model.unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string());
let embedder = OllamaEmbedder::new(&url, &model);   // embedder retains model internally
if !matches!(embedder.is_available(), Ok(true)) { return None; }
let known = match load_all_active_embeddings(conn, &model) {   // model used for BOTH, then dropped
    Ok(v) => v.into_iter().map(|le| (le.learning_id, le.embedding)).collect(),
    Err(e) => { eprintln!("Warning: near-dup checker load failed: {e}"); return None; } // finding #4
};
// struct holds { embedder, threshold, known } — NO model field (dead_code under -D warnings)
```

`best_match(candidate, known)` is the pure primitive (highest-sim regardless of threshold); `find_near_duplicate = best_match(..).filter(|(_, s)| *s >= threshold)`. `check()` reuses `best_match` for both the Duplicate decision and the near-miss log (`NEAR_MISS_LOG_FLOOR = 0.80 <= sim < threshold`).

**Existing reuse targets (all in `src/learnings/embeddings/mod.rs`):**
- `pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32`
- `pub fn compose_embed_text(title: &str, content: &str) -> String`
- `pub fn load_all_active_embeddings(conn, model) -> TaskMgrResult<Vec<LearningEmbedding>>` where `LearningEmbedding { learning_id: i64, embedding: Vec<f32> }`
- `OllamaEmbedder::{new, embed, is_available, model}`

**FEAT-002 — test seam (review finding #1).** `extract_learnings_from_output` calls `spawn_claude` (~mod.rs:105) *before* the recording loop, so move the loop into a private helper and inject the guard behind a trait so the arms are testable with a fake (no subprocess, no Ollama):

```rust
trait NearDupGuard {
    fn check(&self, title: &str, content: &str) -> NearDupOutcome;
    fn register(&mut self, id: i64, embedding: Vec<f32>);
}
impl NearDupGuard for NearDuplicateChecker { /* forward to inherent methods */ }

fn record_extracted_learnings(
    conn: &Connection,
    writer: &mut LearningWriter,
    params_list: Vec<RecordLearningParams>,
    guard: Option<&mut dyn NearDupGuard>,
) -> TaskMgrResult<(Vec<i64>, usize /* deduped */)> { /* Tier-1 then Tier-2 */ }
```

`extract_learnings_from_output` = spawn → parse → enrich → build `Option<NearDuplicateChecker>` → `record_extracted_learnings(conn, &mut writer, params_list, checker.as_mut().map(|c| c as &mut dyn NearDupGuard))` → `writer.flush(conn)`. Records against a bare `&Connection` (NO transaction). `learning_exists` (~mod.rs:256-267) stays the **first, unconditional** Tier-1 check.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this task list. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task needs one not here (then `task-mgr recall --query <text>`).

- **#1414** Embedding pre-filter before LLM dedup: cluster by cosine similarity on stored embeddings — the established dedup pattern; reuse `cosine_similarity` + `load_all_active_embeddings`.
- **#1658** Vector backend loads embeddings separately via `load_all_active_embeddings` and post-filters in Rust — same loader this guard uses.
- **#1513** Tests asserting `try_embed_*` count == 0 (assuming Ollama down) FAIL when local Ollama is running. Test the pure `find_near_duplicate` with synthetic vectors; never assume Ollama state in unit tests.
- **#2670** Graceful degradation for optional services (Ollama): return None / empty and degrade, never error. `NearDuplicateChecker::new -> None` when unavailable.
- **#2726** `LearningWriter` lifecycle: construct BEFORE the loop, `record` in the loop, `flush` AFTER. Do not reorder; do not bypass with inline `store_embedding`.
- **#2135** `extract_learnings_from_output` is invoked from the unified iteration pipeline (`src/loop_engine/iteration_pipeline.rs`) — the sole production caller / wiring point to verify.
- **#1456** Learning dedup key today is `(outcome, title)` string match — leave Tier-1 as-is; widening to title+content is out of scope.
- **#1540** Verify chokepoint enforcement by grepping for bypassed calls — apply when confirming the guard didn't sidestep `LearningWriter`.

---

## CLAUDE.md Excerpts (only what applies to this change)

These bullets are extracted from `CLAUDE.md` and `src/learnings/CLAUDE.md` for the subsystems this change touches. Do NOT Read the full files.

- **Learning Creation Chokepoint**: all production paths that create learnings go through `LearningWriter` (`src/learnings/crud/writer.rs`). Pattern: `LearningWriter::new(Some(db_dir))` → `record(conn, params)` (one+ times) → `flush(conn)` AFTER any enclosing transaction commits. Never flush inside a `rusqlite::Transaction`. The ingestion path (`extract_learnings_from_output`) is one of the four production paths.
- **Graceful degradation for optional services**: Ollama embeddings are best-effort. Embedding failures are swallowed; `curate embed` backfills later. New code must not turn an Ollama outage into a hard error.
- **Models gotcha**: never introduce literal Claude model strings outside `src/loop_engine/model.rs` — `tests/no_hardcoded_models.rs` enforces this. (This change shouldn't touch model IDs at all, but REVIEW must confirm.)
- **Stale slot-worktree test binaries (full-suite gotcha)**: mass `cargo test` failures that all name a removed `…-slot-N/tests/fixtures/…` path are stale shared-target binaries, not a regression — `touch tests/<binary>.rs` and rebuild before concluding the change broke tests.
- **`src/learnings/CLAUDE.md` is auto-loaded** when files in `src/learnings/` are read — REVIEW-001 must update its "Learning Creation Chokepoint" narrative to document the new write-time near-dup guard.

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- Work on the correct branch: **feat/learning-dedup-embedding-guard**
