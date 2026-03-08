# task-mgr

A standalone CLI tool for managing AI agent loop tasks with SQLite as working state. Built in Rust for deterministic state management, institutional memory, and intelligent task selection.

## Why task-mgr?

AI agent loops (like Claude Code running in a loop) face three fundamental problems:

1. **Amnesia**: Each iteration starts fresh with no memory of what was tried before, leading to repeated failures and rediscovered patterns.
2. **Naive ordering**: Without understanding file locality, dependencies, and synergies, agents waste time context-switching between unrelated tasks.
3. **Fragile state**: Bash-based solutions rely on `jq`/`grep`/`sed` for JSON parsing -- untestable, untyped, and prone to silent failures.

task-mgr solves these with:

- **Institutional memory** via a learnings system that captures failure patterns, successful approaches, and workarounds -- then resurfaces them when working on similar tasks.
- **Smart task selection** that scores candidates by priority, file locality, dependency satisfaction, synergy hints, and batch grouping.
- **Crash-safe persistence** using SQLite with WAL mode, file locking, and export-after-every-iteration for recovery.

## Installation

```bash
# From source
cargo install --path .

# Or build directly
cargo build --release
# Binary at target/release/task-mgr
```

## Quick Start

### 1. Create a PRD JSON file

task-mgr works with JSON-formatted Product Requirement Documents (PRDs). Each PRD contains tasks with priorities, dependencies, file references, and acceptance criteria:

```json
{
  "meta": {
    "title": "My Project",
    "version": "1.0.0"
  },
  "stories": [
    {
      "id": "US-001",
      "title": "Implement user login",
      "priority": 10,
      "description": "Add login endpoint with JWT auth",
      "acceptanceCriteria": [
        "POST /login returns JWT token",
        "Invalid credentials return 401"
      ],
      "touchesFiles": ["src/auth.rs", "src/routes.rs"],
      "dependsOn": [],
      "synergyWith": ["US-002"],
      "notes": "Use argon2 for password hashing"
    }
  ]
}
```

### 2. Initialize the database

```bash
task-mgr init --from-json tasks/my-project.json
```

### 3. Start a run and work through tasks

```bash
# Begin a session
RUN_ID=$(task-mgr run begin --format json | jq -r '.run_id')

# Get the next recommended task (claims it for this run)
task-mgr next --claim --run-id "$RUN_ID"

# After completing the task
task-mgr complete US-001 --run-id "$RUN_ID" --commit abc123

# If a task is blocked
task-mgr fail US-002 --error "Missing dependency" --run-id "$RUN_ID"

# Export state for crash recovery (do this after every iteration)
task-mgr export --to-json tasks/my-project.json

# End the session
task-mgr run end --run-id "$RUN_ID" --status completed
```

### 4. Record and recall learnings

```bash
# Record a failure pattern
task-mgr learn --outcome failure \
  --title "rusqlite bundled feature required" \
  --content "Compilation fails without bundled SQLite on cross-platform builds" \
  --solution "Add rusqlite with bundled feature to Cargo.toml" \
  --files "Cargo.toml" \
  --tags rust,sqlite,dependencies \
  --confidence high

# Recall learnings for a task (also happens automatically with `next`)
task-mgr recall --for-task US-015
```

### 5. Run the autonomous loop

```bash
# Run task-mgr's built-in agent loop
task-mgr loop tasks/my-project.json --yes --hours 4

# Or use with an external shell script (see docs/INTEGRATION.md)
```

## Command Reference

### Task Lifecycle

| Command | Description |
|---------|-------------|
| `init` | Import tasks from JSON PRD file(s) |
| `list` | List tasks with status/file/type filtering |
| `show` | Show detailed task information |
| `next` | Get next recommended task (with smart selection) |
| `complete` | Mark task(s) as done |
| `fail` | Mark task as blocked |
| `skip` | Defer task for later |
| `irrelevant` | Mark task as no longer needed |
| `unblock` | Return blocked task to todo |
| `unskip` | Return skipped task to todo |
| `reset` | Reset task(s) to todo for re-running |

### Run Management

| Command | Description |
|---------|-------------|
| `run begin` | Start a new agent loop session |
| `run update` | Update run with progress metadata |
| `run end` | End a session (completed or aborted) |
| `stats` | Show progress summary |
| `history` | Show run history |

### Learnings & Memory

| Command | Description |
|---------|-------------|
| `learn` | Record a learning (failure, success, workaround, pattern) |
| `recall` | Find relevant learnings by task, query, tags, or outcome |
| `learnings` | List all learnings |
| `edit-learning` | Modify an existing learning |
| `delete-learning` | Remove a learning |
| `apply-learning` | Mark a learning as applied (feeds the ranking algorithm) |
| `import-learnings` | Import learnings from JSON |

