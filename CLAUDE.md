# task-mgr Project Notes

## Project layout

- Database: `.task-mgr/tasks.db` (per worktree)
- Main worktree: `$HOME/projects/task-mgr`
- Feature worktrees: `$HOME/projects/task-mgr-worktrees/<branch-name>/`
- PRD task lists: `.task-mgr/tasks/<prd-name>.json`
- Loop prompts: `.task-mgr/tasks/<prd-name>-prompt.md`
- Progress log: `.task-mgr/tasks/progress-<prefix>.txt` (gitignored — managed by `task-mgr init`)

## Subsystem design notes

Module-level CLAUDE.md files (auto-loaded when files in the module are read):

- [`src/lifecycle/CLAUDE.md`](src/lifecycle/CLAUDE.md) — status mutation SSoT, six lifecycle verbs, five hard invariants, FR-006 site→verb mapping table
- [`src/loop_engine/CLAUDE.md`](src/loop_engine/CLAUDE.md) — overflow recovery, auto-review, parallel slots, merge-back conflict resolution, shared iteration pipeline
- [`src/commands/curate/CLAUDE.md`](src/commands/curate/CLAUDE.md) — Ollama embeddings, reranker, dedup dismissals, session cleanup
- [`src/commands/next/CLAUDE.md`](src/commands/next/CLAUDE.md) — soft-dep guard for milestone scheduling
- [`src/learnings/CLAUDE.md`](src/learnings/CLAUDE.md) — LearningWriter chokepoint, supersession, recall scoring

File-scoped invariants are stored as learnings and surface via
`task-mgr recall --for-task <id>` — they don't need to be in this file.

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

## Deprecation policy

`task-mgr init --from-json <prd>` is a **permanent shim** — it will not be removed. Operators who use it today (scripts, docs, muscle memory) can continue to do so indefinitely. The shim prints a one-line stderr notice and dispatches to `task-mgr loop init` (N=1) or `task-mgr batch init` (N>1) after running `init_project`, so the DB state is byte-for-byte identical to the canonical path.

Canonical forms for new scripts and docs:

| Old (deprecated, still valid) | New (canonical) |
|-------------------------------|-----------------|
| `task-mgr init --from-json prd.json` | `task-mgr init && task-mgr loop init prd.json` |
| `task-mgr init --from-json 'tasks/*.json'` | `task-mgr init && task-mgr batch init 'tasks/*.json'` |
| `task-mgr init --from-json prd.json --append --update-existing` | `task-mgr loop init prd.json --append --update-existing` |

See PRD §11 (shim permanence) for the rationale.

