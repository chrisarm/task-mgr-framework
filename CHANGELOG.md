# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Scoped permission modes for Claude subprocess (`Scoped`, `Dangerous`, `Auto`)
  controlled via `LOOP_PERMISSION_MODE` and `LOOP_ENABLE_AUTO_MODE` env vars.
- `--branch <name>` flag for `archive` command to filter by a specific branch
  (conflicts with `--all`).
- Warning on stderr when `LOOP_PERMISSION_MODE` is set to an unrecognized value.
- Auto-mode deprecation hint displayed once per session when applicable.
- Security documentation on `CODING_ALLOWED_TOOLS` and `PermissionMode` enum.

### Changed
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
