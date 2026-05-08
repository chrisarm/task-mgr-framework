# task-mgr Project Notes

## Database Location

The Ralph loop database is at `.task-mgr/tasks.db` (relative to the project/worktree root). Each worktree has its own copy.

## Worktrees

- Main: `$HOME/projects/task-mgr`
- Feature worktrees: `$HOME/projects/task-mgr-worktrees/<branch-name>/`

## Task Files

- PRD task lists: `.task-mgr/tasks/<prd-name>.json`
- Loop prompts: `.task-mgr/tasks/<prd-name>-prompt.md`
- Progress log: `.task-mgr/tasks/progress.txt`

## Model IDs and Effort Mapping

All Claude model IDs and the difficulty→effort mapping live in a single file:
`src/loop_engine/model.rs` (`OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL` constants
and the `EFFORT_FOR_DIFFICULTY` table). After bumping a value there:

```sh
cargo run --bin gen-docs   # regenerates the MODELS block in .claude/commands/tasks.md
```

CI runs `cargo run --bin gen-docs -- --check` which fails if the doc is stale.
Tests import the constants; JSON fixtures use `{{OPUS_MODEL}}` placeholders in
`tests/fixtures/*.json.tmpl` rendered at load time by
`tests/common/mod.rs::render_fixture_tmpl`. A regression test
(`tests/no_hardcoded_models.rs`) ensures literal model strings don't creep back
in outside `model.rs`.

## `task-mgr models` subcommand

List and pin Claude models:

```sh
task-mgr models list                     # offline — built-in model IDs + effort table
task-mgr models list --remote            # live /v1/models (requires both env vars below)
task-mgr models list --refresh           # busts cache before fetch; implies --remote
task-mgr models set-default [<model>]    # prompts interactively when model omitted
task-mgr models set-default <id> --project   # writes .task-mgr/config.json instead
task-mgr models unset-default [--project]
task-mgr models show                     # resolved default + source label
```

**Remote opt-in** (prevents surprise HTTP calls on a globally-exported SDK key):

- `ANTHROPIC_API_KEY` — your Anthropic API key
- `TASK_MGR_USE_API=1` — explicit opt-in; both must be set or we silently fall
  back to the built-in list

Cache: `$XDG_CACHE_HOME/task-mgr/models-cache.json` (24h TTL, stale treated as miss).

**Config locations & precedence** (highest to lowest): explicit task `model` →
`difficulty==high` → PRD `defaultModel` → `.task-mgr/config.json defaultModel`
→ `$XDG_CONFIG_HOME/task-mgr/config.json defaultModel` → none.
`difficulty==high` always escalates to `OPUS_MODEL`, independent of any
default.

The interactive picker fires from `task-mgr init` when nothing resolves and
stdin+stderr are both TTYs. Non-TTY / auto-mode runs print a one-line stderr
hint and skip — no hang.

## Loop CLI Cheat Sheet

- **Add a task**: `echo '{"id":"X-FIX-001","title":"...","difficulty":"medium","touchesFiles":[]}' | task-mgr add --stdin`
- **Link into milestone**: append `--depended-on-by MILESTONE-ID`
- **Mark status**: emit `<task-status>TASK-ID:done</task-status>` (also: `failed`, `skipped`, `irrelevant`, `blocked`)
- **Permission guard**: loop iterations deny Edit/Write on `tasks/*.json` via `--disallowedTools`
- **Never edit** `.task-mgr/tasks/*.json` directly — use the CLI and tags above

## Overflow recovery and diagnostics

When the Claude CLI subprocess returns "Prompt is too long", the loop engine
walks a **four-rung recovery ladder** and writes a diagnostics bundle. Entry
point: `overflow::handle_prompt_too_long` in `src/loop_engine/overflow.rs`,
called from the `PromptTooLong` arm of `run_iteration` in
`src/loop_engine/engine.rs`.

**The ladder** (in order; first rung whose precondition is met wins):

1. **Downgrade effort** — `model::downgrade_effort` (`xhigh → high`). Effort
   never drops below `high` (see `escalate_below_opus` rustdoc on the high-effort
   floor invariant).
2. **Escalate model below Opus** — `model::escalate_below_opus`
   (`haiku → sonnet`, `sonnet → opus`). Closes the Sonnet-default gap that
   used to immediately block the loop on iteration 1.
3. **Escalate to 1M-context Opus** — `model::to_1m_model` (`opus → opus[1m]`).
4. **Block** — task status set to `blocked`; no further recovery attempts.