### Loop & Automation

| Command | Description |
|---------|-------------|
| `loop` | Run autonomous agent loop with a PRD file (uses git worktrees by default) |
| `loop --no-worktree` | Run loop using branch checkout instead of worktrees |
| `status` | Show loop status dashboard |
| `batch` | Run multiple PRDs in sequence |
| `archive` | Archive completed PRDs and extract learnings |

### Maintenance & Tooling

| Command | Description |
|---------|-------------|
| `export` | Export database state to JSON |
| `doctor` | Health check and repair (stale tasks, decay, git reconciliation) |
| `migrate` | Database schema migrations (status, up, down, all) |
| `review` | Review blocked/skipped tasks |
| `completions` | Generate shell completions (bash, zsh, fish, powershell) |
| `man-pages` | Generate man pages |

### Global Options

```
--dir <PATH>       Database directory (default: .task-mgr/)
--format <FORMAT>  Output format: text or json (default: text)
-v, --verbose      Enable verbose output (score breakdowns, decay info)
```

## Task Selection Algorithm

The `next` command uses weighted multi-factor scoring to pick the optimal task:

```
score = (1000 - priority)
      + (10 * file_overlap_count)
      + (3 * synergy_count)
      + (-5 * conflict_count)
```

| Factor | Weight | Rationale |
|--------|--------|-----------|
| Priority | 1000 - p | Dominant factor; priority 1 scores 999, priority 50 scores 950 |
| File overlap | +10/file | 3 shared files shifts by ~30 points (~3 priority levels) |
| Synergy | +3/link | Mild preference, acts as tie-breaker |
| Conflict | -5/link | Discourages but doesn't block |

Tasks are only eligible if their status is `todo` and all `dependsOn` tasks are either `done` or `irrelevant`. Blocked/skipped tasks automatically decay back to `todo` after 32 iterations (configurable via `--decay-threshold`).

The loop engine extends this with adaptive weight calibration (point-biserial correlation) that tunes weights based on historical success/failure outcomes.

## Learnings System

The learnings system provides institutional memory across agent iterations. See [docs/LEARNINGS.md](docs/LEARNINGS.md) for the full guide.

### How it works

1. **Record**: When something notable happens (failure, pattern discovery, workaround), record it with context -- file patterns, error patterns, tags, and confidence level.
2. **Recall**: When `next` selects a task, it automatically retrieves relevant learnings based on file pattern matching (+10 pts), task type matching (+5 pts), and error pattern matching (+2 pts).
3. **Rank**: Learnings are ranked using a UCB (Upper Confidence Bound) bandit algorithm that balances exploitation (proven learnings) with exploration (new ones).
4. **Feedback**: When a task succeeds after a learning was shown, the loop engine marks it as applied, improving future ranking.

### Learning outcomes

| Outcome | When to record |
|---------|---------------|
| `failure` | Non-obvious errors with root causes and solutions |
| `success` | Approaches that worked well and should be replicated |
| `workaround` | Non-ideal fixes for known limitations |
| `pattern` | Reusable code patterns and conventions |

## Loop Engine

The built-in loop engine (`task-mgr loop`) replaces the external `claude-loop.sh` script with a native Rust implementation. Key capabilities:

- **Git worktrees** (default): Uses `git worktree` to isolate work for each PRD branch, avoiding "would be overwritten" errors when you have uncommitted changes
- **Enriched prompts**: Scans source files for actual function/struct signatures, injects dependency context and synergy information
- **Output detection**: Analyzes Claude output for completion markers, blocked signals, rate limits, crashes, and stale iterations
- **Crash recovery**: Exponential backoff (30s base, capped at exponent 20), max 3 consecutive crashes before abort
- **Steering**: Runtime control via `.task-mgr/.pause`, `.task-mgr/.stop`, and `.task-mgr/steering.md`
- **Closed-loop feedback**: Automatically marks learnings as applied when tasks succeed, improving the UCB ranking
- **Adaptive weights**: Calibrates selection weights based on historical success correlations
- **Token tracking**: Monitors API usage for budgeting
- **Batch mode**: Run multiple PRDs sequentially with `task-mgr batch`

```bash
# Run with time budget and auto-confirmation
task-mgr loop tasks/my-project.json --yes --hours 8

# Show current status
task-mgr status tasks/my-project.json --verbose

# Run multiple PRDs
task-mgr batch 'tasks/*.json' --yes

# Disable worktrees (use branch checkout instead)
task-mgr loop tasks/my-project.json --yes --no-worktree
```

### Git Worktrees

By default, `task-mgr loop` creates a git worktree for the PRD's branch instead of checking out the branch directly. This provides several benefits:

