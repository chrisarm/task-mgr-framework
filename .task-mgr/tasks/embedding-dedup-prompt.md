# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Fast Dedup via Embedding Pre-Filter + Parallel Batching** for **task-mgr**.

## Problem Statement

`curate dedup` with ~1,200 active learnings makes ~60 sequential Claude subprocess calls, taking 10-30 minutes. Most calls are wasted on non-duplicates.

Fix: embed learnings locally via Ollama+Jina, cluster by cosine similarity, only send likely-duplicate clusters to the LLM. Parallelize remaining LLM calls.

---

## Priority Philosophy

1. **FUNCTIONING CODE** — Make it work
2. **CORRECTNESS** — Compiles, tests pass
3. **CODE QUALITY** — Clean, no warnings
4. **No over-engineering** — Concrete structs, no traits with one impl, no premature abstractions

**Prohibited:**
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- `unwrap()` in production paths

---

## Task Files

| File | Purpose |
|------|---------|
| `tasks/embedding-dedup.json` | Task list — read tasks, mark complete |
| `tasks/embedding-dedup-prompt.md` | This prompt (read-only) |
| `tasks/progress-{{TASK_PREFIX}}.txt` | Progress log (create if missing) |

---

## Your Task

1. Read `tasks/embedding-dedup.json`
2. Read `tasks/progress-{{TASK_PREFIX}}.txt` (create if missing)
3. Read `CLAUDE.md` for project patterns
4. Verify you're on branch `feat/embedding-dedup`
5. Select the best eligible task (highest priority with all deps met)
6. Implement it, write tests alongside
7. Run quality checks: `cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test`
8. Commit: `feat: STORY-ID-completed - [Title]`
9. Output `<completed>STORY-ID</completed>`
10. Append progress to `tasks/progress-{{TASK_PREFIX}}.txt`

---

## Reference Code

### Ollama API

```
POST /api/embed  {"model": "...", "input": "text" | ["t1", "t2"]}
Response: {"embeddings": [[...f32...]]}

GET /api/tags
Response: {"models": [{"name": "...", ...}]}
```

Default model: `hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0`
Default URL: `http://localhost:11434`
Dimensions: 1024

Reference impl: `$HOME/projects/external-ref/service/src/agent/kb/embedder.rs:82-145`

### Data Flow Contracts

```rust
// Ollama response -> Vec<f32>
let embedding: Vec<f32> = body["embeddings"][0]
    .as_array().ok_or(/* error */)?
    .iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();

// Vec<f32> -> BLOB
let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

// BLOB -> Vec<f32>
let embedding: Vec<f32> = blob.chunks_exact(4)
    .map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
```

### Existing Patterns

- `ProjectConfig`: `src/loop_engine/project_config.rs` — extend with `#[serde(default)]` fields
- `CurateAction` enum: `src/cli/commands.rs:956` — add Embed variant
- `curate_dedup`: `src/commands/curate/mod.rs:517` — sequential loop to replace
- `spawn_claude`: `src/loop_engine/claude.rs:79` — subprocess pattern
- Migration pattern: `src/db/migrations/v14.rs` — follow for v15

---

## Quality Checks (REQUIRED)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

Fix any failures before committing.

---

## Review Tasks

CODE-REVIEW-1 **can add tasks** to the JSON. For each issue found, add a CODE-FIX-xxx task (priority 11-14), add it to VERIFY-001's dependsOn, and commit the JSON.

---

## Stop and Blocked

All tasks `passes: true` and milestones pass → `<promise>COMPLETE</promise>`
Blocked → document in progress file, output `<promise>BLOCKED</promise>`

---

## Important Rules

- **ONE story per iteration**
- **Commit after each passing story**
- **Read before writing**
- **Minimal changes** — only what's required
- **No trait for OllamaEmbedder** — concrete struct, extract trait only if second provider is ever needed
