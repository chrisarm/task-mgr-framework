# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
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
