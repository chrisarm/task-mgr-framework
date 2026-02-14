# Learnings System Guide

The learnings system is task-mgr's institutional memory. It captures knowledge from task execution and recalls it when working on similar tasks, helping AI agents avoid repeating mistakes and reuse successful patterns.

## Table of Contents

- [Concept: Institutional Memory](#concept-institutional-memory)
- [Learning Outcomes](#learning-outcomes)
- [Pattern Matching and Recall](#pattern-matching-and-recall)
- [Recording Learnings](#recording-learnings)
- [Best Practices](#best-practices)
- [Tag Taxonomy](#tag-taxonomy)
- [Examples](#examples)
- [Ranking and Prioritization](#ranking-and-prioritization)

## Concept: Institutional Memory

Traditional agent loops lose context between iterations. Each invocation starts fresh, unaware of patterns discovered or mistakes made in previous runs. This leads to:

- **Repeated failures**: The same error conditions are hit over and over
- **Lost patterns**: Useful approaches are discovered but forgotten
- **Wasted effort**: Time spent rediscovering what was already known

The learnings system solves this by persisting knowledge across iterations:

```
┌─────────────────────────────────────────────────────────────────┐
│  Iteration 1                                                     │
│  - Encounters compilation error                                  │
│  - Discovers: "SQLite requires bundled feature"                  │
│  - Records learning with --files Cargo.toml                      │
└─────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│  Iteration N (later)                                             │
│  - Task involves Cargo.toml changes                              │
│  - task-mgr next automatically includes relevant learning        │
│  - Agent knows to include bundled feature BEFORE hitting error   │
└─────────────────────────────────────────────────────────────────┘
```

### Key Benefits

1. **Failure Prevention**: Learnings from failures help avoid repeating the same mistakes
2. **Pattern Reuse**: Successful approaches are captured and suggested for similar tasks
3. **Context Accumulation**: Knowledge builds up over time, making the agent more effective
4. **Cross-Session Persistence**: Learnings survive across runs, agent restarts, and even project switches

## Learning Outcomes

Each learning is classified by outcome type, which affects how it's used and presented:

### `failure`

Captures what went wrong and how to avoid it in the future.

**When to use:**
- Compilation errors with non-obvious causes
- Runtime failures with specific fixes
- Configuration issues that blocked progress
- External dependency problems

**Key fields:**
- `root_cause`: Why this failed
- `solution`: How to fix or avoid it
- `errors`: Error patterns to match against task errors

**Example:**
```bash
task-mgr learn --outcome failure \
  --title "rusqlite bundled feature required" \
  --content "Compilation fails with missing sqlite3 symbols when building cross-platform" \
  --root-cause "rusqlite defaults to system SQLite which may not be available" \
  --solution "Add bundled feature: rusqlite = { version = \"0.32\", features = [\"bundled\"] }" \
  --files "Cargo.toml" \
  --errors "undefined reference to `sqlite3" \
  --tags "rust,dependencies,sqlite" \
  --confidence high
```

### `success`

Captures what worked well and why, enabling replication of successful approaches.

**When to use:**
- Elegant solutions that should be repeated
- Approaches that outperformed alternatives
- Techniques that solved problems cleanly

**Key fields:**
- `solution`: The successful approach
- `task_types`: Types of tasks this applies to

**Example:**
```bash
task-mgr learn --outcome success \
  --title "Transaction wrapping for multi-table updates" \
  --content "Wrapped related updates in a transaction for atomicity" \
  --solution "let tx = conn.transaction()?; /* operations */ tx.commit()?;" \
  --files "src/db/*.rs" \
  --task-types "US-,FIX-" \
  --tags "rust,sqlite,transactions" \
  --confidence high
```

### `workaround`

Documents non-ideal solutions for known issues, including why the workaround is needed.

**When to use:**
- When a proper fix isn't possible
- For known issues in dependencies
- When constraints force suboptimal solutions

**Key fields:**
- `root_cause`: Why this workaround is necessary
- `solution`: The workaround approach

**Example:**
```bash
task-mgr learn --outcome workaround \
  --title "fs2 FileExt trait needs explicit import" \
  --content "File locking methods are not available without importing the trait extension" \
  --root-cause "Rust trait methods require the trait to be in scope" \
  --solution "use fs2::FileExt; // Required for lock_exclusive() and lock_shared()" \
  --files "src/db/lock.rs" \
  --errors "no method named `lock_exclusive` found" \
  --tags "rust,traits,fs2" \
  --confidence high
```

### `pattern`

Captures reusable patterns and best practices discovered during development.

**When to use:**
- Recurring code structures that work well
- Architectural patterns worth applying elsewhere
- Testing patterns
- Error handling patterns

**Key fields:**
- `content`: Description of the pattern
- `solution`: How to implement it

**Example:**
```bash
task-mgr learn --outcome pattern \
  --title "Command result struct pattern" \
  --content "Each command returns a structured result type with a format_text() function for human output" \
  --solution "struct XxxResult {...} fn format_text(result: &XxxResult) -> String {...}" \
  --files "src/commands/*.rs" \
  --task-types "US-" \
  --tags "rust,cli,patterns" \
  --confidence high
```

## Pattern Matching and Recall

Learnings are recalled based on pattern matching against the current task. The system uses multiple signals to find relevant learnings:

### File Pattern Matching

Learnings specify file patterns using glob syntax:

| Pattern | Matches |
|---------|---------|
| `src/main.rs` | Exact file match |
| `src/*.rs` | All .rs files in src/ |
| `src/db/*.rs` | All .rs files in src/db/ |
| `*.rs` | Any .rs file anywhere |
| `*/db/*` | Any file in any db/ directory |

**Scoring**: File matches contribute +10 points to relevance score.

### Task Type Matching

Learnings can specify task type prefixes they apply to:

| Learning applies_to_task_types | Task ID | Matches? |
|--------------------------------|---------|----------|
| `["US-"]` | `US-001` | Yes |
| `["FIX-", "BUG-"]` | `FIX-123` | Yes |
| `["TECH-"]` | `US-001` | No |

**Scoring**: Task type matches contribute +5 points to relevance score.

### Error Pattern Matching

Learnings can specify error patterns to match against task error messages:

```bash
--errors "E0001,undefined reference,cannot find"
```

When a task has a `last_error` field, learnings with matching error patterns are boosted.

**Scoring**: Error matches contribute +2 points to relevance score.

### Automatic Recall with `next`

The `task-mgr next` command automatically includes relevant learnings:

```bash
$ task-mgr --format json next --claim --run-id $RUN_ID

{
  "task": {
    "id": "US-015",
    "title": "Add database migration",
    "touches_files": ["src/db/migrations.rs"]
  },
  "learnings": [
    {
      "id": 3,
      "title": "Use transactions for schema changes",
      "content": "Wrap DDL statements in transactions for rollback on failure"
    },
    {
      "id": 7,
      "title": "SQLite ALTER TABLE limitations",
      "content": "SQLite doesn't support DROP COLUMN; recreate table instead"
    }
  ]
}
```

### Manual Recall

Query learnings directly for research or debugging:

```bash
# Find learnings for a specific task
task-mgr recall --for-task US-015

# Search by text
task-mgr recall --query "database migration"

# Filter by tags
task-mgr recall --tags "sqlite,schema"

# Filter by outcome
task-mgr recall --outcome failure

# Combine filters
task-mgr recall --for-task US-015 --tags "rust" --limit 10
```

## Recording Learnings

### From the Agent Prompt

The recommended approach is to record learnings directly when they're discovered:

```bash
# After fixing a compilation error
task-mgr learn --outcome failure \
  --title "Short description of what failed" \
  --content "Detailed explanation with context" \
  --task-id "$CURRENT_TASK_ID" \
  --run-id "$RUN_ID" \
  --root-cause "Why this happened" \
  --solution "How to fix or avoid it" \
  --files "path/to/relevant/*.rs" \
  --tags "relevant,tags,here" \
  --confidence high
```

### Required Fields

- `--outcome`: One of `failure`, `success`, `workaround`, `pattern`
- `--title`: Short summary (used in recall listings)
- `--content`: Detailed description

### Optional Fields

| Field | Purpose | Example |
|-------|---------|---------|
| `--task-id` | Link to originating task | `US-001` |
| `--run-id` | Link to run session | `abc123` |
| `--root-cause` | Why it happened | `Missing dependency` |
| `--solution` | How to fix/implement | `Add X to Cargo.toml` |
| `--files` | File patterns (comma-separated) | `src/*.rs,Cargo.toml` |
| `--task-types` | Task type prefixes | `US-,FIX-` |
| `--errors` | Error patterns to match | `E0001,undefined reference` |
| `--tags` | Categorization tags | `rust,sqlite,error` |
| `--confidence` | Reliability: `high`, `medium`, `low` | `high` |

### Editing and Deleting Learnings

```bash
# Edit a learning
task-mgr edit-learning 42 \
  --title "Updated title" \
  --add-tags "new-tag" \
  --remove-tags "old-tag" \
  --confidence high

# Delete a learning
task-mgr delete-learning 42 --yes

# List all learnings
task-mgr learnings

# List recent learnings
task-mgr learnings --recent 10
```

## Best Practices

### When to Record Learnings

**DO record:**
- Non-obvious solutions to errors
- Patterns that will recur
- Gotchas that cost significant time
- Dependencies or configuration requirements
- Workarounds for known issues

**DON'T record:**
- Trivial fixes (typos, obvious bugs)
- One-off issues unlikely to recur
- Task-specific implementation details
- Information already in documentation

### Writing Effective Titles

Titles appear in recall listings, so they should be:

| Good Title | Bad Title |
|------------|-----------|
| `SQLite bundled feature required for cross-platform` | `Fixed compilation error` |
| `Use COALESCE for nullable aggregates` | `Database issue` |
| `fs2 trait import needed for file locking` | `Import fix` |

### Writing Effective Content

Content should include:
1. **What happened**: The problem or discovery
2. **Why it happened**: Root cause or context
3. **How to handle it**: Solution, workaround, or pattern

**Good content:**
```
SQLite SUM with CASE returns NULL on empty tables, not 0.
This causes issues when computing statistics on fresh databases.
Use COALESCE(SUM(...), 0) to handle the empty case.
```

**Poor content:**
```
Fixed the stats command.
```

### Choosing Confidence Levels

| Level | Meaning | When to Use |
|-------|---------|-------------|
| `high` | Verified, tested, reliable | Solution worked and was verified |
| `medium` | Likely correct, worked once | Solution worked but not extensively tested |
| `low` | Tentative, might not generalize | Workaround that might not apply elsewhere |

### Specifying File Patterns

Use specific patterns to avoid noise:

| Too Broad | Too Narrow | Just Right |
|-----------|------------|------------|
| `*.rs` | `src/db/migrations.rs` | `src/db/*.rs` |
| `*` | `Cargo.toml` | `Cargo.toml` (for dependency issues) |
| `src/*` | `src/commands/init.rs` | `src/commands/*.rs` |

### Choosing Task Types

Only specify task types if the learning specifically applies to certain work:

| Applies To | task_types Value |
|------------|------------------|
| All implementation tasks | `["US-"]` |
| Bug fixes | `["FIX-", "BUG-"]` |
| Testing work | `["TEST-"]` |
| Technical debt | `["TECH-"]` |
| All tasks | Don't specify (leave empty) |

## Tag Taxonomy

Consistent tagging improves recall quality. Use these conventions:

### Language Tags

| Tag | For |
|-----|-----|
| `rust` | Rust-specific issues and patterns |
| `sql` | SQL queries and database logic |
| `bash` | Shell scripts |
| `json` | JSON parsing/generation |

### Domain Tags

| Tag | For |
|-----|-----|
| `cli` | Command-line interface |
| `database` | Database operations |
| `async` | Asynchronous code |
| `testing` | Test code |
| `serialization` | Serde, JSON encoding |
| `error-handling` | Error types, propagation |
| `file-io` | File system operations |

### Concept Tags

| Tag | For |
|-----|-----|
| `transactions` | Database transactions |
| `concurrency` | Thread safety, locking |
| `migrations` | Schema migrations |
| `patterns` | Design patterns |
| `traits` | Rust traits |
| `lifetimes` | Rust lifetimes |

### Issue Type Tags

| Tag | For |
|-----|-----|
| `compiler-error` | Rust compilation errors |
| `runtime-error` | Runtime failures |
| `performance` | Performance issues |
| `deprecation` | API deprecation |
| `compatibility` | Cross-platform issues |

### Module Tags

Use module names from the project structure:

| Tag | For |
|-----|-----|
| `commands` | src/commands/ modules |
| `db` | src/db/ modules |
| `models` | src/models/ modules |
| `learnings` | src/learnings/ modules |

## Examples

### Example: Failure Learning from Compilation Error

```bash
task-mgr learn --outcome failure \
  --title "chrono DateTime parsing requires RFC3339 format" \
  --content "SQLite stores timestamps in ISO8601 format which chrono can parse, but the DateTime::parse_from_str function requires an explicit format. Use DateTime::parse_from_rfc3339 for standard timestamps." \
  --task-id "US-008" \
  --run-id "abc123" \
  --root-cause "Using wrong parsing function for timestamp format" \
  --solution "DateTime::parse_from_rfc3339(s).map(|dt| dt.with_timezone(&Utc))" \
  --files "src/models/*.rs" \
  --task-types "US-" \
  --errors "ParsingError" \
  --tags "rust,chrono,datetime,parsing" \
  --confidence high
```

### Example: Pattern Learning from Code Review

```bash
task-mgr learn --outcome pattern \
  --title "Result struct with format_text() for commands" \
  --content "Each command module defines a result struct (e.g., ListResult, ShowResult) and a format_text() function that converts it to human-readable output. main.rs handles format selection." \
  --solution "pub struct XxxResult { ... }\npub fn format_text(result: &XxxResult) -> String { ... }" \
  --files "src/commands/*.rs" \
  --task-types "US-" \
  --tags "rust,cli,patterns,commands" \
  --confidence high
```

### Example: Workaround for SQLite Limitation

```bash
task-mgr learn --outcome workaround \
  --title "SQLite DROP COLUMN requires table recreation" \
  --content "SQLite doesn't support ALTER TABLE DROP COLUMN directly. Must recreate the table to remove columns." \
  --root-cause "SQLite has limited ALTER TABLE support" \
  --solution "CREATE TABLE new_table AS SELECT (needed columns) FROM old_table; DROP TABLE old_table; ALTER TABLE new_table RENAME TO old_table;" \
  --files "src/db/migrations.rs" \
  --task-types "US-,TECH-" \
  --tags "sqlite,schema,migrations,workaround" \
  --confidence high
```

### Example: Success Learning from Optimization

```bash
task-mgr learn --outcome success \
  --title "Use CTE for complex task selection query" \
  --content "The smart task selection query was simplified and made more readable by using Common Table Expressions (CTEs) to separate the scoring logic from the final selection." \
  --solution "WITH scored_tasks AS (SELECT ... FROM tasks WHERE ...) SELECT * FROM scored_tasks ORDER BY score DESC LIMIT 1" \
  --files "src/commands/next.rs" \
  --task-types "US-,PERF-" \
  --tags "sql,sqlite,optimization,patterns" \
  --confidence high
```

## Ranking and Prioritization

### Most Recently Useful First

Learnings are ordered by `last_applied_at` (descending), which tracks when a learning was last marked as useful. This ensures recently helpful learnings appear first.

```sql
ORDER BY
  CASE WHEN last_applied_at IS NULL THEN 1 ELSE 0 END,
  last_applied_at DESC,
  created_at DESC
```

### Relevance Scoring (Task-Based Recall)

When recalling for a specific task (`--for-task`), learnings are scored:

| Match Type | Points |
|------------|--------|
| File pattern match | +10 |
| Task type prefix match | +5 |
| Error pattern match | +2 |

Higher-scoring learnings appear first.

### Tracking Usage

The system tracks how often learnings are shown and applied:

| Field | Meaning |
|-------|---------|
| `times_shown` | How many times this learning was returned by recall |
| `times_applied` | How many times it was marked as useful |
| `last_shown_at` | When it was last shown |
| `last_applied_at` | When it was last applied |

### Future: UCB Bandit Ranking

A future enhancement (Phase 2) may add UCB (Upper Confidence Bound) bandit ranking, which balances:

- **Exploitation**: Showing learnings that have been helpful in the past
- **Exploration**: Occasionally showing new learnings to assess their value

This creates a feedback loop that improves ranking over time based on actual usefulness. Note: This is an optional enhancement that may not be present in all versions.
