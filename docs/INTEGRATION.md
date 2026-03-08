# Shell Integration Guide

This guide explains how to integrate task-mgr into agent loops like Claude Code, including the full workflow, command usage, error handling, and crash recovery.

## Table of Contents

- [Overview](#overview)
- [Full Workflow](#full-workflow)
- [Command Reference for Integration](#command-reference-for-integration)
- [Reference Implementation: claude-loop.sh](#reference-implementation-claude-loopsh)
- [Error Handling and Recovery](#error-handling-and-recovery)
- [Parsing JSON Output](#parsing-json-output)
- [Troubleshooting](#troubleshooting)
- [Helper Scripts](#helper-scripts)
- [Git Worktrees](#git-worktrees)
- [Task Completion Detection](#task-completion-detection)
- [Future: Loop-Driven Task Presentation](#future-loop-driven-task-presentation)

## Overview

task-mgr provides deterministic state management for AI agent loops. The key benefits for shell integration:

1. **Crash Recovery**: SQLite + WAL mode ensures state is never lost
2. **Smart Selection**: Automated task prioritization based on dependencies, file locality, and synergies
3. **Institutional Memory**: Learnings persist across iterations and can be recalled for relevant tasks
4. **JSON Round-Trip**: Export after each iteration enables recovery from any failure
5. **Git Worktrees**: Isolates work for each branch, avoiding conflicts with uncommitted changes

### Core Concepts

| Concept | Description |
|---------|-------------|
| **Run** | A session of agent loop execution (begin → iterate → end) |
| **Task** | A unit of work with status, dependencies, and metadata |
| **Learning** | Knowledge captured from task outcomes for future iterations |
| **Iteration** | A single agent invocation working on one task |
| **Worktree** | An isolated git checkout for a branch (default in `task-mgr loop`) |

## Full Workflow

The typical integration follows this lifecycle:

```
┌─────────────────────────────────────────────────────────────────┐
│  1. INITIALIZATION                                              │
│     task-mgr init --from-json prd.json                          │
│     OR                                                          │
│     task-mgr doctor --auto-fix  (if DB exists)                  │
└─────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│  2. START RUN                                                   │
│     RUN_ID=$(task-mgr run begin --format json | jq -r '.run_id')│
└─────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│  3. ITERATION LOOP                                              │
│     ┌─────────────────────────────────────────────────────────┐ │
│     │  a. Get next task with claim:                           │ │
│     │     task-mgr next --claim --run-id $RUN_ID              │ │
│     │                                                         │ │
│     │  b. Execute agent with task context                     │ │
│     │                                                         │ │
│     │  c. Handle outcome:                                     │ │
│     │     - Success: task-mgr complete TASK_ID                │ │
│     │     - Blocked: task-mgr fail TASK_ID --error "reason"   │ │
│     │     - Skip:    task-mgr skip TASK_ID --reason "reason"  │ │
│     │                                                         │ │
│     │  d. Export state for crash recovery:                    │ │
│     │     task-mgr export --to-json prd.json                  │ │
│     └─────────────────────────────────────────────────────────┘ │
│                              ↑                                  │
│                              └── repeat until done/max iters    │
└─────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│  4. END RUN                                                     │
│     task-mgr run end --run-id $RUN_ID --status completed        │
│     task-mgr export --to-json prd.json                          │
└─────────────────────────────────────────────────────────────────┘
```

## Command Reference for Integration

### Initialization

```bash
# First run: Initialize from PRD JSON file
task-mgr init --from-json tasks/project.json

# Subsequent runs: Sync PRD changes into existing database
task-mgr init --from-json tasks/project.json --append --update-existing

# Health check: Fix stale state (e.g., tasks stuck in_progress)
task-mgr doctor --auto-fix
```

### Run Lifecycle

```bash
# Begin a new run session
# Returns: {"run_id": "abc123", "iteration": 0}
task-mgr --format json run begin

# Update run with progress metadata
task-mgr run update --run-id "$RUN_ID" \
  --last-commit "abc123" \
  --last-files "src/main.rs,src/lib.rs"

# End the run session
task-mgr run end --run-id "$RUN_ID" --status completed
# Or if aborted:
task-mgr run end --run-id "$RUN_ID" --status aborted
```

### Task Selection and Claiming

```bash
# Get next recommended task (read-only)
task-mgr next

# Claim the task (sets status to in_progress)
task-mgr next --claim --run-id "$RUN_ID"

# Consider file locality from previous iteration
task-mgr next --claim --run-id "$RUN_ID" --after-files "src/db/*.rs"

# Disable automatic decay of blocked/skipped tasks
task-mgr next --claim --run-id "$RUN_ID" --decay-threshold 0
```

The `next` command returns both the task and relevant learnings:

```json
{
  "task": {
    "id": "US-001",
    "title": "Implement feature X",
    "description": "...",
    "priority": 10,
    "touches_files": ["src/main.rs"],
    "depends_on": [],
    "synergy_with": ["US-002"]
  },
  "learnings": [
    {
      "id": 1,
      "title": "Watch out for edge case Y",
      "content": "...",
      "tags": ["rust", "edge-case"]
    }
  ],
  "selection_reason": "Highest priority eligible task with file synergy"
}
```

### Task Completion

```bash
# Mark task as done
task-mgr complete US-001 --run-id "$RUN_ID"

# Include commit hash for traceability
task-mgr complete US-001 --run-id "$RUN_ID" --commit "abc123"

# Complete multiple tasks at once
task-mgr complete US-001 US-002 US-003 --run-id "$RUN_ID"

# Force completion (skip status validation)
task-mgr complete US-001 --force
```

### Task Failure/Skip

```bash
# Mark as blocked (default)
task-mgr fail US-001 --error "Missing dependency X" --run-id "$RUN_ID"

# Mark as skipped (deferred for later)
task-mgr skip US-001 --reason "Needs clarification" --run-id "$RUN_ID"

# Mark as irrelevant (requirements changed)
task-mgr irrelevant US-001 --reason "Feature cancelled" --run-id "$RUN_ID"
```

### Recovery Commands

```bash
# Return blocked task to todo
task-mgr unblock US-001

# Return skipped task to todo
task-mgr unskip US-001

# Reset any task to todo (for re-running)
task-mgr reset US-001

# Reset all non-todo tasks
task-mgr reset --all --yes
```

### Export for Crash Recovery

```bash
# Export current state back to PRD JSON
task-mgr export --to-json tasks/project.json

# Also export learnings separately
task-mgr export --to-json tasks/project.json --learnings-file tasks/learnings.json
```

### Recording Learnings

```bash
# Record a failure learning
task-mgr learn --outcome failure \
  --title "SQLite requires bundled feature" \
  --content "Cross-platform compilation requires bundled SQLite" \
  --task-id US-001 \
  --run-id "$RUN_ID" \
  --solution "Add rusqlite with bundled feature to Cargo.toml" \
  --tags "rust,sqlite,dependencies" \
  --files "Cargo.toml"

# Record a pattern learning
task-mgr learn --outcome pattern \
  --title "Use COALESCE for nullable aggregates" \
  --content "SQLite SUM with CASE returns NULL on empty tables" \
  --tags "sql,sqlite" \
  --confidence high
```

### Querying Learnings

```bash
# Get learnings for current task
task-mgr recall --for-task US-001

# Search learnings by text
task-mgr recall --query "database connection"

# Filter by tags
task-mgr recall --tags "rust,error"
```

### Invalidating Learnings

When a learning turns out to be wrong or harmful, use `invalidate-learning` to degrade it via two-step degradation:

```bash
# First call: downgrades confidence to Low (regardless of current level)
task-mgr invalidate-learning 42
# Output: Invalidated learning #42: "Title" (confidence: high -> low)

# Second call: retires the learning (soft-archives it)
task-mgr invalidate-learning 42
# Output: Retired learning #42: "Title" (was already low confidence)

# JSON output for scripting
task-mgr --format json invalidate-learning 42
# {"learning_id":42,"title":"...","previous_confidence":"high","action":"downgraded","new_confidence":"low"}
```

**Two-step behavior:**
1. **First invalidation**: Sets confidence to `Low`, keeping the learning visible but deprioritized
2. **Second invalidation**: Sets `retired_at` timestamp, soft-archiving the learning so it no longer appears in recall results

**Error cases:**
- Non-existent learning ID returns a `NotFound` error (non-zero exit code)
- Already-retired learning returns an `InvalidState` error (non-zero exit code)

## Reference Implementation: claude-loop.sh

The `scripts/claude-loop.sh` script is a complete reference implementation. Key features:

### Basic Usage

```bash
# Run with default settings (10 iterations)
./scripts/claude-loop.sh 10 tasks/my-project.json

# Run with custom prompt file
./scripts/claude-loop.sh 20 tasks/my-project.json scripts/custom-prompt.md

# Run non-interactively
./scripts/claude-loop.sh -y 10 tasks/my-project.json
```

### Steering Features

The loop supports runtime steering via special files:

```bash
# Inject guidance into next iteration
echo "Focus on test coverage" > .task-mgr/steering.md

# Pause loop for interactive input
touch .task-mgr/.pause

# Stop loop gracefully after current iteration
touch .task-mgr/.stop
```

### Key Script Sections

**Initialization** (lines 177-209):
```bash
initialize_database() {
  if [ -f "$db_file" ]; then
    # Existing database: run doctor check
    task-mgr --dir "$TASK_MGR_DIR" doctor --auto-fix
    # Sync with PRD file
    task-mgr --dir "$TASK_MGR_DIR" init --from-json "$PRD_FILE" --append --update-existing
  else
    # Fresh initialization
    task-mgr --dir "$TASK_MGR_DIR" init --from-json "$PRD_FILE"
  fi
}
```

**Task Claiming** (lines 375-400):
```bash
# Get next task with claim and file locality
next_output=$(task-mgr --dir "$TASK_MGR_DIR" --format json next \
  --claim --run-id "$RUN_ID" --after-files "$LAST_FILES")

# Extract task and learnings for prompt
CURRENT_TASK_ID=$(echo "$next_output" | jq -r '.task.id')
task_json=$(echo "$next_output" | jq -c '.task')
learnings_json=$(echo "$next_output" | jq -c '.learnings')
```

**Completion Detection** (lines 463-500):
```bash
# Check for COMPLETE marker
if grep -q "<promise>COMPLETE</promise>" "$OUTPUT_FILE"; then
  task-mgr complete "$CURRENT_TASK_ID" --run-id "$RUN_ID"
  exit 0
fi

# Check for BLOCKED marker
if grep -q "<promise>BLOCKED</promise>" "$OUTPUT_FILE"; then
  task-mgr fail "$CURRENT_TASK_ID" --error "Blocked" --run-id "$RUN_ID"
  continue  # Move to next task
fi

# Check git for commit evidence
if git log --oneline -1 | grep -q "\[$CURRENT_TASK_ID\]"; then
  commit_hash=$(git rev-parse HEAD)
  task-mgr complete "$CURRENT_TASK_ID" --run-id "$RUN_ID" --commit "$commit_hash"
fi
```

**Export After Every Iteration** (lines 509-512):
```bash
# CRITICAL: Export state after every iteration for crash recovery
task-mgr --dir "$TASK_MGR_DIR" export --to-json "$PRD_FILE"
```

**Cleanup Trap** (lines 140-166):
```bash
cleanup() {
  # Export state on any exit
  task-mgr export --to-json "$PRD_FILE" 2>/dev/null || true

  # End run with appropriate status
  local status="aborted"
  if [ "$GRACEFUL_STOP" = true ]; then
    status="completed"
  fi
  task-mgr run end --run-id "$RUN_ID" --status "$status" 2>/dev/null || true
}
trap cleanup EXIT
```

## Error Handling and Recovery

### Stale In-Progress Tasks

If an iteration crashes mid-task, the task remains `in_progress`. The doctor command detects and fixes this:

```bash
# Check for issues
task-mgr doctor

# Auto-fix stale tasks (resets to todo)
task-mgr doctor --auto-fix
```

### Database Corruption

If the database becomes corrupted:

```bash
# Backup current state (if possible)
./scripts/backup-db.sh

# Rebuild from canonical JSON
./scripts/rebuild-from-json.sh tasks/my-project.json

# WARNING: Learnings are lost in rebuild!
# Consider exporting learnings first if database is readable
task-mgr export --to-json /dev/null --learnings-file learnings-backup.json
```

### Recovering from Crashes

The export-after-every-iteration pattern ensures minimal data loss:

1. Crash occurs mid-iteration
2. Restart the loop
3. `doctor --auto-fix` resets the stale in_progress task
4. Loop continues from where it left off

```bash
# Recovery sequence
task-mgr doctor --auto-fix        # Fix stale tasks
./scripts/claude-loop.sh 10 tasks/project.json  # Resume
```

### Handling Blocked Tasks

Blocked tasks don't automatically retry. To review and unblock:

```bash
# List blocked tasks
task-mgr list --status blocked

# Review interactively (JSON mode for scripting)
task-mgr --format json review --blocked

# Unblock specific task
task-mgr unblock US-001

# Unblock all blocked tasks
task-mgr review --blocked --auto
```

### Automatic Decay

By default, blocked/skipped tasks automatically return to todo after 32 iterations:

```bash
# Check tasks approaching decay
task-mgr doctor --decay-threshold 32

# Disable decay for this run
task-mgr next --claim --run-id "$RUN_ID" --decay-threshold 0
```

## Parsing JSON Output

All commands support `--format json` for machine-readable output.

### Extracting Task Information

```bash
# Get next task ID
TASK_ID=$(task-mgr --format json next | jq -r '.task.id')

# Get task details
task-mgr --format json show US-001 | jq '.title, .status, .priority'

# Count tasks by status
task-mgr --format json list | jq 'group_by(.status) | map({status: .[0].status, count: length})'
```

### Extracting Run Information

```bash
# Start run and capture ID
RUN_ID=$(task-mgr --format json run begin | jq -r '.run_id')

# Get run history
task-mgr --format json history --limit 5 | jq '.[].run_id'

# Get detailed run info
task-mgr --format json history --run-id "$RUN_ID" | jq '.tasks_completed'
```

### Extracting Learnings

```bash
# Get learnings for a task
task-mgr --format json recall --for-task US-001 | jq '.[].title'

# Count learnings by outcome
task-mgr --format json learnings | jq 'group_by(.outcome) | map({outcome: .[0].outcome, count: length})'
```

### Stats and Progress

```bash
# Get completion percentage
task-mgr --format json stats | jq '.completion_percentage'

# Get task counts
task-mgr --format json stats | jq '.tasks | to_entries | map("\(.key): \(.value)") | .[]'
```

### Error Handling in JSON

When an error occurs, the JSON output includes an error field:

```bash
output=$(task-mgr --format json next 2>&1)

if echo "$output" | jq -e '.error' > /dev/null 2>&1; then
  error_msg=$(echo "$output" | jq -r '.error')
  if [[ "$error_msg" == *"No eligible tasks"* ]]; then
    echo "All tasks complete!"
    exit 0
  else
    echo "Error: $error_msg" >&2
    exit 1
  fi
fi
```

## Troubleshooting

### "No eligible tasks found"

**Cause**: All tasks are complete, blocked, skipped, or have unmet dependencies.

**Solution**:
```bash
# Check what's blocking progress
task-mgr list --status blocked
task-mgr list --status skipped

# Check dependency status
task-mgr show US-001  # Look at depends_on field

# Unblock or unskip tasks
task-mgr unblock US-001
task-mgr unskip US-002
```

### "Task is in_progress"

**Cause**: Previous iteration crashed before completing the task.

**Solution**:
```bash
task-mgr doctor --auto-fix
```

### "Invalid transition: todo -> done"

**Cause**: Trying to complete a task without claiming it first.

**Solution**:
```bash
# Use --force to override
task-mgr complete US-001 --force

# Or claim first, then complete
task-mgr next --claim  # Claims the next task
task-mgr complete US-001
```

### "Database is locked"

**Cause**: Another process is holding the database lock.

**Solution**:
```bash
# Check for stale lock file
ls -la .task-mgr/tasks.db.lock

# Remove if stale (no process holding it)
rm .task-mgr/tasks.db.lock

# If another process is running, wait or kill it
```

### "JSON parse error"

**Cause**: Invalid JSON in PRD file.

**Solution**:
```bash
# Validate JSON
jq empty tasks/project.json

# Fix JSON errors before importing
```

### Learnings Not Appearing

**Cause**: Learnings may not match the current task's files or tags.

**Solution**:
```bash
# Check all learnings
task-mgr learnings

# Search broadly
task-mgr recall --query "error"

# Check specific task recall
task-mgr recall --for-task US-001 --limit 10
```

### Export Not Updating PRD

**Cause**: Export may fail silently in cleanup.

**Solution**:
```bash
# Run export explicitly with error output
task-mgr export --to-json tasks/project.json

# Check file was updated
ls -la tasks/project.json
```

## Helper Scripts

task-mgr includes helper scripts in `scripts/`:

| Script | Purpose |
|--------|---------|
| `claude-loop.sh` | Full agent loop implementation |
| `backup-db.sh` | Backup database with timestamp |
| `rebuild-from-json.sh` | Recreate DB from JSON (loses learnings) |
| `prompt.md` | Template prompt with learning instructions |

### Backup Before Risky Operations

```bash
# Always backup before experiments
./scripts/backup-db.sh

# Backups stored in .task-mgr/backups/
ls .task-mgr/backups/
# tasks.db.2024-01-15-143022
# tasks.db.2024-01-15-160045
```

### Recovery from Backup

```bash
# If you need to restore
cp .task-mgr/backups/tasks.db.2024-01-15-143022 .task-mgr/tasks.db
```

## Git Worktrees

By default, `task-mgr loop` uses git worktrees to isolate work for each PRD branch. This provides significant benefits for development workflows.

### How Worktrees Work

When you run `task-mgr loop tasks/my-project.json`, the engine:

1. Reads the branch name from the PRD metadata
2. Creates a worktree at `{repo-parent}/{repo-name}-worktrees/{sanitized-branch}/`
3. Runs Claude Code in the worktree directory (not your main checkout)
4. Database and PRD files remain in the original repo

For example:
```
/home/user/myproject/                    # Your main checkout
/home/user/myproject-worktrees/
  └── feature-auth/                      # Worktree for feature/auth branch
```

### Benefits

| Benefit | Description |
|---------|-------------|
| **No dirty tree conflicts** | Uncommitted changes in your main repo won't block the loop |
| **Parallel execution** | Multiple loops can run on different branches simultaneously |
| **Isolation** | Changes in worktrees don't affect your main working directory |
| **Shared objects** | Git worktrees share the `.git/objects` database, saving disk space |

### Disabling Worktrees

Use `--no-worktree` to revert to the old branch-checkout behavior:

```bash
# Use branch checkout instead of worktrees
task-mgr loop tasks/my-project.json --yes --no-worktree
```

This is useful for:
- CI/CD environments where isolation isn't needed
- When you want changes to appear in your main checkout immediately
- Debugging issues with worktree setup

### Worktree Management

Worktrees persist after the loop completes for reuse. To clean up:

```bash
# List all worktrees
git worktree list

# Remove a specific worktree
git worktree remove /path/to/worktree

# Prune stale worktree references
git worktree prune
```

### Path Considerations for Shell Integration

When using the built-in loop (`task-mgr loop`), worktrees are handled automatically. However, if you're building a custom shell integration:

1. **PRD and prompt files**: Always resolve relative to the **original repo** (source_root)
2. **Database**: Always stored in **original repo** at `.task-mgr/tasks.db`
3. **Claude execution**: Runs in the **worktree** (working_root)
4. **Git operations**: Run in the **worktree** for commits, in **original repo** for worktree management

```bash
# Example: Custom integration with worktree awareness
SOURCE_ROOT=$(git rev-parse --show-toplevel)
WORKTREE_ROOT="/path/to/worktree"

# PRD files are in source root
task-mgr --dir "$SOURCE_ROOT/.task-mgr" init --from-json "$SOURCE_ROOT/tasks/prd.json"

# Claude runs in worktree
cd "$WORKTREE_ROOT" && claude --print ...

# Git commits happen in worktree
cd "$WORKTREE_ROOT" && git add . && git commit -m "Task complete"
```

### Troubleshooting Worktrees

**"fatal: already in a worktree"**

You're running task-mgr from inside a worktree for a different branch. Either:
- Run from the main repository
- Run from the worktree for the correct branch

**"Worktree exists but is on wrong branch"**

The computed worktree path already exists but is checked out to a different branch:
```bash
# Remove the conflicting worktree
git worktree remove /path/to/worktree

# Then retry
task-mgr loop tasks/my-project.json --yes
```

**Worktree creation fails**

Check that:
1. The target directory's parent is writable
2. The branch name doesn't have characters that can't be sanitized
3. No other process is holding locks on the git repository

## Task Completion Detection

The loop engine automatically detects and records task completion without requiring the agent to manually update state files.

### How It Works

After each iteration (that isn't a crash, empty output, or rate limit), the loop:

1. **Checks git for task completion**: Looks at the most recent commit message for the claimed task ID
2. **Marks DB as done**: If task ID found in commit, calls `task-mgr complete` to update SQLite
3. **Updates PRD JSON**: Sets `passes: true` for the task in the PRD file

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Claude commits │ --> │  Loop detects    │ --> │  Updates both   │
│  with task ID   │     │  task ID in msg  │     │  DB + PRD JSON  │
└─────────────────┘     └──────────────────┘     └─────────────────┘
```

### Commit Message Format

The loop searches for the task ID anywhere in the commit message (case-insensitive):

```bash
# All of these work:
git commit -m "feat: [SEC-H005] Add feature"
git commit -m "feat: SEC-H005 - implement auth"
git commit -m "Fix bug in sec-h005"
```

### What Claude Needs to Do

With automatic completion detection, Claude only needs to:

1. **Do the work** - implement the task
2. **Commit with task ID** - include task ID in commit message

Claude does **NOT** need to:
- Edit the PRD JSON file
- Run `task-mgr complete`
- Output special completion tags for individual tasks

### Design Rationale

| Decision | Rationale |
|----------|-----------|
| **Git commits as source of truth** | Commits are explicit, intentional actions that prove work was done |
| **Loop handles all bookkeeping** | Reduces agent complexity; agent focuses on coding |
| **Automatic PRD sync** | Keeps JSON file in sync without agent needing to parse/edit it |
| **Case-insensitive matching** | Tolerates variation in commit message formatting |

## Future: Loop-Driven Task Presentation

> **Note**: This section describes planned functionality, not current behavior.

Currently, the prompt template instructs Claude to read the PRD and select tasks. A future improvement moves this responsibility to the loop engine:

### Planned Changes

1. **Loop selects the task** using the scoring algorithm (dependencies, file locality, synergies)
2. **Loop builds task context** including:
   - Task description and acceptance criteria
   - Related tasks (dependencies, synergies)
   - Relevant learnings from previous iterations
   - File context from `touchesFiles`
3. **Loop presents this to Claude** in the prompt, not as files to read

### Benefits

| Benefit | Description |
|---------|-------------|
| **Reduced token usage** | Claude doesn't read entire PRD, only the relevant task |
| **Better task selection** | Loop's algorithm is deterministic and considers more factors |
| **Consistent context** | Loop controls what context is shown, ensuring completeness |
| **Simpler prompts** | Claude receives "here's your task" instead of "read files and decide" |

### Target Workflow

```
┌─────────────────────────────────────────────────────────────────┐
│  Loop Engine                                                     │
│  1. Select next task (scoring algorithm)                         │
│  2. Gather context (deps, learnings, files)                      │
│  3. Build focused prompt with task + context                     │
└─────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│  Claude receives:                                                │
│  - Single task to implement (already selected)                   │
│  - Relevant context (curated by loop)                            │
│  - Clear success criteria                                        │
└─────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│  Claude:                                                         │
│  1. Implements the task                                          │
│  2. Commits with task ID                                         │
│  (Loop handles the rest)                                         │
└─────────────────────────────────────────────────────────────────┘
```
