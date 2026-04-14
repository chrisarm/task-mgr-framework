# Claude Code Agent Instructions

You are an autonomous coding agent implementing **LearningWriter: a common chokepoint for creating learnings + scheduling embeddings** for **task-mgr**.

## Problem Statement

The task-mgr project has four independent code paths that create learnings in SQLite, and each one wires up Ollama embeddings differently — or not at all:

1. `learn()` — single record + ad-hoc `try_embed_learning` call
2. `import_learnings()` — batch in a tx, ad-hoc `try_embed_learnings_batch` after commit
3. `curate_dedup()` → `merge_cluster()` — per-cluster atomic tx, ad-hoc batch embedding after the loop
4. **`extract_learnings_from_output()` (loop engine ingestion)** — calls `record_learning` in a loop with **NO embedding step at all**. This is a silent gap: loop-extracted learnings only get embedded when someone manually runs `curate embed`.

No single chokepoint enforces that every creation also schedules an embedding. A future dev can add a fifth path and silently forget the embedding step — which has already happened once (path #4).

**Goal**: introduce a `LearningWriter` facade that is the required production entry point for creating learnings. Every path uses it. Ollama HTTP calls are deferred via `writer.flush()` so they never run inside a SQLite transaction.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Internalize quality targets** — Read `qualityDimensions` and define what "done well" looks like for THIS task
2. **Map edge cases to implementation plan** — Read `edgeCases`/`failureModes`; for each, decide HOW it will be handled before coding
3. **Choose your approach** — State assumptions, consider 2-3 approaches with tradeoffs, pick the best, document in progress.txt
4. **After coding, self-critique** — "Does this satisfy every qualityDimensions constraint? Every edge case? Is it idiomatic and efficient?" — revise before moving on

---

## How to Work

1. Read `.task-mgr/tasks/learning-writer-refactor.json` for your task list
2. Read `.task-mgr/tasks/progress.txt` (if exists) for context from previous iterations
3. Read `.task-mgr/tasks/long-term-learnings.md` (if exists) for project patterns
4. Read `CLAUDE.md` for project conventions
5. Pick the highest-priority eligible task (`passes: false`, all `dependsOn` complete)
6. **Before coding**: Read the task's DO/DO NOT sections, qualityDimensions, and edgeCases. State your approach briefly.
7. **Implement**: Code + tests together in one coherent change
8. **After coding**: Self-critique — check each acceptance criterion, especially negative ones and known-bad discriminators
9. Run quality checks (below)
10. Commit: `feat: TASK-ID-completed - [Title]`
11. Output `<completed>TASK-ID</completed>`
12. Append progress to `.task-mgr/tasks/progress.txt`

---

## Priority Philosophy

What matters most, in order:

1. **PLAN** — Anticipate edge cases. Consider approaches. Read qualityDimensions first.
2. **FUNCTIONING CODE** — Pragmatic, reliable code that works according to plan
3. **CORRECTNESS** — Self-critique after code. Compiles, type-checks, all tests pass
4. **CODE QUALITY** — Clean code, good patterns, no warnings

**Prohibited outcomes (project-specific):**

- Any production creation path that bypasses LearningWriter and calls `record_learning` directly
- Ollama HTTP calls issued inside an active rusqlite Transaction (would hold DB locks during network I/O)
- `LearningWriter` that panics from Drop (stack-unwind UB) — use `eprintln!` only
- Breaking changes to `record_learning` signature — 25+ test files depend on it
- Tests that only assert "no crash" or check type without verifying content
- Silent failures: embedding errors must log to stderr with learning_id context

---

## Key Context

### Working tree state

Clean. You're starting from a fresh checkout of `main`. Recent commits already landed:

- `0accd19 feat: configurable dedup model (default haiku) and auto-embed on learn/import` — this added the `DedupParams.model` field, the `dedupModel` ProjectConfig setting, and the initial (now-superseded) `try_embed_learning` / `try_embed_learnings_batch` helpers in `src/learnings/embeddings/mod.rs`.

Those helpers already exist and work — `LearningWriter.flush()` will call `try_embed_learnings_batch` internally. Do not delete them; wrap them.

**What needs to change**: `DedupParams` does NOT currently have a `db_dir` field (FEAT-004 will add it) and `learn()` does NOT currently take a `db_dir` parameter (FEAT-002 will add it). Build on the clean `main` state.

### Files to modify

- `src/learnings/crud/writer.rs` (NEW — LearningWriter + PendingEmbed + Drop + tests)
- `src/learnings/crud/mod.rs` (add `pub mod writer; pub use writer::LearningWriter;`)
- `src/learnings/mod.rs` (top-level re-export)
- `src/commands/learn.rs` (migrate to writer)
- `src/commands/import_learnings/mod.rs` (migrate to writer; delete ad-hoc `created` vec)
- `src/commands/curate/mod.rs` (use writer in curate_dedup; delete inline embedding block)
- `src/commands/curate/types.rs` (add `merged_title`, `merged_content` to `MergeClusterResult`)
- `src/commands/curate/tests.rs` (update any destructures of `MergeClusterResult`)
- `src/learnings/ingestion/mod.rs` (add `db_dir: Option<&Path>` param; migrate to writer)
- `src/loop_engine/engine.rs` (pass `Some(params.db_dir)` at the extract_learnings_from_output call site)
- `src/main.rs` (pass `Some(&cli.dir)` at the ExtractLearnings command arm)
- `CLAUDE.md` (REVIEW-001: add "Learning creation chokepoint" note)

### Key functions/types to reuse

- `record_learning` at `src/learnings/crud/create.rs:27` — the low-level primitive. LearningWriter wraps this; tests and `curate_enrich` still call it directly, that's fine.
- `try_embed_learnings_batch` at `src/learnings/embeddings/mod.rs` — already best-effort, batches Ollama HTTP, logs errors. LearningWriter.flush() calls this.
- `RecordLearningParams` at `src/learnings/crud/types.rs` — passed to writer.record unchanged.
- `ProjectConfig` at `src/loop_engine/project_config.rs` — already has `ollama_url` and `embedding_model` read via `read_project_config`. The writer reads these indirectly through `try_embed_learnings_batch`.
- `MergeClusterParams` / `MergeClusterResult` at `src/commands/curate/types.rs` — add two fields to the result (FEAT-004).
- `LoopExecutionParams` in `src/loop_engine/engine.rs` at line ~106 — has `pub db_dir: &'a Path`. Use this for FEAT-005's call site update.

### Key learnings from task-mgr (from prior loop runs)

- **[111]** `rusqlite Transaction` auto-derefs to `&Connection` for function calls — pass `&tx` directly where `&Connection` is expected. No wrapper or trait needed.
- **[1182]** Clippy rejects `&*tx` explicit auto-deref — use `&tx`.
- **[885]** `conn.transaction()` requires `&mut Connection`. The caller holds the tx lifecycle.
- **[110]** Load existing state (e.g., dedup HashSet) BEFORE opening a tx in rusqlite — avoid mutable borrow conflicts.
- **[112]** Within-batch dedup via `HashSet::insert` return value — already used in `import_learnings`; preserve.
- **[693]** Prefer `.expect("invariant reason")` over `.unwrap()` in production. `.unwrap()` is unsafe.
- **[1400]** Graceful degradation: return empty/zero results when Ollama is unavailable, don't propagate errors.
- **[1401]** Embedding config comes from `ProjectConfig` — `ollama_url`, `embedding_model` are `Option<String>` with sensible defaults.
- **[1426]** CLAUDE.md has an existing Embedding/Ollama config section — extend, don't duplicate.
- **[261]** Enrichment is wired AFTER `parse_extraction_response` in `extract_learnings_from_output`'s pipeline — preserve that order.
- **[1271]** Engine integration pattern: imports, startup log, loop body hook — for FEAT-005, only the loop body hook changes.
- **[1195]** Read all consumers of duplicated code BEFORE refactoring — grep `MergeClusterResult {` and `merge_cluster(` before FEAT-004.
- **[824] / [1244]** `cargo test` may surface warnings that `cargo clippy` didn't — run both as part of quality checks.
- **[880]** Inline orchestrator blocks are the real refactoring targets — watch `curate_dedup`'s orchestration.
- **[74] / [55]** Pure-logic modules with comprehensive unit tests pass review cleanly — model `src/learnings/crud/writer.rs` on this pattern.
- **[487] / [488]** Don't over-extract modules — if writer.rs fits in ~150 lines, keep it whole; don't split for SRP's sake alone.
- **[1429]** New curate subcommand requires changes to 5 files in fixed order — but this is NOT a new subcommand, just an internal refactor. No CLI/handlers.rs changes.

### Callers to preserve compatibility with

- `record_learning`: 25+ files including many tests. **Do not rename, remove, or change its signature.** LearningWriter wraps it.
- `learn()` in src/commands/learn.rs: 17 unit tests in the same file + 2 integration tests in tests/. They pass `None` for `db_dir` — that stays working.
- `merge_cluster()`: 26 direct test call sites. **Do not** change its signature. Only `MergeClusterResult` grows.
- `extract_learnings_from_output`: called from main.rs and loop_engine/engine.rs. The 5th parameter `db_dir: Option<&Path>` is additive; old tests can pass `None`.
- `DedupParams::default()`: test-constructed with `db_dir: None` — no-op for writer.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types or field names from variable names.

### RecordLearningParams → LearningWriter → record_learning

```rust
// src/learnings/crud/types.rs — the input struct for record_learning
pub struct RecordLearningParams {
    pub outcome: LearningOutcome,       // enum
    pub title: String,                   // CLONE this before moving into record_learning
    pub content: String,                 // CLONE this before moving into record_learning
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    // ... other fields ...
    pub tags: Option<Vec<String>>,
    pub confidence: Confidence,
}

// Access pattern in LearningWriter.record():
pub fn record(&mut self, conn: &Connection, params: RecordLearningParams) -> TaskMgrResult<RecordLearningResult> {
    let title = params.title.clone();      // BEFORE move
    let content = params.content.clone();  // BEFORE move
    let result = record_learning(conn, params)?;  // params MOVED here
    if self.db_dir.is_some() {
        self.pending.push(PendingEmbed { learning_id: result.learning_id, title, content });
    }
    Ok(result)
}
```

### PendingEmbed → try_embed_learnings_batch

```rust
// src/learnings/crud/writer.rs (NEW)
struct PendingEmbed {
    learning_id: i64,    // i64 (matches learnings.id PRIMARY KEY)
    title: String,
    content: String,
}

// src/learnings/embeddings/mod.rs:try_embed_learnings_batch signature:
pub fn try_embed_learnings_batch(
    conn: &Connection,
    db_dir: &Path,       // &Path, NOT Option<&Path>; writer resolves this internally
    learnings: &[(i64, String, String)],  // tuple form: (id, title, content)
) -> usize;

// Access pattern in LearningWriter.flush():
pub fn flush(mut self, conn: &Connection) -> usize {
    let Some(dir) = self.db_dir else { return 0; };
    if self.pending.is_empty() { return 0; }
    let items: Vec<(i64, String, String)> = std::mem::take(&mut self.pending)
        .into_iter()
        .map(|p| (p.learning_id, p.title, p.content))
        .collect();
    try_embed_learnings_batch(conn, dir, &items)
}
```

### MergeClusterResult (FEAT-004 adds two fields)

```rust
// src/commands/curate/types.rs — BEFORE (do not pattern-destructure the old shape in new code)
pub struct MergeClusterResult {
    pub merged_learning_id: i64,
    pub retired_source_ids: Vec<i64>,
    pub skipped_source_ids: Vec<i64>,
}

// AFTER FEAT-004:
pub struct MergeClusterResult {
    pub merged_learning_id: i64,
    pub merged_title: String,     // NEW — clone params.merged_title before moving into record_learning
    pub merged_content: String,   // NEW — clone params.merged_content before moving into record_learning
    pub retired_source_ids: Vec<i64>,
    pub skipped_source_ids: Vec<i64>,
}

// Access pattern in curate_dedup (FEAT-004):
match merge_cluster(conn, merge_params) {
    Ok(result) => {
        learnings_merged += result.retired_source_ids.len();
        learnings_created += 1;
        writer.push_existing(
            result.merged_learning_id,
            result.merged_title,    // MOVED (no more clone needed if result isn't used later)
            result.merged_content,
        );
        // But all_clusters.push also needs merged_title/content — clone earlier or restructure
    }
    Err(e) => { /* existing warning + continue */ }
}
```

**CONTRACT**: Because `all_clusters.push(DedupCluster { ... merged_title, merged_content ... })` later in `curate_dedup` also needs the strings, either clone them for `push_existing` or clone them for `DedupCluster`. Pick whichever costs less churn. Read lines 870-912 of curate/mod.rs before deciding.

### LoopExecutionParams → extract_learnings_from_output (FEAT-005)

```rust
// src/loop_engine/engine.rs:~106
pub struct LoopExecutionParams<'a> {
    pub conn: /* Connection */,
    pub db_dir: &'a Path,   // <-- this one — pass Some(params.db_dir) at the call site
    pub run_id: &'a str,
    // ... many more fields ...
}

// Call site at src/loop_engine/engine.rs:~639 — current:
extract_learnings_from_output(
    params.conn,
    learning_source,
    Some(&task_id),
    Some(params.run_id),
)

// AFTER FEAT-005 (add 5th parameter):
extract_learnings_from_output(
    params.conn,
    learning_source,
    Some(&task_id),
    Some(params.run_id),
    Some(params.db_dir),   // NEW — passes the writer's db_dir
)
```

---

## What Good Looks Like vs What Bad Looks Like

### Good implementation patterns

- **Chokepoint discipline**: every production path constructs `LearningWriter::new(db_dir)`, calls `.record()` or `.push_existing()`, and ends with `.flush()`. No direct `record_learning` or `try_embed_learnings_batch` calls in command code.
- **Deferred Ollama**: `flush()` is the ONLY place that makes HTTP calls. `record()` is pure DB.
- **Clone-before-move**: clone `title`/`content` from `params` BEFORE the move into `record_learning`. Simplest and avoids lifetime gymnastics.
- **Consume-on-flush**: `flush(self)` prevents reuse. The writer is dead after flush.
- **Drop warns, never panics**: `eprintln!` only. Panicking from Drop is UB during stack unwinding.
- **Graceful degradation**: Ollama down → flush returns 0 silently. DB writes are authoritative; embeddings are best-effort.
- **Doc comments**: every pub method explains the tx rule ("pass `&tx` when inside a transaction; Transaction derefs to Connection").
- **Tests at creation**: FEAT-001's new writer.rs has unit tests in the same file, using the existing `setup_db()` pattern from embeddings/mod.rs tests.

### Bad patterns to avoid

- **`writer.flush(&tx)` before `tx.commit()`** — runs Ollama inside the tx, holding DB locks during network I/O. Always flush AFTER commit.
- **Panicking Drop** — `panic!("un-flushed")` in Drop will abort the process during unwinding. Use `eprintln!` only.
- **`record_learning` bypass** — calling `record_learning` directly from command code instead of through `writer.record`. This is exactly the gap we're closing.
- **`#[must_use]` on LearningWriter** — doesn't work for locals; only applies to function return values. Don't waste time on it.
- **Threading `&mut LearningWriter` through `merge_cluster`** — breaks 26 tests. Use `push_existing` from the caller instead.
- **Making `db_dir` required (non-Option)** in `extract_learnings_from_output` — breaks unit tests that don't have a db_dir. Use `Option<&Path>` and tests pass `None`.
- **Changing `record_learning`'s signature** — 25+ files depend on it. Leave it alone; wrap it.
- **Per-call `flush()` inside a loop** — turns one batched Ollama call into N single calls. Flush ONCE after the entire loop.
- **Losing the `eprintln!` per-learning error in `extract_learnings_from_output`** — individual record failures must log+continue, not abort via `?`.
- **`.unwrap()` in production code** — use `.expect("invariant explanation")` with a meaningful reason.
- **Abstracting over `Connection`/`Transaction` with a trait** — rusqlite doesn't have one, and `Deref` already solves it. Pass `&tx` directly.

---

## Smart Task Selection

Tasks have relationship fields:

```json
{
  "touchesFiles": ["src/module/file.rs"],
  "dependsOn": ["FEAT-001"],   // HARD: Must complete first
  "synergyWith": ["FEAT-002"]  // SOFT: Share context
}
```

### Selection Algorithm

1. **Filter eligible**: `passes: false` AND all `dependsOn` complete
2. **Check synergy**: Prefer tasks where `synergyWith` contains the previous task's ID
3. **Check file overlap**: Prefer tasks with `touchesFiles` matching previous iteration's files
4. **Fall back**: Pick highest priority (lowest number)

Expected order:
- **FEAT-001 first** (no deps, foundation).
- After FEAT-001, FEAT-002/003 in either order (they're independent, both depend only on FEAT-001). FEAT-004 and FEAT-005 extend patterns from the earlier migrations.
- **REFACTOR-001** and **REVIEW-001** last.

---

## Common Wiring Failures

| Symptom                                   | Cause                                          | Fix                        |
| ----------------------------------------- | ---------------------------------------------- | -------------------------- |
| Code compiles but feature doesn't work    | Not registered in dispatcher/router            | Add to registration        |
| Tests pass but prod doesn't use code      | Test mocks bypass real wiring                  | Verify production path     |
| New config field has no effect            | Config read but not passed to component        | Wire config through        |
| Old behavior persists                     | Conditional still routes to old code           | Update routing logic       |
| Function returns nil/default              | Wrong key type (atom vs string)                | Verify key types match     |
| Tests pass but runtime returns wrong data | Test uses hand-built map matching wrong format | Use production-shaped data |
| **LearningWriter not reached from loop engine** | **`extract_learnings_from_output` called with `db_dir: None`** | **Verify engine.rs passes `Some(params.db_dir)`** |
| **Ollama calls timeout SQLite operations** | **`writer.flush()` called before `tx.commit()`** | **Move `flush()` AFTER commit** |
| **MergeClusterResult destructure error in tests** | **FEAT-004 added fields; old destructures missing them** | **grep `MergeClusterResult {` in tests and update all** |
| **record_learning called directly from command code** | **Forgot to migrate a path** | **grep `record_learning(` in `src/commands/` — only writer.record should remain** |

---

## Quality Checks (REQUIRED every iteration)

```bash
cargo fmt --check
cargo check 2>&1 | tee /tmp/check.txt | tail -5 && grep "^error\|^warning" /tmp/check.txt | head -10
cargo clippy -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -5 && grep "^error" /tmp/clippy.txt | head -10
cargo test 2>&1 | tee /tmp/test-results.txt | grep "^test result" && grep "FAILED\|error\[" /tmp/test-results.txt | head -10
```

Fix any failures before committing. Never commit broken code.

**Expected test count**: ~2854 passing tests pre-refactor. After FEAT-001, expect +5 or so (new writer tests). After each FEAT task, the count should stay stable or grow.

---

## Task Files

| File                                              | Purpose                                      |
| ------------------------------------------------- | -------------------------------------------- |
| `.task-mgr/tasks/learning-writer-refactor.json`   | Task list — read tasks, mark complete        |
| `.task-mgr/tasks/learning-writer-refactor-prompt.md` | This prompt (read-only)                   |
| `.task-mgr/tasks/progress.txt`                    | Progress log — append findings and learnings |
| `.task-mgr/tasks/long-term-learnings.md`          | Curated learnings (read first if present)    |

---

## Review Task (REVIEW-001)

When you reach REVIEW-001:

1. Review ALL implementation for quality, security, and integration wiring
2. Verify every new code path is reachable from production entry points (grep `LearningWriter::new` in src/commands/ and src/learnings/ingestion/)
3. Check every acceptance criterion marked "Negative:" — these are the most common failure modes
4. Run full test suite
5. **Manual smoke tests** (if Ollama is running locally):
   ```bash
   task-mgr curate embed --status   # note count
   task-mgr learn --outcome pattern --title "writer smoke test" --content "verify writer path"
   task-mgr curate embed --status   # expect count +1
   ```
6. **Review remaining tasks**: Read progress.txt and git log. If implementation changed APIs, data structures, or assumptions, update remaining task descriptions/criteria to reflect reality.
7. **Update CLAUDE.md**: Add a "Learning creation chokepoint" section pointing at `src/learnings/crud/writer.rs` and documenting:
   - LearningWriter is the required production entry point for creating learnings
   - The tx rule: `record()` works inside a tx (pass `&tx`); `flush()` must run AFTER commit
   - `push_existing()` exists for callers that manage their own atomic multi-statement transactions (e.g., `merge_cluster`)
8. If issues found: add FIX-xxx tasks to the JSON file (priority 50-97), commit JSON
9. The loop will pick up new FIX tasks automatically

---

## Progress Report Format

APPEND to `.task-mgr/tasks/progress.txt`:

```
## [Date/Time] - [Task ID]
- What was implemented
- Files changed
- **Learnings:** (concise — patterns, gotchas, 1-2 lines each)
---
```

---

## Rules

- **One task per iteration**
- **Commit after each task**
- **Read before writing** — always read files first
- **Minimal changes** — only what's required
- Work on the correct branch: `feat/learning-writer-refactor`