**Current recommended process (2026)**: For most work, use plan mode + `/spike` (when uncertainty or a multi-impact abstraction exists) followed by `/plan-tasks` (lean) or light `/tasks`. Full `/prd` + heavy `/tasks` is for large efforts. See the managed "task-mgr workflow" block below for the detailed cheat sheet and flow.

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
  `--depended-on-by CONTRACT-001` (or the PRD's final milestone) when the
  new task implements against a contract defined earlier.

- **Mark status from a loop iteration**: emit a `<task-status>` tag in your
  output. Recognized statuses: `done`, `failed`, `skipped`, `irrelevant`,
  `blocked`. Example: `<task-status>TASK-ID:done</task-status>`.

### Recommended planning & PRD flow (2026 update)

- **Default for most work**: short plan-mode interview → `/spike "risky area or multi-impact abstraction"` (when the riskiest assumption or a contract used by 2+ stories is unclear) → `/plan-tasks` (lean) or light `/tasks`.
- `/spike` owns exploration, thin vertical slice, 2-3 approaches, and (when warranted) emits a `CONTRACT-xxx` task for foundational abstractions.
- Full `/prd "..."` + heavy `/tasks` is reserved for large cross-subsystem efforts. The expensive design critique now lives in the spike.
- PRDs contain a new **2.6 Boundary Contracts & Modularity Targets** section. This is the source for `CONTRACT-xxx` tasks (`taskType: "contract"`).
- Wire dependents with `--depended-on-by CONTRACT-001` (or the PRD milestone). The contract text lives in the progress log.

### Mid-loop JSON sync

When the task-list JSON changes mid-effort (adding tasks, editing
descriptions, recording human-review outcomes), NEVER run bare
`task-mgr init --from-json <prd>.json` — it wipes `status`, `started_at`,
and `completed_at` for every task in the list.

Correct incremental sync (`task-mgr loop init` is the canonical form):

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
- (b) `--depended-on-by CONTRACT-001` (or the PRD’s final milestone) — the canonical way to attach a fixup or implementation task to a contract or milestone of the correct PRD. The prefix is unambiguous from the dependency edge.

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


### Command Reference (generated)

| Command | Description |
| --- | --- |
| `task-mgr init` | Initialize project (`.task-mgr/`) and/or import PRD JSON files |
| `task-mgr list` | List tasks with optional filtering |
| `task-mgr show` | Show detailed information about a single task |
| `task-mgr next` | Get the next recommended task to work on |
| `task-mgr complete` | Mark one or more tasks as completed |
| `task-mgr fail` | Mark one or more tasks as failed (blocked, skipped, or irrelevant) |
| `task-mgr run` | Run lifecycle management (begin, update, end) |
| `task-mgr run begin` | Begin a new run session |
| `task-mgr run update` | Update an active run with progress information |
| `task-mgr run end` | End a run session |
| `task-mgr export` | Export database state to JSON |
| `task-mgr doctor` | Check database health and fix stale state |
| `task-mgr skip` | Skip one or more tasks intentionally (defer for later without marking as failed) |
| `task-mgr irrelevant` | Mark one or more tasks as irrelevant (no longer needed due to changed requireme… |
| `task-mgr learn` | Record a learning from a task outcome |
| `task-mgr recall` | Find relevant learnings for a task or query |
| `task-mgr learnings` | List all learnings |
| `task-mgr apply-learning` | Record that a learning was applied (confirmed useful) |
| `task-mgr invalidate-learning` | Invalidate a learning via two-step degradation |
| `task-mgr unblock` | Return a blocked task to todo status for retry |
| `task-mgr unskip` | Return a skipped task to todo status for retry |
| `task-mgr add` | Add a single task from JSON (stdin or --json) |
| `task-mgr reset` | Reset task(s) to todo status for re-running |
| `task-mgr stats` | Show progress summary (task counts, completion rate, learnings) |
| `task-mgr history` | Show run history |
| `task-mgr delete-learning` | Delete a learning from the database |
| `task-mgr edit-learning` | Edit an existing learning |
| `task-mgr review` | Review blocked and skipped tasks |
| `task-mgr migrate` | Manage database schema migrations |
| `task-mgr migrate status` | Show current migration status |
| `task-mgr migrate up` | Apply the next pending migration |
| `task-mgr migrate down` | Revert the most recent migration |
| `task-mgr migrate all` | Apply all pending migrations (default behavior on db open) |
| `task-mgr completions` | Generate shell completions |
| `task-mgr loop` | Run autonomous agent loop |
| `task-mgr loop init` | Initialize a database from a single PRD JSON file |
| `task-mgr loop run` | Run the autonomous agent loop against a PRD |
| `task-mgr status` | Show status dashboard for PRD projects |
| `task-mgr batch` | Run multiple PRDs in sequence |
| `task-mgr batch init` | Initialize a database from multiple PRD JSON files (one per glob match) |
| `task-mgr batch run` | Run the autonomous agent loop across multiple PRDs |
| `task-mgr import-learnings` | Import learnings from a progress.json or learnings JSON file |
| `task-mgr archive` | Archive completed PRDs and extract learnings |
| `task-mgr extract-learnings` | Extract learnings from a Claude output file using LLM analysis |
| `task-mgr worktrees` | Manage git worktrees (list, prune, remove) |
| `task-mgr worktrees list` | List all git worktrees with branch, path, and lock status |
| `task-mgr worktrees prune` | Remove unlocked worktrees (skips locked and dirty) |
| `task-mgr worktrees remove` | Remove a specific worktree by path or branch name |
| `task-mgr man-pages` | Generate man pages for task-mgr and all subcommands |
| `task-mgr curate` | Curate learnings (retire stale entries, unretire archived ones) |
| `task-mgr curate retire` | Identify and soft-archive stale learnings |
| `task-mgr curate unretire` | Restore soft-archived learnings by ID |
| `task-mgr curate dedup` | Identify and merge duplicate learnings using LLM semantic analysis |
| `task-mgr curate enrich` | Enrich learning metadata using LLM analysis |
| `task-mgr curate embed` | Generate and store Ollama embeddings for active learnings |
| `task-mgr curate count` | Show learning statistics: total, active, retired, and embedded counts |
| `task-mgr decisions` | Manage key architectural decisions |
| `task-mgr decisions list` | List key decisions (pending and deferred by default) |
| `task-mgr decisions resolve` | Resolve a key decision by selecting an option |
| `task-mgr decisions decline` | Decline a key decision (mark as not needed) |
| `task-mgr decisions revert` | Revert a resolved or deferred decision back to pending |
| `task-mgr models` | List Claude models and pin a default |
| `task-mgr models list` | Print the available model IDs |
| `task-mgr models set-default` | Pin a default model (user config by default, `--project` for project) |
| `task-mgr models unset-default` | Clear the pinned default model |
| `task-mgr models show` | Show the currently resolved default model and where it came from |
| `task-mgr current` | Show the currently resolved active PRD context (prefix, source, target path) |
| `task-mgr enhance` | Manage the task-mgr-fenced block in CLAUDE.md / AGENTS.md |
| `task-mgr enhance agents` | Write or update the marker-fenced workflow block in target files |
| `task-mgr enhance show` | Render the chosen profile to stdout. Never writes to disk |
| `task-mgr enhance strip` | Remove the marker block (and its markers) from target files |
| `task-mgr cheatsheet` | Print the curated cheat sheet + clap-generated command reference |
| `task-mgr how` | Map a natural-language intent to canonical task-mgr commands |
<!-- TASK_MGR:END -->

### Fallback runner config (Grok)

The loop engine supports a Grok CLI fallback that promotes a stuck task off
Claude after the overflow ladder is exhausted (rung 4) or after repeated
`RuntimeError` crashes at the Opus ceiling. Disabled by default; opt-in via
`.task-mgr/config.json`:

```json
{
  "version": 1,
  "fallbackRunner": {
    "enabled": true,
    "provider": "grok",
    "model": "grok-build",
    "cliBinary": "/usr/local/bin/grok",
    "runtimeErrorThreshold": 2
  }
}
```

Field defaults: `enabled=false`, `provider="grok"`, `model="grok-build"`,
`cliBinary=null` (resolves bare `grok` on PATH), `runtimeErrorThreshold=2`.
With the block absent or `enabled:false`, loop behavior is byte-identical
to the pure-Claude 4-rung overflow ladder ending in `Blocked`.

`task-mgr loop run` performs a startup binary check: when `enabled=true`,
the configured `cliBinary` (or bare `grok` on PATH) must resolve to an
existing file or the loop exits with a helpful error before the first
iteration. Subsystem design notes are in
[`src/loop_engine/CLAUDE.md`](src/loop_engine/CLAUDE.md) — see "Overflow
recovery and diagnostics" for the 5-rung ladder, "Operator escape valve" for
the override-invalidation contract, and "Provider routing" for the
token-equality classification algorithm.

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