Rungs 1-3 reset the task status to `todo` (and clear `started_at`) so the next
iteration retries with the override applied; rung 4 sets `blocked`.

**Diagnostics bundle (best-effort; failures log via `eprintln!` and never
propagate)**:

- **Prompt dump**: written to
  `.task-mgr/overflow-dumps/<sanitized-task-id>-iter<n>-<unix-ts>.txt`. Contains
  metadata + per-section byte breakdown + dropped sections + the raw assembled
  prompt. Task IDs are sanitized via `overflow::sanitize_id_for_filename`
  (path-traversal defense; `..` collapsed before allowlist filtering).
- **JSONL event log**: appended one-line-per-event to
  `.task-mgr/overflow-events.jsonl`. Each line is a serialized
  `OverflowEvent` (`ts`, `task_id`, `run_id`, `iteration`, `model`, `effort`,
  `prompt_bytes`, `sections`, `dropped_sections`, `recovery`, `dump_path`).
  `sections` is an ordered JSON array of `[name, size]` pairs (NOT a map).
  `recovery` is a tagged object with discriminator field `action` and
  variant-specific siblings (e.g. `{"action": "escalate_model", "new_model": "..."}`).
- **Rotation**: keeps newest 3 dumps per task ID via
  `overflow::rotate_dumps_keep_n`. Each entry (unreadable dir entry, missing
  metadata, failed deletion) is logged and skipped independently so a single
  IO error never aborts the rest of the rotation pass.

**Banner annotation**: when a task is mid-recovery, the iteration banner emits
`(overflow recovery from <original-model>)` next to the model line. The banner
gates on `IterationContext::overflow_recovered` (a `HashSet<String>` of task
IDs that have hit the overflow handler at least once), NOT on `model_overrides`
— see learning #893: crash escalation and consecutive-failure escalation must
stay in their own channels. The original model is captured first-overflow only
via `IterationContext::overflow_original_model.entry().or_insert_with(...)`.

**Order of operations is contractual** (do not reorder):
ctx update → DB UPDATE → stderr → dump → JSONL → rotate. Recovery state must
be durable before any best-effort observability writes.

## Learning Creation Chokepoint

All production code paths that create learnings must go through `LearningWriter` in
`src/learnings/crud/writer.rs`. This ensures every new learning automatically gets an
Ollama embedding scheduled (best-effort, graceful degradation when Ollama is down).

**Pattern:**
1. Construct `LearningWriter::new(db_dir)` — pass `Some(path)` for embedding, `None` in tests.
2. Call `writer.record(conn, params)` (or `writer.push_existing(id, title, content)` for
   callers like `merge_cluster` that do their own `record_learning` inside a transaction).
3. Call `writer.flush(conn)` **after** any enclosing transaction has committed — this is
   where the Ollama HTTP call happens. Never flush inside a `rusqlite::Transaction`.

**Production paths using LearningWriter:**
- `learn()` in `src/commands/learn.rs`
- `import_learnings()` in `src/commands/import_learnings/mod.rs`
- `curate_dedup()` in `src/commands/curate/mod.rs` (via `push_existing` after `merge_cluster`)
- `extract_learnings_from_output()` in `src/learnings/ingestion/mod.rs` (loop engine path)

The low-level `record_learning()` primitive in `src/learnings/crud/create.rs` is still
public for tests and `curate enrich`, but new production creation paths should use
`LearningWriter` to get automatic embedding scheduling.

## Learning Supersession

When a newer learning replaces an older one, the link is tracked in the
`learning_supersessions` join table (migration v17). The old row is retained (for
audit / history) but auto-filtered from recall by default.

- **Create a supersession**: `task-mgr learn --supersedes <old-id> ...` or
  `task-mgr edit-learning <new-id> --supersedes <old-id>`. The old learning's
  confidence is downgraded to `low` and a row is inserted into
  `learning_supersessions(old_id, new_id, superseded_at)`.
- **Recall behavior**: `task-mgr recall` excludes superseded learnings by default.
  Pass `--include-superseded` to see them. Filtering happens in
  `retrieval/mod.rs::passes_query_filters()` via a shared SQL helper — all three
  backends (fts5, patterns, vector) honor the flag.
- **Listing**: `task-mgr learnings` annotates rows with `(superseded by #N)` and
  `(supersedes #M)`.
- See `task-mgr learn --help`, `task-mgr edit-learning --help`,
  `task-mgr recall --help` for flag details.

**Invariants for future maintainers:**

