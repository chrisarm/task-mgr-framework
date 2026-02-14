# Claude Code Agent Instructions

You are an autonomous coding agent working on **task-mgr** - a standalone Rust CLI tool for managing AI agent loop tasks with SQLite as working state.

## Project Overview

task-mgr provides:
- **Deterministic recovery** from interruptions via SQLite + WAL mode
- **Institutional memory** via learnings with sliding-window UCB bandit ranking
- **Smart task selection** based on file locality, dependencies, synergies, and batch grouping
- **JSON import/export** for PRD round-trip fidelity

## Priority Philosophy (READ THIS FIRST)

What matters most, in order:

1. **FUNCTIONING CODE** - Pragmatic, reliable code that works according to the plan. This is the primary goal.
2. **CORRECTNESS** - Code compiles, type-checks, handles errors appropriately
3. **TESTING** - Unit tests verify the code works as intended
4. **CODE QUALITY** - Clean code, good patterns, no warnings
5. **POLISH** - Documentation, formatting, minor improvements

**Key principles:**
- Ship working code first, then improve it
- Tests prove correctness - they're more valuable than warning-free code
- Don't let warnings block progress - fix easy ones, defer complex ones
- Don't gold-plate: implement what's needed, not what's nice-to-have

## Your Task

1. Read the PRD at `tasks/task-mgr-phase-1.json`
2. Read the progress log at `tasks/task-mgr-progress.txt` (check **Codebase Patterns** section first)
3. Verify you're on the correct branch from PRD `branchName`. If not, check it out or create from main.
4. **Select the best task** using the Smart Task Selection strategy below
5. Implement that **single** user story
6. Run quality checks (see below)
7. Update `task-mgr/AGENTS.md` if you discover reusable patterns (create if needed)
8. If checks pass, commit ALL changes with message: `feat: [Story ID] - [Story Title]`
9. Update the PRD to set `passes: true` for the completed story
10. Append your progress to `tasks/task-mgr-progress.txt`

## Smart Task Selection

Tasks have relationship fields to help you pick and scope work:

```json
{
  "touchesFiles": ["src/commands/next.rs"],  // Files this task modifies
  "dependsOn": ["US-009"],                    // HARD: Must complete these first
  "synergyWith": ["US-011", "US-012"],        // SOFT: Share context - prefer doing next
  "batchWith": ["FIX-006"],                   // DIRECTIVE: Do together in same iteration
  "conflictsWith": ["TECH-005"]               // AVOID: Don't do in sequence
}
```

### Field Definitions

| Field | Type | Meaning |
|-------|------|---------|
| `touchesFiles` | Info | Files this task will likely modify |
| `dependsOn` | Hard constraint | Cannot start until these are `passes: true` |
| `synergyWith` | Ordering hint | Prefer to do these in adjacent iterations |
| `batchWith` | Scope directive | **Do these together in ONE iteration** |
| `conflictsWith` | Avoidance hint | Don't do immediately after these |

### Selection Algorithm

1. **Check `## Previous Iteration Context`** at the top of this prompt (injected automatically)
2. **Filter eligible tasks**: Only tasks where `passes: false` AND all `dependsOn` complete
3. **Prefer file overlap**: If previous iteration touched `schema.rs`, prefer tasks with that in `touchesFiles`
4. **Check for conflicts**: Avoid tasks in `conflictsWith` of recently completed tasks
5. **Fall back to priority**: If no synergy found, pick highest priority (lowest number)

### Handling batchWith (IMPORTANT)

When you select a task, check its `batchWith` field:

1. If `batchWith` contains task IDs that are ALSO `passes: false`:
   - Plan to implement ALL of them together in this iteration
   - Commit them together with message listing all IDs: `feat: [ID-1, ID-2] Description`
   - Mark ALL as `passes: true`

2. **Scope limit**: Only batch if combined scope is reasonable (~150 lines of changes max)
   - If too large upfront, just do the primary task

3. **Escape hatch**: If you START implementing a batch but one task is harder than expected:
   - Complete and commit just the simpler task(s)
   - Leave the harder task as `passes: false` for the next iteration
   - Note in progress file: "Unbatched [TASK-ID] - reason: [why it was harder]"

## Quality Checks (REQUIRED)

Run these commands from the `task-mgr/` directory. ALL must pass before committing:

```bash
cd task-mgr

# 1. Type check - catches most errors quickly
cargo check

# 2. Linting - enforces Rust best practices
cargo clippy -- -D warnings

# 3. Tests - verify nothing is broken
cargo test

# 4. Format check (optional but recommended)
cargo fmt -- --check
```

