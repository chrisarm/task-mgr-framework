# Changelog — 2026-05-11

## Split `task-mgr init` and add `task-mgr enhance`

**Branch**: `feat/init-split-and-enhance`
**PRD**: `tasks/init-split-and-enhance.md`

### What shipped

- **`task-mgr init`** (no args) now scaffolds `.task-mgr/`, runs migrations, and writes a default config — no PRD required. Project setup is decoupled from PRD import.
- **`task-mgr loop init <prd>` / `task-mgr batch init <glob>...`** are the new canonical PRD-import paths. `Loop` and `Batch` are now parent-with-subcommand (`Init` / `Run`).
- **`task-mgr enhance {agents,show,strip}`** writes a marker-fenced workflow block into `CLAUDE.md` and `AGENTS.md` so agents (Claude Code, Cursor, Aider) know how to use task-mgr correctly. `--profile workflow|full`, `--dry-run`, and `--create` flags supported.
- **`task-mgr init --enhance`** convenience flag combines project scaffold + agent doc enhancement.
- **`task-mgr init --from-json <prd>`** and the flat-form `task-mgr loop <prd>` / `task-mgr batch <glob>` are **permanent deprecated shims** — they print a one-line stderr notice and dispatch to the canonical path. Will not be removed; see `CLAUDE.md` "Deprecation policy".
- New shared util: `src/util/marker_splice.rs` (byte-preserving fenced-block splice + atomic write) consumed by both `gen-docs` and `enhance`.

### Why it matters

- Operators bootstrapping a new project no longer need to fabricate a placeholder PRD just to get `.task-mgr/` initialized. `cd new-project && task-mgr init` now works.
- Agent-facing docs (`CLAUDE.md`, `AGENTS.md`) can be regenerated in place without manual marker management — `task-mgr enhance agents` is idempotent and byte-preserving outside the markers.
- The Loop/Batch CLI shape now matches the rest of the tool's parent/subcommand convention (e.g. `task-mgr curate ...`), so `--help` discovery is consistent.

### Breaking changes

None. Every prior invocation form still works; deprecated forms print one stderr line and dispatch identically.

---
