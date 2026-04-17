# Architecture & Design Rationale

This document explains the architectural decisions behind task-mgr: what was chosen, what was rejected, and why.

## Table of Contents

- [Problem Statement](#problem-statement)
- [System Overview](#system-overview)
- [Core Design Decisions](#core-design-decisions)
- [Database Design](#database-design)
- [Task State Machine](#task-state-machine)
- [Task Selection Algorithm](#task-selection-algorithm)
- [Learnings System](#learnings-system)
- [Loop Engine](#loop-engine)
- [Error Handling](#error-handling)
- [Testing Strategy](#testing-strategy)
- [Module Boundaries](#module-boundaries)

---

## Problem Statement

AI agent loops (e.g., Claude Code running repeatedly against a PRD) suffer from three structural problems:

1. **Stateless iterations**: Each agent invocation starts fresh. It doesn't know what was tried before, what failed, or what patterns were discovered. This leads to agents hitting the same compilation error five times in a row.

2. **Dumb task ordering**: Without dependency awareness and file locality, agents context-switch between unrelated areas of the codebase. Implementing a database migration then jumping to a UI component then back to a database query wastes context window budget on re-understanding code.

3. **Brittle orchestration**: The original `claude-loop.sh` (1,455 lines of bash) used `jq`, `grep`, and `sed` to parse JSON and detect outcomes. This was untestable, untyped, had GNU/BSD portability issues, and failed silently on edge cases.

### Design goal

Replace the bash script with a typed, testable Rust CLI that adds three capabilities the shell script couldn't support:

1. **Closed-loop learning feedback** -- Track which learnings are shown and whether they helped
2. **Enriched prompt context** -- Scan actual source code for signatures instead of guessing
3. **Adaptive selection weights** -- Tune scoring based on historical outcomes

---

## System Overview

```
                          ┌──────────────────────────┐
                          │       PRD JSON File       │
                          │  (tasks, deps, files)     │
                          └──────────┬───────────────┘
                                     │ init / export
                                     ▼
┌─────────────────────────────────────────────────────────────┐
│                        task-mgr CLI                          │
│                                                              │
│  ┌───────────┐  ┌───────────┐  ┌───────────┐  ┌──────────┐ │
│  │   init    │  │   next    │  │ complete  │  │  learn   │ │
│  │  export   │  │ (select + │  │   fail    │  │  recall  │ │
│  │  doctor   │  │  claim +  │  │   skip    │  │  bandit  │ │
│  │  migrate  │  │  recall)  │  │   reset   │  │          │ │
│  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘  └────┬─────┘ │
│        │              │              │              │        │
│        └──────────────┴──────────────┴──────────────┘        │
│                              │                               │
│                    ┌─────────▼──────────┐                    │
│                    │   SQLite (WAL)     │                    │
│                    │   + fs2 locking    │                    │
│                    └────────────────────┘                    │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐  │
│  │                    Loop Engine                          │  │
│  │  engine → prompt → context → claude → detection →      │  │
│  │  feedback → calibrate → crash/stale tracking           │  │
│  └────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

The system has two modes of operation:

1. **CLI commands** (synchronous): Individual commands like `next`, `complete`, `learn` that read/write SQLite and exit. Used by external scripts or humans.
2. **Loop engine** (long-running): The `task-mgr loop` command that orchestrates repeated Claude invocations, managing the full lifecycle internally.

Both share the same database layer, models, and command implementations.

---

## Core Design Decisions

### Decision 1: SQLite over PostgreSQL

**Chosen**: Embedded SQLite with WAL mode and bundled compilation.

**Rejected**: PostgreSQL, JSON files on disk, in-memory state.

**Rationale**: task-mgr is a CLI tool that runs on developer machines. PostgreSQL requires a running server -- unacceptable for a zero-config tool. JSON files lack transactions, concurrent access safety, and query capability. In-memory state doesn't survive crashes.

SQLite with WAL mode provides:
- Zero-config: no server, no setup, just a file
- ACID transactions with crash safety
- Concurrent reads (WAL mode allows readers while writing)
- FTS5 for full-text search on learnings
- ~2-4ms connection time, appropriate for CLI tools

The `bundled` feature compiles SQLite from source, ensuring FTS5 availability and eliminating system dependency issues.

### Decision 2: No connection pooling

**Chosen**: Each command opens a fresh SQLite connection and closes it on exit.

**Rejected**: Connection pool (r2d2, deadpool), persistent connection.

**Rationale**: CLI commands are short-lived processes. A connection takes ~2-4ms to open. Pooling adds complexity for zero benefit when the process exits immediately. Even the loop engine creates connections per-iteration, which is fine for SQLite.

### Decision 3: File locking with fs2

**Chosen**: Advisory file locking via `fs2::FileExt` for write serialization.

**Rejected**: SQLite's built-in locking (insufficient for multi-process CLI), database-level mutexes.

**Rationale**: Multiple task-mgr processes might run simultaneously (e.g., one in a loop, another manual `task-mgr stats` query). Read-only commands don't acquire the lock. Write commands (init, complete, fail, next --claim) acquire an exclusive lock on `.task-mgr/tasks.db.lock`.

The lock file includes the PID for debugging stuck locks. If a process crashes without releasing the lock, the OS releases the advisory lock automatically.

### Decision 4: Rust over continuing with Bash

**Chosen**: Full Rust rewrite of the loop orchestration.

**Rejected**: Incremental bash improvements, Python wrapper, Node.js.

**Rationale**: The bash script had five specific pain points:

1. **JSON parsing via jq**: Fragile, no type checking. A missing field silently produces empty strings.
2. **Portability**: `md5sum` vs `md5`, `date` flag differences between GNU and BSD.
3. **Untestable**: No unit tests for bash functions. Behavior verified only by manual runs.
4. **No type safety**: Shell variables with no validation. Typo in a variable name = silent bug.
5. **Difficult to extend**: Adding a new detection pattern (rate limit, reorder hint) requires careful shell string manipulation.

Rust provides compile-time type checking, a test framework, and deterministic behavior. The trade-off is higher development cost, which was acceptable given this is a long-lived infrastructure tool.

### Decision 5: Synchronous CLI, async only for loop

**Chosen**: All CLI commands are synchronous. Only `task-mgr loop` uses a tokio runtime.

**Rejected**: Fully async architecture.

**Rationale**: SQLite operations are synchronous. Network I/O only happens in the loop engine (spawning Claude, OAuth token refresh, usage API). Creating a tokio runtime only when needed avoids the compile-time cost of async for 90% of commands.

```rust
// Only the loop command creates a runtime
Commands::Loop { .. } => {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async { run_loop(config).await });
}
```

### Decision 6: ureq over reqwest for HTTP

**Chosen**: `ureq` (synchronous, minimal HTTP client).

**Rejected**: `reqwest` (async, full-featured).

**Rationale**: The loop engine makes simple synchronous HTTP calls for OAuth token refresh and usage API queries. `ureq` is ~200KB; `reqwest` pulls in hyper, tower, and a TLS stack at ~10MB. The additional features (connection pooling, HTTP/2, streaming) aren't needed for occasional REST calls.

---

## Database Design

### Schema overview

```
┌──────────────┐     ┌──────────────┐     ┌───────────────────┐
│    tasks     │────▶│  task_files   │     │ task_relationships │
│              │     │              │     │                   │
│ id (PK)      │     │ task_id (FK)  │     │ task_id (FK)      │
│ title        │     │ file_path     │     │ related_id        │
│ priority     │     └──────────────┘     │ rel_type          │
│ status       │                          └───────────────────┘
│ description  │
│ error_count  │     ┌──────────────┐     ┌──────────────────┐
│ last_error   │     │    runs      │────▶│    run_tasks     │
│ ...          │     │              │     │                  │
└──────────────┘     │ run_id (PK)  │     │ run_id (FK)      │
                     │ iteration    │     │ task_id (FK)     │
┌──────────────┐     │ status       │     │ outcome          │
│  learnings   │     └──────────────┘     └──────────────────┘
│              │
│ id (PK)      │     ┌───────────────┐    ┌──────────────────┐
│ outcome      │     │ learning_tags │    │  prd_metadata    │
│ title        │     │               │    │                  │
│ content      │     │ learning_id   │    │  Preserves PRD   │
│ confidence   │     │ tag           │    │  structure for   │
│ times_shown  │     └───────────────┘    │  round-trip      │
│ times_applied│                          └──────────────────┘
│ ...          │     ┌───────────────┐
└──────────────┘     │ global_state  │
                     │               │
                     │ iteration_ctr │
                     │ last_task_id  │
                     └───────────────┘
```

### Why separate `task_files` and `task_relationships` tables?

**Rejected alternative**: Store files and relationships as JSON arrays in the tasks table.

**Rationale**: Separate tables enable indexed queries. Finding "all tasks that touch `src/db/*.rs`" requires scanning JSON arrays (slow) vs. a simple `WHERE file_path GLOB 'src/db/*.rs'` on an indexed column (fast). The task selection algorithm queries files and relationships extensively -- O(1) lookups via HashMaps built from indexed queries matter for the 3.5-6.3ms target.

### Why `global_state` instead of derived counters?

The iteration counter and last task ID live in a single-row `global_state` table rather than being derived from `run_tasks` counts.

**Rationale**: Derived counters require scanning the run_tasks table on every `next --claim`. A single-row update is O(1). The counter also survives database rebuilds from JSON export (it's exported as part of the state).

### Migration strategy

Schema migrations are versioned (v1, v2, v3) and tracked in a `schema_migrations` table. Each migration is a Rust function that runs DDL in a transaction.

**Why not sqlx migrations?**: task-mgr bundles SQLite and doesn't use sqlx. Keeping migrations as Rust code allows conditional logic (e.g., "add column if not exists") and testing.

---

## Task State Machine

```
           ┌─────────────┐
           │    todo      │ ◄──────────────────────────┐
           └──────┬───────┘                            │
                  │ next --claim                       │
                  ▼                                    │
           ┌─────────────┐                             │
           │ in_progress  │                             │
           └──┬───┬───┬──┘                             │
              │   │   │                                │
    complete  │   │   │  fail/skip                     │
              │   │   │                                │
              ▼   │   ▼                                │
     ┌──────┐ │   │  ┌─────────┐    unblock/unskip    │
     │ done │ │   │  │ blocked │ ───────────────────────┤
     └──────┘ │   │  │ skipped │                       │
              │   │  └─────────┘                       │
              │   │                                    │
              │   │  irrelevant                        │
              │   ▼                                    │
              │ ┌────────────┐                         │
              │ │ irrelevant │     (32-iter decay)     │
              │ └────────────┘     for blocked/skipped─┘
              │
              │ (terminal - no transitions out)
              ▼
```

### Key design choices

**`done` is terminal**: Once completed, a task cannot transition to any other state. This prevents accidentally re-opening completed work. The `reset` command exists for intentional re-runs but requires explicit action.

**`irrelevant` satisfies dependencies**: A task marked irrelevant counts as "satisfied" for dependency resolution. This allows the dependency chain to unblock when a prerequisite is determined to be unnecessary rather than requiring it to be completed.

**Decay mechanism**: Blocked and skipped tasks record the iteration number when they entered that state. After a configurable threshold (default: 32 iterations), they automatically return to `todo`. This prevents tasks from being permanently stuck due to transient issues.

**`--force` flag**: The `complete` and `fail` commands accept `--force` to override invalid transitions (e.g., `todo` directly to `done`). This is an escape hatch for recovery scenarios, not normal workflow.

### Human review checkpoints

Tasks in the PRD JSON can be marked with `"requiresHuman": true` to designate them as human review gates. When such a task completes, the loop engine pauses for interactive feedback before continuing.

**Data flow**: The `requiresHuman` boolean is parsed from the PRD JSON (`PrdUserStory.requires_human`), stored in the `tasks` table as `requires_human INTEGER DEFAULT 0`, and queried by the engine after each iteration completes.

**Trigger mechanism**: After a task is marked `done`, `trigger_human_reviews()` queries for tasks with `requires_human = 1` that were completed since the current loop run started (`completed_at >= epoch`). For each matching task:

1. `handle_human_review()` (in `signals.rs`) displays an interactive banner with the task ID, title, and notes
2. Reads multi-line feedback from stdin until an empty line or EOF
3. An optional per-task timeout (`humanReviewTimeout` in seconds) uses a channel-based reader thread — if the timeout expires with no input, the review is skipped gracefully
4. If feedback is provided, it is tagged as `[Human Review for {task_id}] {text}` and accumulated in `SessionGuidance` for injection into subsequent iteration prompts
5. `mutate_prd_from_feedback()` (in `prd_reconcile.rs`) spawns a Claude subprocess to analyze the feedback against remaining todo tasks and atomically updates the PRD JSON with any modifications

**Batch mode interaction**: In batch/yes mode, `trigger_human_reviews()` temporarily overrides `yes_mode` to ensure the interactive pause still occurs — human review gates are never silently skipped.

---

## Task Selection Algorithm

### Why weighted multi-factor scoring?

Three approaches were considered:

| Approach | Pros | Cons |
|----------|------|------|
| Pure priority | Simple, predictable | Ignores locality, causes context switching |
| Pure file locality | Minimizes switching | Ignores priority, may leave critical work undone |
| **Weighted scoring** | Balances all factors | More complex, weights need tuning |

The weighted approach was chosen because AI agent loops benefit most from file locality (staying in the same area of the codebase) while still respecting priorities. The scoring formula:

```
score = (1000 - priority) + (10 * file_overlap) + (3 * synergy) + (-5 * conflict)
```

### Why these specific weights?

- **Priority base of 1000**: Creates a ~50-point range (priority 1 = 999, priority 50 = 950). This means file overlap can meaningfully influence selection (~30 points for 3 files) but can't completely override a large priority difference.
- **File overlap at 10**: Three shared files shifts by ~30 points, roughly equivalent to 3 priority levels. This makes locality meaningful without overwhelming.
- **Synergy at 3**: Acts as a tie-breaker when tasks are close in score. Doesn't override priority.
- **Conflict at -5**: Mild discouragement. A high-priority conflicting task still wins if it's clearly the best choice.

### Adaptive weight calibration

The loop engine extends static weights with adaptive calibration:

1. After each iteration, compute point-biserial correlation between task attributes and success/failure.
2. Adjust weights by the correlation coefficient, bounded to 0.5x-2.0x of defaults.
3. Store adjusted weights in `global_state` (JSON).

**Why point-biserial correlation instead of Pearson?** Simpler formula (mean_diff / normalizer), avoids expensive standard deviation calculations, stays in [-1, 1] range, and handles zero-variance cases gracefully. With limited data (dozens of iterations, not thousands), a simpler estimator is more appropriate.

### Deterministic tiebreaking

When scores are equal: lower priority number wins, then lexicographic task ID ordering. This ensures the same inputs always produce the same selection, which matters for reproducibility.

---

## Learnings System

### The core insight

Traditional agent loops treat each iteration as independent. The learnings system creates a **cross-iteration memory** that accumulates knowledge over time:

```
Iteration 1: Hits compilation error → records learning
Iteration N: Working on similar task → learning is surfaced automatically
             → agent avoids the error without hitting it first
```

### Multi-signal recall

Learnings are matched to tasks via three independent signals:

| Signal | Score | Mechanism |
|--------|-------|-----------|
| File pattern | +10 | Glob matching: learning's `applies_to_files` against task's `touchesFiles` |
| Task type | +5 | Prefix matching: `US-` from task ID against learning's `applies_to_task_types` |
| Error pattern | +2 | Substring matching: learning's `applies_to_errors` against task's `last_error` |

The intentional asymmetry (file >> type >> error) reflects that file locality is the strongest predictor of relevance.

### UCB bandit ranking

The Upper Confidence Bound algorithm balances two competing goals:

- **Exploitation**: Show learnings that have been confirmed useful (high `times_applied / times_shown` ratio)
- **Exploration**: Occasionally show new learnings to assess their value (high uncertainty bonus for low `times_shown`)

```
UCB_score = (times_applied / times_shown) + c * sqrt(ln(total_shown) / times_shown)
```

The exploration constant `c` is tuned so that new learnings (shown 0-2 times) get a meaningful chance while proven learnings still dominate. A sliding window ensures very old learnings with outdated statistics don't permanently dominate.

### Why FTS5 for text search?

SQLite's FTS5 extension provides BM25-ranked full-text search. Compared to `LIKE '%keyword%'`:

- BM25 ranking considers term frequency and document length
- Inverted index is O(log n) vs. O(n) table scan
- Supports phrase queries and boolean operators

FTS5 is included for free with the `bundled` rusqlite feature (SQLite 3.9+), so there's no additional dependency cost.

### Feedback loop closure

The loop engine's `feedback.rs` module closes the learning feedback loop:

1. `prompt.rs` builds the prompt and records which learning IDs were included (`shown_learning_ids`)
2. On successful task completion, `feedback.rs` calls `record_learning_applied()` for each shown learning
3. This updates `times_applied` and `last_applied_at`, improving future UCB scores

This only happens on success -- failed iterations don't update learning statistics, preventing noise from contaminating the ranking.

---

## Loop Engine

### Architecture

The loop engine (`src/loop_engine/`) is the largest subsystem with 18 modules:

```
┌─────────────────────────────────────────────────────────────────┐
│                         engine.rs                                │
│                    (orchestrator loop)                           │
│                                                                  │
│  For each iteration:                                             │
│    1. next() → task + learnings          ┌─────────────────────┐│
│    2. context.rs → scan source files     │   Pure State        ││
│    3. prompt.rs → assemble prompt        │   Machines:         ││
│    4. claude.rs → spawn subprocess       │   - crash.rs        ││
│    5. detection.rs → analyze output      │   - stale.rs        ││
│    6. Handle outcome (complete/fail/skip)│   (no I/O, fully    ││
│    7. trigger_human_reviews() → pause    │    testable)        ││
│    8. feedback.rs → close learning loop  └─────────────────────┘│
│    9. calibrate.rs → adjust weights                              │
│                                                                  │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────────────────┐ │
│  │  signals.rs  │ │  deadline.rs │ │      env.rs              │ │
│  │  .stop/.pause│ │  --hours     │ │  git, .env, branch detect│ │
│  └──────────────┘ └──────────────┘ └──────────────────────────┘ │
│                                                                  │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────────────────┐ │
│  │   oauth.rs   │ │   usage.rs   │ │      status.rs           │ │
│  │  token mgmt  │ │  API monitor │ │  dashboard command       │ │
│  └──────────────┘ └──────────────┘ └──────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

### Key design patterns

**Pure state machines for crash/stale tracking**: `crash.rs` and `stale.rs` are pure state machines with no I/O. They accept events and return decisions. This makes them fully testable with unit tests:

```rust
// crash.rs - no filesystem, no network, just state transitions
pub struct CrashTracker {
    consecutive_crashes: u32,
    max_crashes: u32,
    base_delay_secs: u64,
}

impl CrashTracker {
    pub fn record_crash(&mut self) -> CrashAction {
        self.consecutive_crashes += 1;
        if self.consecutive_crashes >= self.max_crashes {
            CrashAction::Abort
        } else {
            CrashAction::Backoff(self.backoff_seconds())
        }
    }

    fn backoff_seconds(&self) -> u64 {
        // Exponent capped at 20 to prevent overflow
        // 2^20 * 30s ≈ 8.7 hours (sufficient maximum)
        let exponent = (self.consecutive_crashes - 1).min(20);
        self.base_delay_secs.saturating_mul(1u64 << exponent)
    }
}
```

**Graceful degradation everywhere**: Missing credentials skip monitoring (don't crash). Missing `.env` uses defaults. Missing `steering.md` continues without it. Missing API responses fall back gracefully. The loop engine should never crash due to a missing optional resource.

**Enriched prompt context**: Instead of pasting raw task JSON into a template, the prompt builder:

1. Scans actual source files matching `touchesFiles` patterns
2. Extracts `pub fn`, `struct`, `enum`, `trait` signatures
3. Respects token budgets (per-file: 1500 chars, total: configurable)
4. Includes dependency completion summaries
5. Injects relevant learnings with feedback tracking

The per-file budget (1500 chars) prevents one large file from consuming the entire context budget. Without it, a 5000-char file would starve other files of representation.

**Reorder hints with bounds**: Claude can suggest working on a different task via `<reorder>TASK-ID</reorder>` in its output. This is treated as a hint, not an override:

- Valid reorders only (task exists and is eligible)
- Maximum 2 consecutive reorders before forcing the algorithm's recommendation
- Prevents Claude from thrashing between tasks indefinitely

**Task lifecycle via CLI + side-band tag** (replaces direct PRD JSON edits by the loop agent): the agent never reads or writes `tasks/*.json`. Instead:

- **New tasks** are created by piping a task JSON to `task-mgr add --stdin`. Priority is auto-computed as `top_task.priority - 1`, so the new task is picked next. `--depended-on-by <existing-id>` atomically wires the new task into an existing task's `dependsOn` array (both DB row and PRD JSON). DB + JSON are always updated together via a temp-file + atomic-rename write.
- **Status transitions** emit a `<task-status>TASK-ID:done</task-status>` side-band tag (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`). `detection::extract_status_updates` parses all tags in the output; `engine::apply_status_updates` dispatches each through the existing `complete` / `fail` / `skip` / `irrelevant` / `reset_tasks` command handlers. Unknown status keywords are logged and skipped; malformed tags don't corrupt well-formed tags later in the output. Side-band tags do NOT change `IterationOutcome` — they're metadata applied alongside whatever outcome detection returned.
- **Permission guard**: `--disallowedTools` passed to the Claude subprocess scopes-deny `Edit`/`Write` on `tasks/*.json` paths. `Read` on the PRD and `Bash(task-mgr:*)` remain allowed. The iteration prompt (`prompt_sections::task_ops`) documents the rules directly so the agent knows the intended alternative before hitting the guard.

### Crash recovery strategy

Recovery operates at multiple levels:

1. **Iteration level**: Export PRD JSON after every iteration. If the process crashes, the next run picks up from the exported state.
2. **Run level**: `doctor --auto-fix` resets stale `in_progress` tasks to `todo`. Run cleanup in a trap handler records run end status.
3. **Crash backoff**: Exponential backoff (30s * 2^n, capped at 2^20) prevents thundering herd on transient API failures.
4. **Signal handling**: Ctrl+C triggers graceful shutdown (finish current operation, export state, end run). Double Ctrl+C forces immediate exit.

### Steering and interactive control

The loop supports runtime steering without restart:

| Mechanism | How | Purpose |
|-----------|-----|---------|
| `.task-mgr/.stop` | Touch file | Stop after current iteration |
| `.task-mgr/.pause` | Touch file | Pause for interactive guidance input |
| `.task-mgr/steering.md` | Write file | Inject guidance into remaining iterations |
| `requiresHuman` | PRD field | Auto-pause after task completion for human review |
| Ctrl+C | Signal | Graceful shutdown |

Steering guidance accumulates across iterations (concatenated with separators), allowing progressive refinement of agent behavior.

---

## Error Handling

### Error type hierarchy

```rust
pub enum TaskMgrError {
    DatabaseError(rusqlite::Error),              // Simple wrapper
    DatabaseErrorWithContext { file, op, source }, // Rich context
    IoError(std::io::Error),
    IoErrorWithContext { file, op, source },
    JsonError(serde_json::Error),
    LockError { message, hint },                  // With recovery hints
    NotFound { resource_type, id },
    InvalidState { resource, id, expected, actual },
    InvalidTransition { task_id, from, to, hint },
    UnsafePath { context, path, reason },          // Path traversal guard
}
```

### Design principles

**Actionable error messages**: Every error variant is designed to tell the user what went wrong AND what to do about it. `LockError` includes a `hint` field with recovery instructions. `InvalidTransition` explains valid transitions.

**Factory methods for common patterns**: Instead of constructing error variants inline, factory methods like `task_not_found()`, `lock_error_with_hint()`, and `invalid_state()` reduce boilerplate and ensure consistent formatting.

**Path validation at input boundaries**: The `validate_safe_path()` function rejects absolute paths, parent traversal (`..`), home directory expansion (`~`), and UNC paths. This runs on paths from PRD files (untrusted input), not on CLI arguments (trusted user input).

---

## Testing Strategy

### Test pyramid

| Layer | Count | What's tested |
|-------|-------|---------------|
| Unit tests (`#[cfg(test)]`) | ~1200+ | Individual functions, state machines, scoring logic |
| Integration tests (`tests/`) | ~200+ | Multi-command workflows, import/export round-trips |
| CLI tests (`assert_cmd`) | ~40+ | End-to-end command execution |
| Concurrent tests | ~10+ | File locking under contention |
| E2E loop test | 1 | Full loop with mock Claude script |

### Testing patterns

**Tempdir isolation**: Every integration test creates a temporary directory with `tempfile::tempdir()`. Tests never share state, enabling parallel execution.

**Mock Claude script**: The E2E loop test uses a bash script that simulates Claude's behavior -- reading the prompt, outputting completion markers, and verifying the feedback loop works end-to-end.

**Pure state machine testing**: `crash.rs` and `stale.rs` are tested exhaustively because they have no I/O dependencies. Boundary conditions like exponent overflow (cap at 20) and hash collision resistance are covered.

**Flaky test prevention**: Environment variable manipulation (common in config tests) is isolated into pure parsing functions tested directly, avoiding race conditions with parallel test execution.

### TDD approach for new modules

New modules follow a tests-first workflow:

1. Write comprehensive tests with `#[ignore]` (types don't exist yet)
2. Define types and interfaces to make tests compile
3. Implement until tests pass
4. Remove `#[ignore]` tags

This was particularly valuable for the loop engine, where 18 modules have complex interactions. Tests define the contract; implementation fills it in.

---

## Module Boundaries

### Size discipline

Files are kept under ~720 lines. When a module exceeds this, it's split into submodules. This is a soft guideline enforced during refactoring phases.

Examples of splits:
- `commands/init/` → `parse.rs`, `import.rs`, `output.rs`
- `commands/fail/` → `mod.rs`, `output.rs`, `transition.rs`, `tests.rs`
- `learnings/crud/` → `create.rs`, `read.rs`, `update.rs`, `delete.rs`, `types.rs`, `output.rs`
- `learnings/recall/` → `mod.rs`, `fts.rs`, `patterns.rs`

### Output formatting convention

Every command returns a typed result struct (e.g., `NextResult`, `ListResult`, `StatsResult`). The `TextFormattable` trait provides human-readable formatting:

```rust
trait TextFormattable {
    fn format_text(&self) -> String;
}
```

The `handlers.rs` module dispatches between text and JSON output based on `--format`:

```rust
fn output_result<T: Serialize + TextFormattable>(result: &T, format: OutputFormat) {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(result).unwrap()),
        OutputFormat::Text => print!("{}", result.format_text()),
    }
}
```

This separation means command implementations never deal with output formatting -- they return data, and the handler layer formats it.

### Dependency direction

```
main.rs → cli/ → commands/ → db/
                     ↓         ↓
                  models/ ← learnings/
                     ↓
                  error.rs

loop_engine/ → commands/ (library calls, not subprocess)
             → db/
             → models/
```

The loop engine calls command functions directly as library calls, not by spawning `task-mgr` subprocesses. This provides compile-time type checking and avoids the fragile output parsing that plagued the bash implementation.

---

## What This Is NOT

- **Not a project management tool**: task-mgr manages agent loop iterations, not human sprints. There are no assignees, estimates, or boards.
- **Not a CI/CD system**: It orchestrates AI agent work, not build pipelines. The loop engine spawns Claude, not Docker containers.
- **Not a database migration tool**: The `migrate` command manages task-mgr's own schema, not your application's database.
- **Not a generic task runner**: Tasks represent PRD user stories with acceptance criteria, dependencies, and file references -- not arbitrary shell commands.