- **No conflicts with dirty working tree**: You can have uncommitted changes in your main repo without blocking the loop
- **Parallel work**: Multiple loops can run on different branches simultaneously
- **Isolation**: Changes in the worktree don't affect your main checkout

Worktrees are created at `{repo-parent}/{repo-name}-worktrees/{branch-name}/`. For example, if your repo is at `/home/user/myproject` and the PRD branch is `feature/auth`, the worktree will be at `/home/user/myproject-worktrees/feature-auth/`.

Use `--no-worktree` to revert to the old behavior of checking out branches directly (useful for CI/CD or when you prefer the simpler model).

## Iterative Build Workflow

task-mgr is designed around a 5-phase iterative workflow that takes you from idea to working code:

```
  Full workflow (large tasks):
  ┌──────────┐    ┌──────────┐    ┌──────────┐    ┌──────────┐    ┌──────────┐
  │  1. Plan │───▶│  2. PRD  │───▶│ 3. Tasks │───▶│ 4. Build │───▶│ 5. Learn │
  │          │    │  /prd    │    │  /tasks  │    │          │    │          │
  │  Explore │    │  Define  │    │  Break   │    │  Execute │    │  Capture │
  │  & Design│    │  Quality │    │  Down &  │    │  Loop or │    │  Lessons │
  └──────────┘    └──────────┘    │  Sequence│    │  Batch   │    └──────────┘
       │                          └──────────┘    └──────────┘
       │          Shortcut (small-medium tasks):        ▲
       └─────────▶ /plan-tasks ─────────────────────────┘
```

### Phase 1: Plan

Use Claude Code's **Plan Mode** (`Shift+Tab` to toggle) to explore the codebase, understand constraints, and design your approach before writing any code.

### Phase 2: PRD

The `/prd` skill generates a structured Product Requirements Document from rough requirements:

```bash
/prd "Add batch execution mode with configurable concurrency"
```

This produces a `tasks/prd-{feature}.md` file with quality dimensions, edge cases, multiple approaches with tradeoffs, public contracts, and risk analysis. The structured format ensures the implementing agent has precise targets instead of vague instructions.

### Phase 3: Tasks

The `/tasks` skill converts a PRD into a JSON task list and prompt file for loop execution:

```bash
/tasks tasks/prd-batch-mode.md
```

This produces:
- `tasks/batch-mode.json` — Ordered task list with dependencies, quality dimensions, and edge cases per task
- `tasks/batch-mode-prompt.md` — System prompt for the autonomous agent with codebase context

### Shortcut: `/plan-tasks` (Skip PRD for Small-Medium Work)

For tasks that can be accomplished in 8-10 tasks (1-7 files, clear requirements), `/plan-tasks` combines planning and task generation into one step — skipping the PRD:

```bash
/plan-tasks "Update archive command to iterate PRDs by prefix and archive completed ones independently"
```

This explores the codebase, asks clarifying questions to expose hidden assumptions, then directly produces the task JSON and prompt file. Each task includes both positive requirements (what to do) and negative requirements (what not to do), with known-bad discriminators to catch plausible-but-wrong implementations.

Use `/plan-tasks` when:
- Requirements are clear and scope is bounded
- A reference implementation or existing pattern exists to follow
- The change is a refactor, enhancement, or bug fix to existing code

Use `/prd` + `/tasks` when:
- The task is large (7+ files, architectural decisions, multiple phases)
- Scope is uncertain and needs crystallizing
- Multiple stakeholders need to review requirements

### Phase 4: Build

Run the iterative autonomous loop:

```bash
# Interactive (confirms before starting)
task-mgr loop tasks/batch-mode.json

# Non-interactive (CI/CD or background execution)
task-mgr loop tasks/batch-mode.json --yes
```

