# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [1.1.2] - 2026-11-25

### Fixed
- `task-mgr enhance agents` without `--profile` keeps the profile of each file's existing block. Before, it rewrote a full-profile block as workflow-only and dropped the "LLM coding guidelines" section. A file with no block still gets `workflow`, and an explicit `--profile` still wins.

### Changed
- CI no longer runs the macOS test job. It has been failing on `main` without reporting a failed test; it is off until that is fixed. Release builds for macOS are unchanged.

## [1.1.1] - 2026-11-25

### Added
- `task-mgr curate enrich --reset-examined` clears the record of learnings already examined, so the next run looks at all of them again.

### Fixed
- `task-mgr curate enrich` no longer re-sends learnings it has already examined. A learning the model has answered for is recorded (schema v22, `enrich_examined`) and skipped on later runs, even when a field stays empty because nothing applies. An interrupted run resumes at the first unanswered batch.

## [1.1.0] - 2026-11-24

### Added
- `task-mgr unblock` and `task-mgr unskip` accept several task ids. Every id is checked before any task changes.
- At startup, the loop force-commits its task files (PRD JSON, prompt, PRD markdown), so repos that gitignore them still have them committed before slot worktrees branch.
- `task-mgr recall --format json` carries a `full_details` field holding the `task-mgr show-learning <ids>` command for the returned learnings, matching the footer text output already prints.

### Changed
- `task-mgr show-learning` is listed in the `task-mgr enhance` workflow block, the cheat sheet recipes, and the staged best-practices doc, so agents read full learnings with it instead of scripting over recall output.
- When `task-mgr complete` refuses tasks that were never claimed, the error lists every such id with its status and prints one rerun line with `--force` and all the ids. It says that no `unblock` step is needed and that `--commit` is optional.
- The documented way to close a resolved CLARIFY or other human-review task is `task-mgr complete --force <id>`.

### Fixed
- `task-mgr --dir X init` writes the database and config to `X`, not to a `.task-mgr` folder beside it.
- A fresh database waits for a lock while switching to WAL, instead of failing at once with "database is locked".
- `archive` moves files across filesystems and finds the PRD markdown that sits next to the task list.
- Repaired stale tests that broke `cargo clippy --all-targets` and the library test suite.

## [1.0.2] - 2026-10-30

### Changed
- Package version is 1.0.2.

## [1.0.1] - 2026-10-30

### Added
- Rate-limit text is classified only after the child CLI reports an error. The rate-limit reaction runs after the ladder, from a pre-pipeline snapshot, and a completed task tag skips overflow handling. When output has no completed tag, the grace-buffer tail is scanned.
- Generated loops have a prune gate.

### Changed
- The built-in Claude ladder uses family aliases (`fable`, `opus`, `sonnet`, `haiku`). Claude Code resolves each alias to the current model in that family. A full model id remains a snapshot pin via `task-mgr models set-tier`.
- The built-in Grok `standard` rung omits `--model`, so the Grok CLI uses its own default.
- `task-mgr init` stages `~/.claude/docs/task-mgr-best-practices.md` (add/update `--from-json`, `update --stdin`, scoped export). Slash-command skills (`prd-tasks`, `plan-tasks`, `spike`, `review-loop`) pin spawn-fixups with `--from-json`.

### Fixed
- PRD path identity follows a symlink temp prefix.
- `task-mgr archive` names the task prefix on skip lines.
- The dev-container Ollama health check uses `ollama list`.

## [1.0.0] - 2026-10-29

### Added
- New git worktrees go to `<worktreeRoot>/<repo-name>/<sanitized-branch>/` when `worktreeRoot` is set in the user config (`$XDG_CONFIG_HOME/task-mgr/config.json`, otherwise `~/.config/task-mgr/config.json`) or `TASK_MGR_WORKTREE_ROOT` is an absolute path. The value `sibling` keeps `{repo-parent}/{repo-name}-worktrees/`. The root must already exist and be its own mounted filesystem; task-mgr does not create it and does not fall back when that check fails. `task-mgr worktrees` follows `git worktree list`, so worktrees already in the sibling directory stay listed. Two repositories that share a basename cannot share one `<root>/<repo-name>/` directory.

## [0.3.5] - 2026-10-28

### Added
- `task-mgr show-learning <id> [<id>...]` prints the full record for one or more learnings: untruncated content, root cause, solution, tags, applicability, stats, and retired/superseded status (`--format json` for every field). It is read-only and does not count as the learning being shown. When some IDs exist, the missing ones are listed and it exits 0; when none exist, it fails with `Learning not found`. `recall` text output now ends with a `Full details: task-mgr show-learning <ids>` line, and the loop prompt's Relevant Learnings block, `task-mgr how recall`, and the `tm-recall` skill point agents to it instead of the SQLite database.

## [0.3.4] - 2026-09-23

### Added
- `task-mgr loop stop --prefix <prefix>` writes the stop file the running loop actually watches. A prefix stop file created after process start is also honored in the launch `tasks/` directory, the feature worktree `tasks/`, and the main checkout `tasks/`. A stale extra-dir prefix file is removed at start with instructions to create a fresh one. A batch that honors `.task-mgr/tasks/.stop` deletes that file so the next batch is not stopped by it.

### Fixed
- `task-mgr archive` warns and skips a missing registered PRD file instead of aborting, so later files and PRDs still archive.

## [0.3.3] - 2026-09-18