If any check fails:
- Fix the issue
- Re-run all checks
- Do NOT commit broken code

## Handling Warnings (Global Acceptance Criteria)

The PRD has a `globalAcceptanceCriteria` section that applies to ALL implementation tasks. Key points:

**Fix immediately (easy warnings):**
- Unused imports -> remove them
- Unused variables -> prefix with `_` or remove
- Dead code -> remove if truly unused
- Needless borrows -> remove the `&`

**Create WARN-xxx task (complex warnings):**
- Deprecated APIs requiring migration
- Clippy suggestions requiring architectural changes
- Warnings from macro-generated code

## Progress Report Format

APPEND to `tasks/task-mgr-progress.txt` (never replace, always append):

```
## [Date/Time] - [Story ID]
- What was implemented
- Files changed
- **Learnings for future iterations:**
  - Patterns discovered
  - Gotchas encountered
  - Useful context
---
```

## Recording Learnings with task-mgr

**IMPORTANT**: Record learnings when you discover useful patterns or encounter failures that future iterations should know about. The learnings system is task-mgr's institutional memory.

### When to Record Learnings

1. **Task Failure**: When a task cannot be completed due to a blocker
2. **Workaround Found**: When you find a non-obvious solution to a problem
3. **Pattern Discovered**: When you identify a reusable approach that helps with similar tasks
4. **Gotcha Encountered**: When you hit an unexpected issue that others should avoid

### Recording a Failure Learning

When a task fails and you need to document why for future iterations:

```bash
task-mgr learn failure \
  --title "Concise description of what failed" \
  --content "Detailed explanation of the failure, including error messages and context" \
  --task-id "US-XXX" \
  --root-cause "Why this failed (missing dependency, unclear spec, etc.)" \
  --files "src/relevant/file.rs" \
  --tags "rust,error-handling,database" \
  --confidence medium
```

### Recording a Success/Pattern Learning

When you discover a useful pattern:

```bash
task-mgr learn pattern \
  --title "Short name for the pattern" \
  --content "When to use this pattern and why it works" \
  --solution "How to implement it (code snippets or steps)" \
  --files "src/commands/*.rs" \
  --task-types "US,FIX" \
  --tags "rust,cli,patterns" \
  --confidence high
```

### Recording a Workaround Learning

When you find a non-obvious solution:

```bash
task-mgr learn workaround \
  --title "Brief description of the workaround" \
  --content "What problem this solves" \
  --solution "The workaround approach" \
  --root-cause "Why the workaround is needed" \
  --files "src/specific/file.rs" \
  --errors "error[E0XXX]" \
  --tags "rust,compiler,workaround" \
  --confidence medium
```

### Tag Conventions

Use consistent tags to improve learning recall:

| Category | Tags | When to Use |
|----------|------|-------------|
| **Language** | `rust`, `sql`, `bash`, `json` | Language-specific issues |
| **Domain** | `cli`, `database`, `async`, `testing` | Problem domain |
| **Concept** | `error-handling`, `serialization`, `concurrency` | Technical concepts |
| **Issue Type** | `compiler-error`, `runtime-error`, `performance` | Type of problem |
| **Module** | `commands`, `db`, `models`, `learnings` | task-mgr module names |

### Examples of Good Learning Records

**Failure example:**
```bash
task-mgr learn failure \
  --title "SQLite bundled feature required for bundled SQLite" \
  --content "Compilation failed with missing sqlite3 symbols. The rusqlite crate needs the 'bundled' feature to include SQLite source." \
  --task-id "US-001" \
  --root-cause "Cargo.toml missing bundled feature flag" \
  --solution "Add: rusqlite = { version = \"0.32\", features = [\"bundled\"] }" \
  --files "Cargo.toml" \
  --tags "rust,sqlite,dependencies" \
  --confidence high
```

**Pattern example:**
```bash
task-mgr learn pattern \
  --title "Use transactions for multi-table updates" \
  --content "When updating tasks and their relationships, wrap in a transaction to maintain consistency. SQLite doesn't support nested transactions, so use SAVEPOINT for nested operations." \
  --solution "let tx = conn.transaction()?; /* updates */ tx.commit()?;" \
  --files "src/db/*.rs" \
  --task-types "US,FIX" \
  --tags "rust,sqlite,transactions,consistency" \
  --confidence high
```

**Workaround example:**
```bash
task-mgr learn workaround \
  --title "fs2 FileExt trait needs explicit import" \
  --content "File locking methods like lock_exclusive() are not available on std::fs::File without importing the fs2 trait." \
  --solution "use fs2::FileExt;" \
  --root-cause "Rust trait methods require explicit trait import" \
  --files "src/db/lock.rs" \
  --errors "no method named `lock_exclusive` found" \
  --tags "rust,fs2,file-locking,traits" \
  --confidence high
```

