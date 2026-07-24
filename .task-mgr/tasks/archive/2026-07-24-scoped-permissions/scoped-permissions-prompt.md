# Claude Code Agent Instructions

You are an autonomous coding agent implementing **scoped permissions for Claude subprocess invocation** in **task-mgr**.

## Problem Statement

task-mgr currently spawns Claude Code with `--dangerously-skip-permissions`, which bypasses ALL permission checks — giving the autonomous agent unrestricted access to the filesystem, network, and all tools. This needs to be replaced with scoped permissions (`--permission-mode dontAsk` + `--allowedTools`) that grant only the tools the agent actually needs. Additionally, support for the upcoming `--enable-auto-mode` flag should be added (disabled by default) with a deprecation hint when it becomes available.

---

## How to Work

1. Read `tasks/scoped-permissions.json` for your task list
2. Read `tasks/progress.txt` (if exists) for context from previous iterations
3. Read `CLAUDE.md` for project conventions
4. Pick the highest-priority eligible task (`passes: false`, all `dependsOn` complete)
5. **Before coding**: Read the task's DO/DO NOT sections and edge cases. State your approach briefly.
6. **Implement**: Code + tests together in one coherent change
7. **After coding**: Self-critique — check each acceptance criterion, especially negative ones and known-bad discriminators
8. Run quality checks (below)
9. Commit: `feat: TASK-ID-completed - [Title]`
10. Output `<completed>TASK-ID</completed>`
11. Append progress to `tasks/progress.txt`

---

## Key Context

### The core change

Replace `--dangerously-skip-permissions` in `spawn_claude()` with a `PermissionMode` enum that supports three strategies:
- **Dangerous** (legacy escape hatch): `--dangerously-skip-permissions`
- **Scoped** (new default): `--permission-mode dontAsk --allowedTools "Read,Edit,Write,..."`
- **Auto** (future): `--enable-auto-mode` (disabled by default, launching >= March 11, 2026)

### spawn_claude has 4 production callers with different needs

| Caller | File | Needs Tools? | PermissionMode |
|--------|------|-------------|----------------|
| Main loop | engine.rs:459 | Yes (coding) | Scoped { tools: Some(CODING_ALLOWED_TOOLS) } |
| Learning extraction | learnings/ingestion/mod.rs:77 | No (text analysis) | Scoped { tools: None } |
| Curate enrich | curate/enrich.rs:234 | No (text analysis) | Scoped { tools: None } |
| Curate dedup | curate/mod.rs:613 | No (text analysis) | Scoped { tools: None } |

### Files to modify

- `src/loop_engine/config.rs` — PermissionMode enum, constants, env var parsing
- `src/loop_engine/claude.rs` — spawn_claude signature, flag construction, tests
- `src/loop_engine/engine.rs` — IterationParams struct, run_loop() wiring, deprecation hint
- `src/learnings/ingestion/mod.rs` — caller update
- `src/commands/curate/enrich.rs` — caller update
- `src/commands/curate/mod.rs` — caller update
- `src/loop_engine/watchdog.rs` — test caller update

### Key functions/types to reuse

- `config::parse_env_bool()` at config.rs:144 — parse boolean env vars
- `config::parse_bool_value()` at config.rs:134 — parse boolean strings (needs to be made pub(crate))
- `CLAUDE_BINARY` env var pattern at claude.rs:75 — existing env override for the binary
- `spawn_claude_echo()` test helper at claude.rs:457 — CLAUDE_BINARY=echo for test assertions
- `ENV_MUTEX` at claude.rs:454 — mutex for env var tests to avoid parallel interference

### Callers to preserve compatibility with

- `engine.rs` run_iteration() at line 459 — main loop caller
- `learnings/ingestion/mod.rs` extract_learnings_from_output() at line 77
- `curate/enrich.rs` enrichment batch loop at line 234
- `curate/mod.rs` dedup batch loop at line 613
- `watchdog.rs` tests at lines 308 and 362

---

## What Good Looks Like vs What Bad Looks Like

### Good implementation patterns:

- `PermissionMode` as an enum with data (Scoped carries allowed_tools) — type-safe, exhaustive matching
- Args built as `Vec<String>` — clean ownership, no lifetime juggling
- Permission mode resolved once in run_loop() and threaded through IterationParams — no repeated env lookups
- Text-only callers explicitly pass `Scoped { allowed_tools: None }` — clear intent
- Tests use CLAUDE_BINARY=echo to capture actual CLI args — verifies real behavior

### Bad patterns to avoid:

- Hardcoded permission flags (the old approach) — inflexible, no configuration
- Checking LOOP_ENABLE_AUTO_MODE before LOOP_PERMISSION_MODE — wrong precedence, dangerous should always win
- Passing `Scoped { allowed_tools: None }` for the main loop caller — would break all iterations
- Adding PermissionMode to LoopConfig — it's per-call-site, not global config
- Using `--permission-mode=dontAsk` as a single arg — it's two separate args: `--permission-mode` then `dontAsk`

---

## Quality Checks (REQUIRED every iteration)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

Fix any failures before committing. Never commit broken code.

---

## Task Files

| File | Purpose |
|------|---------|
| `tasks/scoped-permissions.json` | Task list — read tasks, mark complete |
| `tasks/scoped-permissions-prompt.md` | This prompt (read-only) |
| `tasks/progress.txt` | Progress log — append findings and learnings |

---

## Environment Variables (New)

| Variable | Values | Default | Description |
|----------|--------|---------|-------------|
| `LOOP_PERMISSION_MODE` | `dangerous` | (unset) | Force legacy --dangerously-skip-permissions |
| `LOOP_ENABLE_AUTO_MODE` | `true`/`false` | `false` | Enable --enable-auto-mode flag |
| `LOOP_ALLOWED_TOOLS` | comma-separated | CODING_ALLOWED_TOOLS | Override default tool allowlist |
| `LOOP_AUTO_MODE_AVAILABLE` | `true`/`false` | (unset) | Triggers deprecation hint at loop start |

---

## Review Task (REVIEW-001)

When you reach REVIEW-001:

1. Review ALL implementation for quality, security, and integration wiring
2. Verify permission_mode flows correctly: env → run_loop → IterationParams → spawn_claude
3. Verify CODING_ALLOWED_TOOLS matches what scripts/prompt.md instructs the agent to use
4. Check every acceptance criterion marked "Negative:" — these are the most common failure modes
5. Run full test suite
6. If issues found: add FIX-xxx tasks to the JSON file (priority 50-98), commit JSON
7. The loop will pick up new FIX tasks automatically

---

## Progress Report Format

APPEND to `tasks/progress.txt`:

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
- Work on the correct branch: `feat/scoped-permissions`
