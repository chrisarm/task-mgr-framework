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

<!-- TASK_MGR:END -->

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