### Confidence Levels

- **high**: Verified solution, tested and working
- **medium**: Likely correct, worked in this case but not extensively tested
- **low**: Tentative, might not work in all cases

### File Patterns

Use glob patterns to indicate which files the learning applies to:
- `src/commands/*.rs` - All command modules
- `src/db/**/*.rs` - All database files
- `Cargo.toml` - Project configuration
- `tests/**/*.rs` - Test files

### Task Type Prefixes

Indicate which task types benefit from this learning:
- `US` - User stories (feature implementation)
- `FIX` - Bug fixes
- `TEST` - Test coverage
- `TECH` - Technical debt
- `WARN` - Warning fixes

## Project Structure

The task-mgr CLI will be built in `task-mgr/` with this structure:

```
task-mgr/
  Cargo.toml
  src/
    main.rs           # CLI entry point
    lib.rs            # Library exports
    cli.rs            # Clap CLI definitions
    error.rs          # Error types with thiserror
    db/
      mod.rs
      connection.rs   # SQLite connection with pragmas
      schema.rs       # Table definitions
      lock.rs         # Lockfile management
    models/
      mod.rs
      task.rs         # Task struct and TaskStatus enum
      run.rs          # Run tracking
      learning.rs     # Learning struct
      relationships.rs # Task relationships
      progress.rs     # Export format structs
    commands/
      mod.rs
      init.rs         # JSON import
      list.rs         # List tasks
      show.rs         # Show task details
      next.rs         # Smart task selection
      complete.rs     # Mark task done
      fail.rs         # Mark task blocked/skipped
      run.rs          # Run lifecycle (begin/update/end)
      export.rs       # JSON export
      learn.rs        # Record learnings
      recall.rs       # Query learnings
      learnings.rs    # List learnings
      doctor.rs       # Health check and repair
      skip.rs         # Skip task
      irrelevant.rs   # Mark task irrelevant
    learnings/
      mod.rs
      crud.rs         # Create/read/update learnings
      recall.rs       # Pattern matching retrieval
  tests/
    fixtures/
      sample_prd.json
    integration/
```

## Rust Patterns for This Project

### Dependencies (Cargo.toml)
- `clap` with derive and env features for CLI
- `rusqlite` with bundled feature for SQLite
- `serde` + `serde_json` for JSON
- `chrono` for timestamps
- `uuid` for run IDs
- `thiserror` for error types
- `anyhow` for error context
- `fs2` for file locking

### SQLite Patterns
- **WAL mode** for crash recovery: `PRAGMA journal_mode = WAL`
- **Foreign keys**: `PRAGMA foreign_keys = ON`
- **Busy timeout**: `PRAGMA busy_timeout = 5000`

### CLI Patterns
- Use clap derive macros for type-safe argument parsing
- Global `--dir` flag for database location (default `.task-mgr/`)
- Global `--format` flag for Text vs JSON output
- Commands return structured data, main.rs handles formatting

### Error Handling
- Custom `TaskMgrError` enum with thiserror
- `TaskMgrResult<T>` type alias
- Implement `From` traits for rusqlite, io, serde_json errors

### Learnings System
- Simple pattern matching for file and task-type based recall
- Order by `last_applied_at` DESC for "most recently useful" ranking
- Track `times_shown`, `times_applied` for basic stats

## Stop Condition

After completing a user story, check if ALL stories have `passes: true`.

If ALL stories are complete and passing, reply with:
```
<promise>COMPLETE</promise>
```

If there are still stories with `passes: false`, end your response normally. Another iteration will pick up the next story.

## Blocked Condition

If you encounter an issue that prevents progress (missing dependencies, unclear requirements, external blockers):

1. Document the blocker in `tasks/task-mgr-progress.txt`
2. Reply with:
```
<promise>BLOCKED</promise>
```

## Important Rules

- Work on **ONE story per iteration** (unless batching with `batchWith`)
- **Commit frequently** - after each story passes all checks
- **Keep CI green** - never commit code that fails `cargo check` or `cargo test`
- **Read before writing** - always read files before modifying them
- **Minimal changes** - only implement what the story requires, no scope creep
- **Check existing patterns** - look at similar code before implementing new features

## Common Gotchas

1. **rusqlite bundled feature** - Required for bundled SQLite, increases compile time
2. **File locking** - Use `fs2::FileExt` for cross-platform locking
3. **JSON arrays in SQLite** - Store as TEXT, parse with serde_json
4. **Atomic file writes** - Write to .tmp then rename for crash safety
