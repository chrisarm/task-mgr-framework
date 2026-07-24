# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Curate session cleanup (orphan ai-title file workaround)** for **task-mgr**.

## Problem Statement

Claude Code 2.1.110 has a known bug: the `--no-session-persistence` flag prevents conversation history from being written, but does NOT prevent an `ai-title` metadata file from being created at `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`. These files are single-line, 130-byte stubs (title only), but they accumulate — curate dedup/enrich spawns one claude subprocess per batch, so a single `task-mgr curate dedup` run leaves tens of orphan files per invocation.

Empirical confirmation: a wrapper logging argv (`CLAUDE_BINARY=/tmp/claude-log-argv.sh`) captured from a live `task-mgr curate dedup` showed argv is correct and includes `--no-session-persistence` in the right position. The flag is not being overridden by task-mgr; the CLI writes the metadata file as part of session init, before/around the persistence check.

The fix (this feature): add an opt-in to `spawn_claude` that (1) forces a known UUID via `--session-id <uuid>` and (2) detaches a thread that sleeps 30s then deletes the specific jsonl at `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`. Only curate's two call sites opt in. Loops and learnings-ingestion are unchanged.

Why not other approaches:
- `--bare` requires `ANTHROPIC_API_KEY` (disables OAuth/keychain auth) — user uses OAuth, would break curate.
- Dedicated cwd for curate isolates orphans but doesn't remove them, and still pollutes the projects dir with a separate folder.
- Snapshot/diff cleanup risks touching unrelated sessions (e.g. interactive Claude Code running in the same project dir, concurrent curate batches).
- Fixed known UUID means the deletion target is unambiguous — zero chance of collateral damage.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Internalize quality targets** — Read `qualityDimensions` and define what "done well" looks like for THIS task
2. **Map edge cases to implementation plan** — Read `edgeCases`/`failureModes`; for each, decide HOW it will be handled before coding
3. **Choose your approach** — State assumptions, consider 2-3 approaches with tradeoffs, pick the best, document in progress.txt
4. **After coding, self-critique** — "Does this satisfy every qualityDimensions constraint? Every edge case? Is it idiomatic and efficient?" — revise before moving on

---

## How to Work

1. Read `tasks/curate-session-cleanup.json` for your task list
2. Read `tasks/progress.txt` (if exists) for context from previous iterations
3. Read `tasks/long-term-learnings.md` for project patterns
4. Read `CLAUDE.md` for project conventions
5. Pick the highest-priority eligible task (`passes: false`, all `dependsOn` complete)
6. **Before coding**: Read the task's DO/DO NOT sections, qualityDimensions, and edgeCases. State your approach briefly.
7. **Implement**: Code + tests together
8. **After coding**: Self-critique against every acceptance criterion, especially negative ones and known-bad discriminators
9. Run quality checks (below)
10. Commit: `feat: TASK-ID-completed - [Title]`
11. Output `<completed>TASK-ID</completed>` (task-mgr CLI will also accept `<task-status>TASK-ID:done</task-status>`)
12. Append progress to `tasks/progress.txt`

---

## Priority Philosophy

1. **PLAN** — Approach before code; consider 2-3 options
2. **FUNCTIONING CODE** — Pragmatic, reliable, wired in
3. **CORRECTNESS** — Self-critique; all tests pass
4. **CODE QUALITY** — Clean, idiomatic, no warnings

**Prohibited outcomes:**

- Tests that only assert "no crash" or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- A cleanup mechanism that could delete unrelated session files (e.g., interactive sessions, other curate batches, loop iterations)
- A cleanup thread that blocks or prolongs curate latency
- CLI arg ordering that breaks `--no-session-persistence` (must stay before `-p` per learning 671)

---

## Key Context

### Files to modify

- `src/loop_engine/claude.rs` — `spawn_claude()` at line 80, args-building block at 92-144, spawn block at 149-187. Existing helper `cleanup_ghost_sessions()` at line 668 (targets a DIFFERENT dir — `~/.claude/sessions/`, not `~/.claude/projects/`; do not conflate).
- `src/commands/curate/mod.rs` — dedup call site at line 604
- `src/commands/curate/enrich.rs` — enrich call site at line 235
- All other callers of `spawn_claude(` across `src/` — update to pass `false` explicitly for the new arg:
  - `src/learnings/ingestion/mod.rs:84`
  - `src/loop_engine/prd_reconcile.rs:659`
  - `src/loop_engine/engine.rs:528`
  - `src/loop_engine/watchdog.rs:308, 364`
  - `src/loop_engine/claude.rs:747, 1013, 1087, 1491`