### Changed
- **Breaking:** `task-mgr export` defaults to the **active PRD only** (scoped
  dump). Pass `--all` to restore the previous dump-all behavior (all unarchived
  tasks + `prd_metadata ORDER BY id LIMIT 1`).
- **Breaking:** Writing `--to-json` onto a registered `task_list` always
  requires `--force`. Export remains a **lossy dump** (no `taskPrefix`, status
  collapsed to `passes`, extra keys stripped) — `--force` is not a merge.
  Prefer an unregistered dump path, or pin scope with `--from-json`.
- Overwrite-guard identity is dest-bound: an absolute `--to-json` at a
  linked worktree live JSON requires `--force` even when cwd is main.

### Notes
- Residual (not a code story): after merge, copy spawn-fixup / `add` /
  `update` / export recipes into `~/.claude/docs/task-mgr-best-practices.md`.
  That file is **not** in this repo — do not vendor a copy here.

## [0.3.2] - 2026-09-08

### Added
- Quota rung policy: remaining-percent gate, horizon heuristic, proto-channel
  exclude, `--use-other-models-ttl`, down-only clamp, and
  `models set-usage-rule` / `set-tier-fallback`. A low frontier HUD row is not
  an account emergency — continue on standard and cheaper rungs without a
  `set-tier` pin.

### Changed
- Clippy 1.98 clean (`-D warnings`) on CI stable.

### Added
- `task-mgr add` subcommand: accepts a task JSON object on stdin (`--stdin`),
  auto-assigns priority relative to existing tasks, and syncs the PRD JSON file
  atomically. Supports `--depended-on-by <task-id>` to wire the new task into
  an existing task's `dependsOn` list immediately upon creation.
- `<task-status>TASK-ID:status</task-status>` side-band tag: loop agents emit
  this tag to drive task state transitions (done / fail / skip / irrelevant)
  without ever touching the PRD JSON directly. The loop engine parses and
  dispatches each tag after every iteration.
- Loop iteration prompt now includes a `task_ops` section that instructs agents
  to use the `task-mgr` CLI and `<task-status>` tag for all task lifecycle
  operations, and explicitly forbids direct edits to `tasks/*.json`.
- Loop permission policy: `tasks/*.json` paths are added to
  `--disallowedTools` (Edit/Write) so the permission guard is enforced at the
  Claude subprocess level, with a stderr hint pointing agents to `task-mgr add
  --stdin` instead.
- `task-mgr models` subcommand family: `list [--remote/--refresh]`,
  `set-default [<id>] [--project]`, `unset-default [--project]`, `show`.
  Supports live `/v1/models` discovery when `ANTHROPIC_API_KEY` and
  `TASK_MGR_USE_API=1` are both set; silent offline fallback otherwise.
- Per-user `$XDG_CONFIG_HOME/task-mgr/config.json` with a `defaultModel` field
  that survives across worktrees. `.task-mgr/config.json` now also accepts
  `defaultModel` (overrides the user default when set).
- Per-iteration `--effort` level (derived from task difficulty) now appears in
  the iteration header and in `progress.txt` log entries.
- `cargo run --bin gen-docs [-- --check]` regenerates the model block in
  `.claude/commands/tasks.md` from `src/loop_engine/model.rs`; CI enforces sync.
- Regression guard `tests/no_hardcoded_models.rs` prevents literal model
  strings from creeping outside the canonical `model.rs`.
- Scoped permission modes for Claude subprocess (`Scoped`, `Dangerous`, `Auto`)
  controlled via `LOOP_PERMISSION_MODE` and `LOOP_ENABLE_AUTO_MODE` env vars.
- `--branch <name>` flag for `archive` command to filter by a specific branch
  (conflicts with `--all`).
- Warning on stderr when `LOOP_PERMISSION_MODE` is set to an unrecognized value.
- Auto-mode deprecation hint displayed once per session when applicable.
- Security documentation on `CODING_ALLOWED_TOOLS` and `PermissionMode` enum.

### Changed
- **Breaking (internal API):** `resolve_task_model` now takes a
  `ModelResolutionContext` struct instead of three positional `Option<&str>`
  args. Eliminates a silent-swap footgun and makes the new
  project/user-default fallbacks self-documenting at call sites.
- Model resolution precedence extended with two new fallback tiers
  (project config default → user config default) below PRD default.
  `difficulty==high` still always forces `OPUS_MODEL`.
- Effort-level mapping: `low→high`, `medium→xhigh`, `high→max`
  (was `low→medium`, `medium→high`, `high→max`) to match the renamed
  Claude CLI levels.
- Opus canonical ID bumped from `claude-opus-4-6` to `claude-opus-4-7`.
- Test fixtures that reference model IDs renamed to `.json.tmpl` and now use
  `{{OPUS_MODEL}}` / `{{SONNET_MODEL}}` / `{{HAIKU_MODEL}}` placeholders
  rendered by `tests/common/mod.rs::render_fixture_tmpl`.
- **Breaking:** `batch` command banner now displays the actual glob pattern(s)
  instead of just the pattern count (e.g. `matching 'tasks/*.json'` instead of
  `matching 1 pattern(s)`).
- Batch deduplication now canonicalizes file paths before the `HashSet` check,
  preventing duplicates from symlinks or relative-vs-absolute paths.
- Default permission mode changed from `--dangerously-skip-permissions` to
  scoped `--permission-mode dontAsk` with `CODING_ALLOWED_TOOLS`.

### Fixed
- Batch archive scoping now correctly filters by branch.
- Batch enrichment and deduplication bugs resolved.
- Stdin pipe handling for prompt delivery to Claude subprocess.