- **`apply_supersession` runs AFTER `LearningWriter::flush`** in `learn()` — the
  new learning's `id` is only known post-insert. In `edit_learning()` the id is
  known upfront so `apply_supersession` can run before/after other field edits;
  it runs after so typo'd `--supersedes` values don't roll back unrelated edits.
- **Single source for the filter SQL**: `pub(crate) const SUPERSESSION_SUBQUERY`
  in `src/learnings/retrieval/mod.rs` is the canonical `NOT IN (SELECT
  old_learning_id FROM learning_supersessions)` fragment. All retrieval call
  sites (`fts5::execute_fts5_query`, `fts5::execute_like_query`,
  `fts5::execute_unfiltered_query`, `patterns::load_learnings_with_applicability`,
  `recall::load_ucb_fallback`) must format this const into their WHERE clauses
  alongside — never replacing — the existing `retired_at IS NULL` filter.
- **Vector backend filters in Rust, not SQL**: `vector.rs` loads embeddings
  directly, so supersession is enforced via `load_superseded_ids()` +
  `HashSet::contains` after the retrieval. Keep the two paths in sync when
  changing filter semantics.
- **Tests that touch `learning_supersessions` need `setup_db_with_migrations()`**
  — the plain `setup_db()` calls `create_schema()` only, which stops at v0.

## Recall Score Output

`task-mgr --format json recall` returns numeric scores alongside the categorical
`confidence` field so consumers can parse signal strength:

- `relevance_score` — raw retrieval score (FTS5 BM25, pattern-match points, or
  vector cosine similarity, depending on backend)
- `ucb_score` — UCB1 bandit score (present on `--for-task` queries)
- `combined_score` — aggregated ranking score used for ordering
- `match_reason` — human-readable explanation (e.g. `"FTS5 text match"`,
  `"file pattern match, task type match"`)

The underlying `recall_learnings()` / `recall_learnings_with_backend()` signatures
are unchanged; scored output flows through `recall_learnings_scored()` and the
existing CLI formatters.

## Embedding / Ollama Configuration

`curate embed` generates local embeddings via Ollama for the dedup pre-filter. Configure in `.task-mgr/config.json`:

```json
{
  "ollamaUrl": "http://localhost:11435",
  "embeddingModel": "hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0"
}
```

- **Default URL**: `http://localhost:11435` (the bundled docker-compose stack
  remaps to 11435 to avoid clashing with a host-installed `ollama serve` on the
  upstream-default 11434)
- **Default model**: `hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0` (1024 dimensions)
- **Schema**: Migration v15 adds `learning_embeddings` table (BLOB storage, little-endian f32)

### Graceful Degradation

- `curate dedup` works without Ollama — falls back to standard batch sizing when no embeddings exist
- `curate embed --status` only queries the DB (no Ollama connection needed)
- `curate embed` returns a clear error if Ollama is unreachable or the model is missing
- `recall --query <text>` HARD-FAILS by default if Ollama is unreachable. Pass
  `--allow-degraded` to fall back to silently-empty vector results (useful for
  offline runs). `recall --for-task <id>` (no `--query`) does not need Ollama.

### Reranker (optional)

The recall pipeline can layer a cross-encoder reranker on top of the per-backend
union slate. Reranking only fires for `recall --query <text>` (with or without
`--for-task`); `--for-task` alone runs the today's UCB-only pipeline.

Configure in `.task-mgr/config.json`:

```json
{
  "rerankerUrl": "http://localhost:8080",
  "rerankerModel": "jina-reranker-v2-base-multilingual",
  "rerankerOverFetch": 3
}
```

