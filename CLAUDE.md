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

The interactive picker fires from `task-mgr init` (project-level scaffold) and
from the deprecated `task-mgr init --from-json X` shim path when nothing
resolves and stdin+stderr are both TTYs. Non-TTY / auto-mode runs print a
one-line stderr hint and skip — no hang. The picker does NOT fire from
`task-mgr loop init` or `task-mgr batch init` directly.

## Loop CLI Cheat Sheet

- **Add a task**: `echo '{"id":"X-FIX-001","title":"...","difficulty":"medium","touchesFiles":[]}' | task-mgr add --stdin`
- **Link into milestone**: append `--depended-on-by MILESTONE-ID`
- **Mark status**: emit `<task-status>TASK-ID:done</task-status>` (also: `failed`, `skipped`, `irrelevant`, `blocked`)
- **Permission guard**: loop iterations deny Edit/Write on `tasks/*.json` via `--disallowedTools`
- **Never edit** `.task-mgr/tasks/*.json` directly — use the CLI and tags above
- **Spawn-fixup PRD targeting**: the loop now exports `TASK_MGR_ACTIVE_PREFIX` and `task-mgr add --stdin` auto-prefixes new task IDs + their `dependsOn` / `--depended-on-by` targets to the active PRD — bare IDs (`FIX-001`) and fully-prefixed IDs both work. Cross-PRD IDs (those starting with a different known PRD's prefix) are rejected with a hard error. For human invocations outside a loop (where `TASK_MGR_ACTIVE_PREFIX` may not be set), you must still disambiguate manually. When a CODE-REVIEW or MILESTONE iteration spawns ad-hoc fixup tasks (`CODE-FIX-`, `WIRE-FIX-`, `IMPL-FIX-`, `REFACTOR-N-`), the `task-mgr add --stdin` invocation MUST disambiguate the destination PRD or the entry leaks into whatever PRD JSON the CLI defaults to. Two reliable forms: (a) pass `--from-json tasks/<correct-prd>.json`, or (b) pipe `--depended-on-by <milestone-of-correct-prd>` so the prefix is unambiguous from the dependency edge. Symptom of getting it wrong: orphan `passes: false` placeholders showing up in an unrelated PRD's JSON during merge-back review (real example: this PRD's CODE-REVIEW-1 spawned `WIRE-FIX-001`/`CODE-FIX-002` into `loop-reliability.json`; the actual fix landed correctly under REFACTOR-N tasks, but the misfiled placeholders persisted as cosmetic drift).
- **PRD JSON lives in two places — sync both before re-launching a loop**: `tasks/<feature>.json` exists in BOTH the main repo AND the worktree the loop runs in. The loop reads the **worktree's** copy on each iteration ("PRD file changed, re-importing tasks..."). Editing the main-repo JSON via `Edit` + `task-mgr loop init tasks/<feature>.json --append --update-existing` from the main repo refreshes the main DB once, but the loop will silently re-import the stale worktree copy on its next iteration, undoing the edit. Reliable fix: `cp tasks/<feature>.json <worktree>/tasks/<feature>.json` before re-launching, OR edit the worktree's copy directly. Verify by jq-comparing `.userStories[] | select(.id=="X") | .acceptanceCriteria` between both locations.
- **Soft-dep guard tokenizer false-positive on AC cross-references**: `src/commands/next/selection.rs::mentioned_fixup_prefixes` tokenizes ACs on `[A-Z0-9-]` and matches `token.starts_with("{prefix}-")` for `SPAWNED_FIXUP_PREFIXES` (`REFACTOR-N`, `CODE-FIX`, `WIRE-FIX`, `IMPL-FIX`). A non-self-fixup task whose AC mentions e.g. `CODE-FIX-002` as a standalone token will be **deferred indefinitely** while any same-prefix sibling is active — even if that sibling depends on the deferred task (deadlock). Bypass: write the cross-reference as `<prd-prefix>-CODE-FIX-002` so it tokenizes as one fully-prefixed token, OR rephrase to not name the sibling at all (e.g., "the implementation task" instead of "CODE-FIX-002"). Description and notes fields are NOT scanned — ACs only. Found in unify-execution-followups; deadlocked TEST-INIT-001 ↔ CODE-FIX-002.

## Deprecation policy

`task-mgr init --from-json <prd>` is a **permanent shim** — it will not be removed. Operators who use it today (scripts, docs, muscle memory) can continue to do so indefinitely. The shim prints a one-line stderr notice and dispatches to `task-mgr loop init` (N=1) or `task-mgr batch init` (N>1) after running `init_project`, so the DB state is byte-for-byte identical to the canonical path.

Canonical forms for new scripts and docs:

| Old (deprecated, still valid) | New (canonical) |
|-------------------------------|-----------------|
| `task-mgr init --from-json prd.json` | `task-mgr init && task-mgr loop init prd.json` |
| `task-mgr init --from-json 'tasks/*.json'` | `task-mgr init && task-mgr batch init 'tasks/*.json'` |
| `task-mgr init --from-json prd.json --append --update-existing` | `task-mgr loop init prd.json --append --update-existing` |

See PRD §11 (shim permanence) for the rationale.

## Auto-launch /review-loop after loop end

After a clean loop exit (all tasks complete), `task-mgr` can spawn an interactive
`claude "/review-loop tasks/<prd>.md"` session automatically. The user lands directly
in the review without a manual hand-off step.

**Default behavior**: fires when `autoReview: true` (default) AND `tasks_completed >= autoReviewMinTasks`
(default 3). Both live in `.task-mgr/config.json`. An empty config means both defaults apply.

**CLI overrides** (clap-enforced mutual exclusion):
- `--auto-review` — force on; treats the task-count threshold as 1
- `--no-auto-review` — force off unconditionally

**Batch mode**: ONE review fires at end-of-batch for the LAST successful PRD that met the
threshold — never per-PRD. Earlier PRDs in the batch are skipped even if they individually
qualified.

**Suppression cases** (prints a recovery hint, continues, exit code unchanged):
- Non-TTY stdout (CI, pipes) — hint: re-run interactively to get the review
- `tasks/<prd>.md` not found AND `tasks/prd-<stem>.md` not found — hint: name the markdown file to match
- Worktree path missing or cleaned up — hint: re-run `claude "/review-loop tasks/<prd>.md"` manually

**Process model**: `Command::status()` — blocking spawn, stdin/stdout/stderr inherit so the
review session is fully interactive. `ANTHROPIC_API_KEY` and other env vars inherit automatically.

**Module**: `src/loop_engine/auto_review.rs` — `Decision`, `resolve_decision`, `should_fire`,
`ReviewLauncher` trait, `maybe_fire`.

**Invariant**: auto-review failure NEVER changes the loop or batch exit code.

**Known footgun — paths with whitespace**: `ProcessLauncher::launch`
(`src/loop_engine/auto_review.rs:130`) interpolates the PRD path into a single
slash-command argv element: `format!("/review-loop {}", md.display())`. Claude
re-tokenizes the slash-command body on whitespace, so a PRD path containing
spaces (e.g. `tasks/My PRD.md`) splits into multiple tokens and the review
launch fails to find the file. Not a security issue (no shell, `Command::arg`
is safe), but project convention is space-free `tasks/<feature>.md` paths for
exactly this reason — keep it that way. If the Claude CLI grows a structured
args form, prefer that over in-band quoting.

`maybe_fire` enforces this convention with a launch-boundary guard: if the
resolved markdown path contains any `char::is_whitespace` character, the
launch is suppressed and a stderr hint tells the operator to rename the file
and re-run `/review-loop` manually. The guard sits AFTER `prd_md_path` (so it
sees the actual file we'd hand to Claude) and BEFORE `launcher.launch` (so
no fragmented argv ever reaches `claude`). It deliberately does not attempt
to quote or escape — quoting Claude's slash-command body is brittle, and
suppression with a clear hint is the simpler, more honest contract.

**Outer/inner split for test reachability**: `maybe_fire` is a thin
wrapper that performs the TTY pre-check and delegates to
`maybe_fire_inner` (`pub(crate)`), which contains every launch-decision
gate (decision, worktree existence, markdown path resolution, whitespace
guard, launcher dispatch). `cargo test` runs in a non-TTY env, so a unit
test that goes through the public `maybe_fire` would short-circuit at
the TTY gate before reaching any inner gate — meaning a test asserting
"this guard suppresses launch" via `CapturingLauncher` would pass even
if the guard were deleted. Tests for inner-side gates
(`maybe_fire_inner_*`) call the inner function directly to bypass the
TTY gate and exercise the real guard logic; a single
`maybe_fire_outer_suppresses_in_non_tty` test exercises the outer
wrapper to prove the TTY gate still fires. When adding a new
launch-boundary guard, add it inside `maybe_fire_inner` and test it via
the inner — never via the outer.

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

## Iteration pipeline (shared)

Sequential (`run_iteration`) and parallel-wave (`run_slot_iteration` +
`process_slot_result`) execution paths share a single post-Claude pipeline:
`process_iteration_output` in `src/loop_engine/iteration_pipeline.rs`. The
module-level rustdoc lists the steps in order (progress logging,
`<key-decision>` extraction, `<task-status>` dispatch, completion ladder
including the `is_task_reported_already_complete` fallback, learning
extraction, bandit feedback, per-task crash tracking) and the two engine.rs
call sites (sequential at ~3204 in `run_loop`, wave at ~1166 in
`process_slot_result`).

**Why a shared pipeline**: before this unification, wave mode silently
skipped behaviors the sequential path treated as core — slot output was
never extracted for new learnings, bandit feedback never updated, and the
completion fallback didn't fire. The single-pipeline contract makes
parity-divergence a compile-time concern (any new step is added in one
place; both call sites pick it up).

**Prompt-builder companion**: `src/loop_engine/prompt/mod.rs` documents the
three-builder layout (`core` / `sequential` / `slot`) plus the main-thread
bundle rule — slot prompts must be built on the main thread before
`thread::spawn` because `rusqlite::Connection` is `!Send`. A compile-time
`Send` assertion on `SlotPromptBundle` enforces this; sections added to the
sequential prompt MUST also be wired through the wave builder so the two
paths cannot drift again.

**Out of scope for the pipeline** (kept at the call sites): wrapper-commit,
external-git reconciliation, human-review trigger, rate-limit waits,
pause-signal handling, slot merge resolution (see "Slot merge-back conflict
resolution" below).

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

Note: merge resolution is intentionally NOT part of the shared
`iteration_pipeline` (see "Iteration pipeline (shared)" above) — it requires
working-tree state owned by `run_wave_iteration`, not the per-slot
post-Claude processing block.

## Parallel-slot scheduling

Five layered defenses harden parallel-slot execution against the cascade
that produced the mw-datalake incident (a 2-slot loop whose slot-1
merge-back failed on iteration 1 with a recomputed-slot-path ENOENT,
silently kept launching new waves, and eventually diverged 22-vs-18
commits with un-merged `Cargo.lock` modifications on each side).

### 1. Slot path threading (cause-fix)

`merge_slot_branches_with_resolver` (`src/loop_engine/worktree.rs`) takes
`slot_paths: &[PathBuf]` and uses `slot_paths[0]` as slot 0's path, never
recomputing it via `compute_slot_worktree_path(project_root, branch, 0)`.
The recomputation diverges when the loop runs from inside the matching
worktree — `compute_slot_worktree_path` re-derives a path under
`{parent(project_root)}/{slot0_name}-worktrees/...` while the actual slot 0
worktree IS the project root. Engine threads the paths returned by
`ensure_slot_worktrees` through `WaveParams::slot_worktree_paths`.

`compute_slot_worktree_path` is still correct for slots 1+ inside
`merge_slot_branches_with_resolver` and for `cleanup_slot_worktrees` — only
the slot 0 lookup was wrong.

### 2. Consecutive-merge-fail halt threshold

`ProjectConfig::merge_fail_halt_threshold` (default `2`) caps consecutive
parallel-slot merge-back failure waves before the engine halts. Single
failures are recoverable (next wave gets a clean slate from the
resolver); two-in-a-row indicate a cascading state. The reset/halt
contract is implemented once in
`apply_merge_fail_reset_and_halt_check` (`src/loop_engine/engine.rs`)
and called from the wave-loop boundary — sequential-loop and wave-loop
paths must not re-implement it.

Threshold semantics:
- `0` — never halt (legacy "log and continue" behavior, preserved
  bit-for-bit on the same forced-fail input)
- `1` — halt on any merge-back failure
- `2` (default) — halt after two consecutive merge-back failure waves

### 3. Implicit-overlap baseline + buildy heuristic

`select_parallel_group` in `src/commands/next/selection.rs` serializes
shared-infra contention through a single synthetic `__shared_infra__`
slot per wave. A candidate "claims" the synthetic slot when ANY of:

- (a) some `touchesFiles` entry's basename matches the union of
  `IMPLICIT_OVERLAP_FILES` (Cargo.lock, uv.lock, package-lock.json,
  go.sum, etc. — Rust/Python/JS/Go ecosystems out-of-the-box) ∪
  `ProjectConfig::implicit_overlap_files` ∪
  `PrdFile::implicit_overlap_files` (project + PRD lists EXTEND, do not
  replace, the baseline);
- (b) the task id matches `BUILDY_TASK_PREFIXES` (`FEAT`, `REFACTOR`,
  `REFACTOR-N`, `CODE-FIX`, `WIRE-FIX`, `IMPL-FIX` — superset of
  `SPAWNED_FIXUP_PREFIXES`) via the same token-aware
  `id_body_matches_prefix` matcher used by the soft-dep guard (no
  parallel matcher);
- (c) the task's `claims_shared_infra` field (Option<bool>, migration
  v19) is `Some(true)` — explicit override.

`Some(false)` overrides BOTH (a) and (b); `None` falls through to (a) ∨
(b). This deliberately changes the empty-`touchesFiles` parallelism
baseline — buildy-prefix tasks claim infra even with no listed files.

### 4. Cross-wave file affinity (un-merged ephemeral branches)

`select_parallel_group` accepts `ephemeral_overlay: &[(branch, files)]`
listing files claimed by un-merged ephemeral slot branches from prior
waves. A candidate is deferred when its `touchesFiles` overlap with any
ephemeral branch's claimed set — preventing the same file from being
modified on two divergent branches across waves.

Engine builds the overlay via `worktree::list_unmerged_branch_files`
(`git diff --name-only {base}...{ephemeral}`) for each `{branch}-slot-N`
ephemeral that hasn't merged back. Empty overlay → identical results to
the pre-FEAT-004 implementation (strict superset filter).

**Deadlock guard**: when the greedy pass yields an empty group AND every
candidate's only overlap was ephemeral, `ParallelGroupResult::ephemeral_block_diagnostics`
is populated with named blocking branches. Engine treats this as
equivalent to `failed_merges` non-empty so the FEAT-002 reset/halt
contract fires and the loop halts cleanly with named branches instead
of spinning until stale-iteration abort.

### 5. Stale ephemeral branch hygiene at startup

`reconcile_stale_ephemeral_slots` (`src/loop_engine/worktree.rs`) runs
once at loop startup BEFORE `ensure_slot_worktrees`. For each
`{branch}-slot-N` left over from a prior crash:
- Clean (worktree dir gone, no un-merged commits) → branch deleted, no
  abort.
- Un-merged commits exist AND `halt_threshold > 0` → abort startup
  (the operator must reconcile before the new loop can run).
- Dirty working tree (uncommitted changes) → abort regardless of
  `halt_threshold` (no automated cleanup of unsaved work).

Branch-name shape uses `ephemeral_slot_branch(branch, slot)` (slot 0 is
the loop's base branch; slots 1+ are `{branch}-slot-{N}`). Idempotent —
running twice produces identical state on the second pass.

**Slot-0 SAFETY GUARD (load-bearing)**: `classify_ephemeral_branch`
returns `Err` when the parsed slot suffix is `0`, and
`list_ephemeral_slot_branches` filters `slot > 0`. Production code never
creates a `{branch}-slot-0` ref (slot 0 reuses the base branch directly
in `ensure_slot_worktrees`), but a stray ref from a buggy past version,
manual operator action, or recovery artifact would otherwise classify
as `CleanMerged` with `worktree_path` pointing at the **loop's main
worktree** — `compute_slot_worktree_path(_, branch, 0)` short-circuits
to `compute_worktree_path(_, branch)`. The downstream
`delete_merged_ephemeral` would then `git worktree remove` the loop's
running worktree. Guard MUST hold; never broaden the glob without
adding the slot==0 rejection at the same boundary.

### 6. Run-level config caching (restart required for mid-loop edits)

`ProjectConfig` and the PRD-side `implicit_overlap_files` override are
loaded ONCE at `run_loop` startup and threaded through
`WaveIterationParams` (`prd_implicit_overlap_files`, `project_config`).
`run_wave_iteration`, `apply_merge_fail_reset_and_halt_check`, and the
merge-back resolver setup all read from the cached references — never
call `read_project_config` or `read_prd_implicit_overlap_files` from
inside a wave hot path.

**Mid-loop edits to `.task-mgr/config.json` or the PRD JSON do NOT take
effect** — operators must restart the loop to apply config changes.
Same restart-required semantics every other run-scoped knob already
has (`parallel_slots`, `default_model`, `merge_resolver_*`).
Documenting this here so the next "I changed config and nothing
happened" question has a quick answer.

### 7. Failed-merge accounting: `Vec<FailedMerge>`, not parallel arrays

`WaveOutcome.failed_merges: Vec<FailedMerge>` carries `(slot, task_id)`
as a single struct so the slot/task pairing is a type-level invariant.
The earlier shape (parallel `Vec<usize>` + `Vec<Option<String>>` held
lockstep by rustdoc) was correct but implicit; mismatched lengths would
have silently truncated under `zip`. Don't reintroduce parallel arrays
here, and apply the same shape preference for any future
"slot + per-slot data" aggregation.

**Synthetic-deadlock sentinel (`SYNTHETIC_DEADLOCK_SLOT = usize::MAX`)**:
`handle_ephemeral_deadlock` inserts one entry with this slot index when
every blocking ephemeral branch had a malformed suffix
(`synth_slots.is_empty() && !diagnostics.is_empty()`). Without it,
`failed_merges` would be empty, `apply_merge_fail_reset_and_halt_check`
would reset `consecutive_merge_fail_waves` to 0, and the deadlock
guard would silently spin until the stale-iteration tracker aborted —
defeating the FEAT-002 cascade halt. The diagnostic step special-cases
the sentinel to print `<malformed deadlock blocker>` instead of
synthesizing `{branch}-slot-18446744073709551615`.

General pattern: **any synthesis that translates "we observed a
problem" into "produce a failure record" must always emit at least
one record, even if the upstream parsers all rejected the input** —
otherwise downstream "is_empty" checks invert the safety guarantee.

### Touchpoints

| Concern | File | Symbol |
| --- | --- | --- |
| Slot path threading | `src/loop_engine/worktree.rs` | `merge_slot_branches_with_resolver` |
| Halt threshold contract | `src/loop_engine/engine.rs` | `apply_merge_fail_reset_and_halt_check` |
| Failed-merge struct | `src/loop_engine/engine.rs` | `FailedMerge`, `SYNTHETIC_DEADLOCK_SLOT` |
| Implicit overlap baseline | `src/commands/next/selection.rs` | `IMPLICIT_OVERLAP_FILES`, `BUILDY_TASK_PREFIXES` |
| Cross-wave overlay | `src/loop_engine/worktree.rs` + `src/commands/next/selection.rs` | `list_unmerged_branch_files`, `ephemeral_overlay` parameter |
| Startup hygiene + slot-0 guard | `src/loop_engine/worktree.rs` | `reconcile_stale_ephemeral_slots`, `classify_ephemeral_branch` |
| Run-level config caching | `src/loop_engine/engine.rs` | `WaveIterationParams::project_config`, `prd_implicit_overlap_files` |
<!-- TASK_MGR:BEGIN -->
## task-mgr workflow

This block is managed by `task-mgr enhance` — edits inside the
`TASK_MGR:BEGIN` / `TASK_MGR:END` markers will be overwritten on the next
`task-mgr enhance agents` run. Content outside the markers is preserved.

### CLI cheat sheet

- **Look up a task**: `task-mgr show <task-id>`
- **List tasks**: `task-mgr list` (filter with `--status`, `--prefix`, `--task-type`)
- **Pick the next eligible task**: `task-mgr next` (add `--claim` to mark it
  `in_progress`)
- **Add a new task** (review fix / refactor / follow-up): pipe a single JSON
  object to `task-mgr add --stdin`. Example:

  ```sh
  echo '{"id":"CODE-FIX-001","title":"Fix race","difficulty":"medium","touchesFiles":["src/foo.rs"]}' \
    | task-mgr add --stdin
  ```

  Priority is auto-computed (top-task priority minus one). Pass
  `--depended-on-by <milestone-id>` when the new task should block a
  milestone — this is the canonical way to wire spawn-fixups into the
  correct PRD (see "Spawn-fixup PRD targeting" below).

- **Mark status from a loop iteration**: emit a `<task-status>` tag in your
  output. Recognized statuses: `done`, `failed`, `skipped`, `irrelevant`,
  `blocked`. Example: `<task-status>TASK-ID:done</task-status>`.

### Mid-loop JSON sync

When the task-list JSON changes mid-effort (adding tasks, editing
descriptions, recording human-review outcomes), NEVER run bare
`task-mgr init --from-json <prd>.json` — it wipes `status`, `started_at`,
and `completed_at` for every task in the list.

Correct incremental sync (post this PR, `task-mgr loop init` is the
canonical form; `task-mgr init --from-json` survives indefinitely as a
deprecated shim that prints a stderr notice and dispatches to the same
path):

```sh
task-mgr loop init <prd>.json --append --update-existing --dry-run  # preview
task-mgr loop init <prd>.json --append --update-existing             # apply
```

`--append --update-existing` preserves status fields on existing rows,
refreshes `description` / `acceptanceCriteria` / `notes` / `files` /
relationships, and adds any new tasks. Safe to run against an
in-progress loop.

### Human-in-the-loop CLARIFY tasks

When a task requires human sign-off (`requires_human: true`), the loop
emits `<promise>BLOCKED</promise>` until resolution. On resolution,
embed a machine-readable `humanReviewOutcome` block directly in the
JSON task entry:

```json
"humanReviewOutcome": {
  "resolvedAt": "YYYY-MM-DD",
  "resolvedBy": "<name>",
  "confirmedValues": { },
  "deltasFromProposed": [ ],
  "additionalRequirements": [ ]
}
```

Then update downstream task entries in the SAME commit — their embedded
rate-limit / threshold / flag values must match the confirmed outcome,
or the loop will implement the proposed (wrong) value. Sync with
`task-mgr loop init <prd>.json --append --update-existing`, then
`task-mgr complete <clarify-task-id>`.

### Spawn-fixup PRD targeting

When a milestone or CODE-REVIEW iteration spawns ad-hoc fixup tasks
(`CODE-FIX-`, `WIRE-FIX-`, `IMPL-FIX-`, `REFACTOR-N-`), the
`task-mgr add --stdin` invocation MUST disambiguate the destination PRD
or the entry leaks into whatever PRD JSON the CLI defaults to. Two
reliable forms:

- (a) `--from-json tasks/<correct-prd>.json` — explicit path
- (b) `--depended-on-by <milestone-of-correct-prd>` — the prefix is
  unambiguous from the dependency edge

Symptom of getting it wrong: orphan `passes: false` placeholders show
up in an unrelated PRD's JSON during merge-back review.

### Learnings, recall, decisions

- `task-mgr learn --outcome <success|failure|workaround|pattern> --title ...`
  records a learning. Auto-embedded for future recall.
- `task-mgr recall --for-task <id>` finds learnings scored for THIS task
  (file pattern + task type + error pattern matches).
- `task-mgr recall --query "<text>"` runs the vector backend (requires
  Ollama; pass `--allow-degraded` for offline runs).
- `task-mgr apply-learning <id>` and `task-mgr invalidate-learning <id>`
  feed the UCB bandit ranking.
- `task-mgr decisions list` shows pending architectural decisions;
  `task-mgr decisions resolve <id> <letter>` records the choice.

### Never edit `tasks/*.json` directly

The PRD task JSON is the source of truth for the loop engine. Editing
it by hand corrupts loop state (the engine re-imports the file on each
iteration and may revert your edit). Use the CLI subcommands above
plus the `<task-status>` tag — never `Edit` or `Write` the JSON
yourself.

## LLM coding guidelines

These guidelines apply to every code change in this repository regardless
of which agent makes them.

### 1. Think before coding

State assumptions BEFORE writing code. Identify the inputs, the
invariants, the failure modes, and the boundary between "user input"
and "trusted internal state." If a requirement is ambiguous, ask one
clarifying question; do not paper over ambiguity with defensive code.

Read the surrounding context — at minimum the file you are editing, the
direct callers of the function you are changing, and any tests that
exercise it. Code review is cheaper than a revert.

### 2. Simplicity first

Prefer the simplest change that satisfies the requirement. Three
similar lines beats a premature abstraction. A direct function call
beats a trait + impl + factory.

Do NOT add error handling, fallbacks, or validation for scenarios that
cannot happen. Trust framework guarantees. Validate at system
boundaries (user input, external APIs), not at every internal call
site. Do NOT add feature flags or backwards-compatibility shims when
you can just change the code.

### 3. Surgical changes

Change only what the task requires. A bug fix does not need
surrounding cleanup. A one-shot operation does not need a helper
function. A new feature does not need a refactor of adjacent code.

If a refactor is genuinely required, do it as a separate commit (or a
separate task) with its own justification — never bundled into an
unrelated change. Reviewers should be able to read the diff and see
exactly what behavior changed.

### 4. Goal-driven execution

Every change must trace back to an acceptance criterion. Before
committing, check: does this diff move every acceptance criterion
strictly toward "satisfied"? If a line does not, delete it.

Run the scoped quality gate before committing: the linters and tests
filter to the module you touched. Do not skip the gate to save time —
a CI failure costs more than the gate run.

<!-- TASK_MGR:END -->