### Key functions/types to reuse

- `uuid::Uuid::new_v4()` — already used in `src/commands/curate/dedup.rs` and `src/commands/curate/enrich.rs`. Cargo.toml: `uuid = { version = "1", features = ["v4", "serde"] }`.
- `std::thread::spawn(move || { ... })` — detached; do NOT hold the join handle.
- `std::time::Duration::from_secs(30)` — put this behind a named const (e.g. `TITLE_ARTIFACT_CLEANUP_DELAY`).
- `std::env::var("HOME")` — already used nearby in `cleanup_ghost_sessions` at line 669.
- `CLAUDE_BINARY_MUTEX` at `src/loop_engine/test_utils.rs:16` — serialize tests that mutate `CLAUDE_BINARY`.

### Key learnings from task-mgr (from prior loop runs)

- **Learning [671] — CLI arg ordering matters**: flags (output-format, --no-session-persistence, permission-mode, model, effort, --session-id, --allowedTools, --disallowedTools) must appear BEFORE `-p`. Claude parses flags only to the left of the prompt position. Placing `--session-id` after `-p` will make Claude ignore it and pick a fresh UUID — the cleanup path will then target the wrong file and the orphan survives.
- **Learning [1429] — curate subcommand 5-file order**: not directly applicable here (no new subcommand), but a reminder that touching curate means mod.rs, cli/commands.rs, handlers.rs may all be relevant. For THIS feature, only curate/mod.rs and curate/enrich.rs change at call sites.
- **Learning [1273] — session_guidance propagation**: unrelated pattern; ignore.
- **Learning [1432] — curate tests use in-memory SQLite**: relevant if you add tests in `src/commands/curate/tests.rs`. Pattern: `create_schema(&conn)` on in-memory Connection.

### Callers to preserve compatibility with

Every `spawn_claude` caller must be updated to pass `false` for the new arg, OR `true` at the two curate sites. No defaults. Compile errors on missed sites are the safety mechanism.

---

## Path-encoding contract (Data Flow)

Claude encodes the cwd by replacing `/` with `-`, preserving the leading slash as a leading dash:

| Input cwd                                               | Encoded dir name                                         |
| ------------------------------------------------------- | -------------------------------------------------------- |
| `$HOME/foo`                                       | `-home-chris-foo`                                        |
| `$HOME/projects/task-mgr`      | `-home-chris-Documents-startat0-Projects-task-mgr`       |
| `$HOME/foo/` (trailing slash)                     | `-home-chris-foo` (trim trailing slash first)            |

Full target path: `<HOME>/.claude/projects/<encoded>/<uuid>.jsonl`.

**Verify empirically**: `ls ~/.claude/projects/ | head -5` in a dev shell shows entries like `-home-chris-Documents-...`. Match that exact scheme.

**Do NOT resolve symlinks**: this repo can be reached as both `$HOME/projects/task-mgr` (symlink) and `$HOME/projects/task-mgr` (real). Claude encodes whichever path the process cwd reports — use the cwd as-is without `canonicalize()`, or the encoded path won't match where Claude actually wrote the file.

Suggested pure helper signature:

```rust
fn encoded_cwd_dir(cwd: &Path, home: &Path) -> PathBuf {
    let cwd_str = cwd.to_string_lossy().trim_end_matches('/').to_string();
    let encoded = cwd_str.replace('/', "-");
    home.join(".claude").join("projects").join(encoded)
}
```

---

## What Good Looks Like vs What Bad Looks Like

### Good implementation patterns

- Opt-in is a bare `bool` positional arg at the end of `spawn_claude`, matching the existing verbose-positional style (already `#[allow(clippy::too_many_arguments)]`).
- UUID generated once per spawn, printed into argv before `-p`, and captured `move`d into the cleanup thread by value.
- Cleanup thread: `std::thread::spawn(move || { std::thread::sleep(DELAY); let _ = std::fs::remove_file(&path); })` — detached, result ignored.
- Path-encoding is a pure function taking `&Path` inputs, easy to unit-test without filesystem.
- Delay behind a named const at module top.