- **`rerankerUrl`** — base URL of a [gpustack/llama-box](https://github.com/gpustack/llama-box)
  server exposing OpenAI-compatible `/v1/rerank`. Reranker is disabled when unset.
- **`rerankerModel`** — model name passed in the `model` field of the rerank
  request. Required alongside `rerankerUrl`; either-or disables rerank.
- **`rerankerOverFetch`** — per-backend over-fetch factor. Slate size is
  `min(limit * over_fetch, 30)`. Default `3`. Higher = better recall headroom,
  longer rerank latency.
- **Example llama-box invocation** (CPU, port 8080):

  ```sh
  llama-box --rerank-only --port 8080 \
      --model /models/jina-reranker-v2-base-multilingual.gguf
  ```

  See `docker/docker-compose.yml` for a full Docker setup that bundles Ollama
  embeddings + llama-box rerank, GPU-by-default with a `--profile cpu` fallback.

#### Soft-fail asymmetry

The reranker is a quality booster, not a correctness primitive: when the
server is unreachable, recall emits a `[warn]` line to stderr and returns the
un-reranked candidates with their original BM25/cosine/pattern scores. Recall
still exits `0`. Contrast with Ollama, which by default hard-fails because the
vector backend is part of the recall result, not just an ordering heuristic.

#### `--query "X" --for-task Y` interaction

When both are set:
1. Per-backend top-N union slate is fetched (FEAT-003's `retrieve_for_rerank`).
2. Cross-encoder reranks the slate by `(query, candidate)` similarity.
3. UCB tiebreaks within ±0.05 rerank-score bands (same band → higher UCB wins).
4. Slate is truncated to `--limit`.

`--for-task` alone (no `--query`) skips steps 1-3 entirely; the reranker is
NOT consulted.

## Dedup Dismissal Memory

`curate dedup` persists pairs the LLM has already examined and found distinct in the
`dedup_dismissals` table (migration v18: composite PK `(id_lo, id_hi)` **plus
`CHECK (id_lo < id_hi)`** for defense-in-depth, plus `idx_dedup_dismissals_hi`).
Subsequent runs skip batches whose every C(N,2) pair is already dismissed, so
users don't re-pay LLM calls for the same "no duplicates" output.

- **Pair normalization**: `normalize_pair()` canonicalizes `(a, b)` to `(min, max)`;
  all writes go through `record_dismissals()`. The v18 CHECK constraint backstops
  this at the schema level — a self-pair or reversed pair that slipped past Rust
  normalization fails at INSERT time rather than silently corrupting the table.
- **Narrow conflict suppression**: `record_dismissals` uses
  `ON CONFLICT (id_lo, id_hi) DO NOTHING`, **not** `INSERT OR IGNORE`. This keeps
  duplicates idempotent while letting CHECK (or any future NOT NULL / FK) failures
  propagate as real errors instead of being swallowed.
- **Multi-row INSERT**: `record_dismissals` emits a single
  `INSERT ... VALUES (?,?),(?,?),...` per chunk of 256 pairs (512 params,
  well under `SQLITE_MAX_VARIABLE_NUMBER`). One round-trip per chunk, not per pair.
- **When dismissals are recorded**: after a successful LLM batch, every C(N,2) pair
  from the batch minus (a) pairs the LLM grouped as duplicates and (b) pairs whose
  IDs were retired by a strictly earlier batch.
- **Merge-map rewrite**: when the batch itself merges sources `{A,B}→N`, recorded
  pairs are rewritten via a per-batch `merge_map` so retired source IDs become the
  surviving merged ID. `(A,C)+(B,C)` collapse to `(N,C)`; two clusters in one batch
  `{A,B}→N1, {C,D}→N2` collapse the four cross-pairs to a single `(N1,N2)`. Without
  this rewrite the dismissals would point at retired (inert) rows and the next run
  would re-call the LLM on `(N, survivor)` pairs the LLM has effectively already
  judged. Logic lives in `compute_dismissal_pairs()` in `src/commands/curate/mod.rs`.
- **When they are NOT recorded**: `dry_run=true` (read-only convention) OR the batch
  raised an LLM error (can't trust a batch whose result we never got). The
  `continue` in the LLM error arm short-circuits before any dismissal accounting.
- **Forcing re-examination**: `task-mgr curate dedup --reset-dismissals` clears the
  table (`clear_dismissals()`) before the run; applies even with `--dry-run` because
  a reset is an administrative action, not an LLM pass.
- **`DedupResult.clusters_skipped`**: serde `default = 0` so JSON consumers parsing
  older output still work; new runs populate it with the count of batches skipped.
- Table has no foreign keys to `learnings` — rows for retired learnings are inert
  and harmless (they just never match an active cluster).

Helpers live in `src/commands/curate/mod.rs` as `pub(crate)` (not exported outside
the crate): `load_dismissals`, `record_dismissals`, `clear_dismissals`,
`is_fully_dismissed`, `compute_dismissal_pairs`, plus the private `normalize_pair`
/ `unordered_pairs`.

## Curate session cleanup workaround

Claude Code 2.1.110 writes an `ai-title` jsonl to `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`
even with `--no-session-persistence`. To avoid polluting the user's projects dir, `curate dedup`
and `curate enrich` opt into `spawn_claude`'s `cleanup_title_artifact` arg: a fixed UUID is
passed via `--session-id` (before `-p`, required — Claude parses flags only left of the prompt)
and, after `child.wait()` returns, that exact file is removed synchronously. An earlier detached
30s-delay thread design was replaced because threads die when the parent `task-mgr` process
exits; synchronous post-wait cleanup is both simpler and guaranteed to run. Scope is narrow —
loops and learning ingestion do NOT opt in; only the curate call sites do. See `spawn_claude`
and `cleanup_title_artifact_sync` in `src/loop_engine/claude.rs`.

## Soft-dep guard for milestone scheduling

`build_scored_candidates` in `src/commands/next/selection.rs` applies a **soft-dep
filter** after the formal `dependsOn` check. It defers any candidate whose
acceptance criteria reference a known spawned-fixup prefix
(`SPAWNED_FIXUP_PREFIXES = ["REFACTOR-N", "CODE-FIX", "WIRE-FIX", "IMPL-FIX"]`)
while a same-prefix `todo`/`in_progress` sibling still exists in the same PRD.
Defends against forgotten `--depended-on-by <milestone-id>` edges when the loop
spawns ad-hoc fixups in response to a milestone's AC text.

**Invariants for future maintainers:**

- **Token-aware exact-prefix matching, never loose substring**:
  `id_body_matches_prefix` requires the `{prefix}-` boundary at start-of-id OR
  after a `-`. Bare `id.contains("CODE-FIX")` would false-match `CODE-FIXTURE-1`
  — that's the regression the trailing dash exists to prevent.
- **AC writing convention**: the filter tokenizes acceptance-criteria text on
  non-`[A-Z0-9-]` chars and matches `token.starts_with("{prefix}-")`. Tokens
  must start with the bare prefix — an AC that writes a fully task-prefixed form
  like `cbd7d081-REFACTOR-N-xxx` tokenizes as one token starting with
  `cbd7d081-` and **silently bypasses** the guard. PRD authors who want the
  guard to fire should write the prefix as a standalone token (`REFACTOR-N-xxx`,
  `CODE-FIX-xxx`, etc.) — typically inside a parenthetical or slash-list as in
  `"Any spawned CODE-FIX/WIRE-FIX/IMPL-FIX/REFACTOR-N tasks have passes=true"`.
- **Self-fixup short-circuit**: `task_is_self_fixup` returns early so a
  `REFACTOR-N-001` candidate whose own AC mentions `REFACTOR-N-xxx` is never
  blocked. Sibling fixups remain co-schedulable across slots — this is the
  primary reason the guard fires only on milestone-class candidates.
- **`task_prefix` threading is mandatory**: `get_active_task_ids` mirrors
  `get_completed_task_ids` exactly — `prefix_and` clause + `archived_at IS NULL`.
  Omitting either is a known regression source: the prefix scoping is the only
  defense against PRD-A's milestone being blocked by PRD-B's active fixup, and
  archived rows must never block (they're inert).
- **`SPAWNED_FIXUP_PREFIXES` is the sole expansion point**: adding a new
  ad-hoc-spawn task type (e.g. `PERF-FIX`) requires extending this slice;
  `mentioned_fixup_prefixes` and `find_active_blockers_for_prefixes` iterate
  it directly, no other registration needed.

**Operator visibility**: a single `eprintln!` per deferred candidate
(`"Deferring <id>: AC references active fixup task(s): <sorted blocker IDs>"`)
fires at the filter site — not per-blocker, not per-AC-line. Sort order in
the message is stable for grep friendliness.

**Companion prompt-side teaching** (`src/loop_engine/prompt_sections/task_ops.rs`):
the loop agent is taught to pass `--depended-on-by <milestone-id>` when spawning
a fix in response to a milestone's AC. The selection-side guard is the catch;
the prompt-side teaching is the cause-fix. Both layers ship together by design
— neither is sufficient alone.

## Slot merge-back conflict resolution

When parallel-slot waves finish, `merge_slot_branches_with_resolver` (in
`src/loop_engine/worktree.rs`) runs `git merge --no-edit` from slot 0 for each ephemeral
slot branch. On a non-zero exit it lists the conflicted files and invokes a `MergeResolver`
(callback seam, `pub(crate) trait`); the engine wires `ClaudeMergeResolver` from
`src/loop_engine/merge_resolver.rs`, which spawns Claude in slot 0's already-conflicted
worktree (`PermissionMode::Auto`, `working_dir = slot0_path`, 600s timeout) with a prompt
that explicitly prohibits push, branch deletion, hard reset outside the merge, and history
rewrites. The resolver's `Resolved` claim is **never trusted**: the caller re-inspects
MERGE_HEAD and HEAD post-spawn and downgrades a lying resolver to `failed_slots` with a
forced `git reset --hard pre_merge_head`. `SlotFailureKind::ResolverAttempted` vs
`PreResolver` lets engine.rs pick the right warning text without string-sniffing.