The loop claims tasks in dependency order, runs Claude Code for each, validates results, and moves on. See [Loop Execution](#loop-execution) for details.

### Phase 5: Learn

Learnings accumulate automatically during loop execution. The agent captures what worked, what didn't, and patterns discovered. These feed into future iterations via UCB bandit ranking.

Use the learnings feedback skills during interactive sessions:

| Skill | Purpose |
|-------|---------|
| `/tm-apply` | Confirm a learning was useful (boosts UCB ranking score) |
| `/tm-learn` | Record a learning (auto-detects outcome type, task context) |
| `/tm-recall` | Query learnings by task, text, tags, or auto-detect from context |
| `/tm-invalidate` | Invalidate a wrong learning (two-step: downgrade then retire) |
| `/tm-status` | Show project status dashboard, task progress, active runs |
| `/tm-next` | Get next recommended task with scoring breakdown |

When a learning blocks progress (e.g., "tests can't run because service X is down"), the agent is instructed to **test the claim first** — run one test to verify — rather than blindly skipping work based on stale information.

### Setting Up the Skills

All skills are included in this repo at `.claude/commands/`. There are two categories:

**Workflow skills** (`/prd`, `/tasks`, `/plan-tasks`) — used during planning phases:

| Skill | Purpose |
|-------|---------|
| `/prd` | Generate structured PRD from rough requirements |
| `/tasks` | Convert PRD to JSON task list + prompt file |
| `/plan-tasks` | Combined planning + task generation (skip PRD for small work) |

**Task-mgr skills** (`/tm-apply`, `/tm-learn`, `/tm-recall`, `/tm-invalidate`, `/tm-status`, `/tm-next`) — used during build/learn phases.

**Option A: Use the repo copies directly** — Claude Code automatically loads skills from `.claude/commands/` in the project root. Just clone the repo and they're available.

**Option B: Copy to global commands** — To make skills available across all projects:

```bash
# Workflow skills
cp .claude/commands/prd.md ~/.claude/commands/
cp .claude/commands/tasks.md ~/.claude/commands/
cp .claude/commands/plan-tasks.md ~/.claude/commands/

# Task-mgr skills (recommended for all projects using task-mgr)
cp .claude/commands/tm-apply.md ~/.claude/commands/
cp .claude/commands/tm-learn.md ~/.claude/commands/
cp .claude/commands/tm-recall.md ~/.claude/commands/
cp .claude/commands/tm-invalidate.md ~/.claude/commands/
cp .claude/commands/tm-status.md ~/.claude/commands/
cp .claude/commands/tm-next.md ~/.claude/commands/
```

The loop engine warns at startup if task-mgr skills are missing from `~/.claude/commands/` and shows the exact copy commands needed.

## Shell Integration

For integrating task-mgr into your own agent loops or CI pipelines, see [docs/INTEGRATION.md](docs/INTEGRATION.md). The core pattern is:

```bash
# Initialize
task-mgr init --from-json prd.json
task-mgr doctor --auto-fix

# Run loop
RUN_ID=$(task-mgr --format json run begin | jq -r '.run_id')
while true; do
    TASK=$(task-mgr --format json next --claim --run-id "$RUN_ID" --after-files "$LAST_FILES")
    TASK_ID=$(echo "$TASK" | jq -r '.task.id // empty')
    [ -z "$TASK_ID" ] && break

    # Execute agent with task context...

    task-mgr complete "$TASK_ID" --run-id "$RUN_ID"
    task-mgr export --to-json prd.json  # crash recovery
done
task-mgr run end --run-id "$RUN_ID" --status completed
```

## Shell Completions

```bash
# Bash
task-mgr completions bash > ~/.local/share/bash-completion/completions/task-mgr

# Zsh
task-mgr completions zsh > ~/.zsh/completions/_task-mgr

# Fish
task-mgr completions fish > ~/.config/fish/completions/task-mgr.fish
```

## Project Structure

```
task-mgr/
├── src/
│   ├── main.rs              # Entry point, CLI dispatch
│   ├── cli/                 # CLI definition (clap derive)
│   ├── commands/            # Command implementations (init, next, complete, ...)
│   ├── db/                  # SQLite connection, schema, migrations, file locking
│   ├── models/              # Data structures (Task, Learning, Run)
│   ├── learnings/           # CRUD, recall, UCB bandit ranking
│   ├── loop_engine/         # Autonomous loop (engine, prompt, context, detection, ...)
│   ├── handlers.rs          # Output formatting (text/JSON)
│   └── error.rs             # Error types with actionable messages
├── tests/                   # Integration tests (CLI, concurrent, import/export, e2e)
├── docs/                    # INTEGRATION.md, LEARNINGS.md, design documents
└── scripts/                 # Shell scripts and prompt templates
```

## Development

```bash
# Quality gates (all must pass)
cargo check                    # Type checking
cargo clippy -- -D warnings    # Lint (warnings are errors)
cargo test                     # Unit + integration tests
cargo fmt -- --check           # Format check
```

## Further Documentation

- [docs/INTEGRATION.md](docs/INTEGRATION.md) -- Shell integration guide with full workflow examples
- [docs/LEARNINGS.md](docs/LEARNINGS.md) -- Learnings system guide with best practices and tag taxonomy
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) -- Architectural design document with design rationale
- [docs/designs/task-selection.md](docs/designs/task-selection.md) -- Task selection algorithm design
- [docs/designs/next-command.md](docs/designs/next-command.md) -- Next command integration design
- [docs/designs/learnings-recall.md](docs/designs/learnings-recall.md) -- Learnings recall algorithm design

## License

MIT License