### Bad patterns to avoid

- `thread::spawn(...).join()` — defeats the purpose, blocks spawn_claude.
- `std::fs::read_dir()` to find recently-created .jsonl files — risks deleting interactive sessions or concurrent batches.
- Fallback to a "newest file" heuristic if the uuid-derived path isn't found — same collateral-damage risk.
- Putting `--session-id` after `-p` — Claude ignores it, UUID mismatch → orphan survives, cleanup runs against a file that was never created.
- Making the new arg `Option<bool>` with a default — every caller should be forced to decide; compile errors are a feature here.
- Reusing `cleanup_ghost_sessions()` — it targets `~/.claude/sessions/`, NOT `~/.claude/projects/<cwd>/`.
- Enabling the opt-in for `learnings/ingestion` or `prd_reconcile` — user scope is curate only.

---

## Common Wiring Failures

| Symptom                                          | Cause                                         | Fix                                   |
| ------------------------------------------------ | --------------------------------------------- | ------------------------------------- |
| `--session-id` appears in argv but file survives | Flag placed AFTER `-p` — Claude ignored it    | Move flag before `-p` in args vector  |
| cargo check: "expected 10 arguments"             | Missed a caller of spawn_claude               | Update every call site — no defaults  |
| Cleanup thread never runs                        | Handle dropped before thread runs             | Detach with `thread::spawn`, no `_ =` binding that could get dropped (thread::spawn already detaches when handle is dropped — fine) |
| File deleted but curate still slow               | Thread is joined or channel-awaited somewhere | Ensure spawn_claude returns immediately after scheduling cleanup |
| Encoded path doesn't match what's on disk        | Called canonicalize() on cwd                  | Use cwd as-is; do not resolve symlinks |

---

## Quality Checks (REQUIRED every iteration)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test 2>&1 | tee /tmp/test-results.txt | tail -5 && grep "FAILED\|error\[" /tmp/test-results.txt | head -10
```

Fix any failures before committing.

---

## Empirical verification (REVIEW-001)

```bash
# Build
cargo build --release

# Run a dry dedup with argv logging wrapper
CLAUDE_BINARY=/tmp/claude-log-argv.sh target/release/task-mgr curate dedup --dry-run --concurrency 1 2>&1 | tail -20

# Inspect argv — must contain --session-id <uuid> BEFORE -p
cat /tmp/claude-argv.log | head -3

# Snapshot projects dir immediately, and again at 40s — orphans present then cleared
ls -la ~/.claude/projects/-home-chris-Documents-startat0-Projects-task-mgr/*.jsonl | head -5
sleep 40
ls -la ~/.claude/projects/-home-chris-Documents-startat0-Projects-task-mgr/*.jsonl | head -5
```

Document the pre/post counts in `tasks/progress.txt`.

---

## Task Files

| File                                      | Purpose                                 |
| ----------------------------------------- | --------------------------------------- |
| `tasks/curate-session-cleanup.json`       | Task list — read, mark complete         |
| `tasks/curate-session-cleanup-prompt.md`  | This prompt (read-only)                 |
| `tasks/progress.txt`                      | Progress log — append findings          |
| `tasks/long-term-learnings.md`            | Curated learnings (read first)          |

---

## Review Task (REVIEW-001)

When you reach REVIEW-001:

1. Review all new code for correctness, security, idiomatic Rust
2. Run the empirical verification above and paste the before/after counts into progress.txt
3. Confirm spawn_claude's doc-comment describes the new opt-in and its safety guarantees (deterministic target, detached thread)
4. Check CLAUDE.md — add a short "Curate session cleanup workaround" section if useful for future contributors
5. Record a `task-mgr learn --outcome workaround --confidence high --tags curate,claude-cli,session-persistence` entry summarizing: "Claude Code 2.1.110 writes ai-title metadata to ~/.claude/projects/<cwd>/<uuid>.jsonl despite --no-session-persistence; curate passes its own --session-id and schedules a 30s deferred delete to clean up."
6. If issues: add FIX-xxx tasks to JSON (priority 50-97), commit. Otherwise mark REVIEW-001 `passes:true`.

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

- One task per iteration
- Commit after each task
- Read before writing
- Minimal changes — only what's required
- Branch: `feat/curate-session-cleanup`
