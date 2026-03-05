# Quick Start Guide

Welcome to **task-mgr** — a CLI tool that helps AI coding agents (like Claude Code) work through tasks intelligently instead of wandering aimlessly. It tracks what's done, what failed, and what to try next.

## Prerequisites

- **Rust toolchain**: Install via [rustup](https://rustup.rs/) if you don't have it
- **Git**: task-mgr uses git for branching and worktrees
- **Claude Code** (optional): Required only if you want to run the autonomous loop

## Install

```bash
cd task-mgr
cargo build --release

# Option A: Copy the binary somewhere on your PATH
cp target/release/task-mgr ~/.local/bin/

# Option B: Or install directly
cargo install --path .
```

Verify it works:

```bash
task-mgr --help
```

## Core Concepts

Before diving in, here's the mental model:

| Concept | What it is |
|---------|-----------|
| **PRD** | A JSON file listing tasks with priorities, dependencies, and file references |
| **Task** | A single unit of work (e.g., "Add login endpoint") with a lifecycle: `todo` -> `in_progress` -> `done` |
| **Run** | A session where an agent works through tasks. Tracks which tasks were attempted |
| **Learning** | A note about what worked or failed — remembered across runs so mistakes aren't repeated |
| **Loop** | The autonomous mode where task-mgr drives Claude Code to complete tasks one by one |

## When to Use task-mgr vs. Plan Mode

Claude Code already has a built-in **plan mode** (`Shift+Tab` to toggle) that lets you think through a problem, explore the codebase, and then implement — all in one interactive session. For many tasks, that's all you need.

task-mgr adds a layer on top: persistent state, dependency ordering, crash recovery, learnings across sessions, and unattended execution. The question is whether that overhead is worth it for *your* specific effort.

### The Spectrum

Think of it as a spectrum from lightweight to heavyweight:

```
  Just ask Claude     Plan Mode          task-mgr             task-mgr batch
  ─────────────────────────────────────────────────────────────────────────►
  "Fix this bug"      "Design and        "Build this          "Ship these 3
                       build this         feature while        features while
                       feature with me"   I'm at lunch"       I sleep"
```

**Just ask Claude Code** — Single question, single fix, one file. No planning needed. "Why is this test failing?" or "Add a --verbose flag to this command."

**Plan mode** — You're figuring out *what* to build or *how* to approach it. You want to explore the codebase, weigh tradeoffs, and iterate on a design before writing code. Plan mode is interactive and conversational — you're in the driver's seat. Great for: architecture decisions, debugging complex issues, prototyping an approach, or implementing a feature where you want to review each step.

**task-mgr** — You *already know* what to build and can describe it as a concrete list of tasks with acceptance criteria. The work has dependencies (task B needs task A done first), touches many files, and would benefit from running unattended. task-mgr handles ordering, crash recovery, and remembers what failed so the agent doesn't repeat mistakes. Great for: multi-file features, large refactors, migration efforts, or anything you want to run overnight.

**task-mgr batch** — Same as above, but across multiple independent features. Run three PRDs in sequence while you sleep.

### When task-mgr Earns Its Keep

**Multi-step features with ordering constraints.** If task B can't start until task A is done (e.g., "define the schema" before "write the query layer" before "add the API route"), task-mgr's dependency graph prevents wasted work. Example: adding a full authentication system — schema, models, routes, middleware, tests — where each layer builds on the previous one.

**Large refactors that touch many files.** Renaming a core abstraction, migrating from one framework to another, splitting a monolith into modules. These have dozens of small tasks with implicit ordering. task-mgr keeps the agent on track across crashes and session boundaries.

**Efforts where failures are likely and repeatable.** Complex integrations, unfamiliar codebases, or work that frequently hits build errors and edge cases. The learnings system captures what went wrong so the agent doesn't make the same mistake in iteration 5 that it made in iteration 2.

**Unattended execution.** The loop engine with time budgets, crash recovery, and steering files is built for "start it and walk away." If you're running overnight or over a weekend, the orchestration earns its keep.

**Cross-cutting changes with file overlap.** When tasks touch the same files, task-mgr's file-locality scoring groups them together, reducing context-switching. Example: updating 15 API endpoints to use a new error format — worked through in file-clustered order, not random order.

### When to Skip task-mgr

**You're still exploring.** "Why is this slow?" or "What's the best approach here?" requires open-ended thinking. Use plan mode to investigate, then create a PRD once you have a plan.

**Single-task changes.** One bug, one endpoint, one config update. Just ask Claude Code.

**Tightly interactive work.** UI polish, API design iteration, or anything where you want to review and adjust after every change. The autonomous loop will frustrate you — work interactively instead.

**Small efforts (< 5 tasks).** The setup cost of writing a PRD and running the loop isn't worth it. Just work through them in one session.

### The Typical Flow

Most efforts actually use *both*. A common pattern:

1. **Plan mode** — Explore the codebase, understand the problem, design the approach
2. **`/prd`** — Turn your plan into a structured PRD
3. **`/tasks`** — Convert the PRD into an ordered task list
4. **`task-mgr loop`** — Let the agent execute unattended

Plan mode is where you think. task-mgr is where you execute.

### Quick Decision Guide

| Question | If yes... |
|----------|-----------|
| Am I still figuring out *what* to build? | Plan mode |
| Do I have a clear list of tasks with acceptance criteria? | task-mgr |
| Are there more than 5 tasks with dependencies? | task-mgr |
| Will this take more than one session to complete? | task-mgr (crash recovery + learnings) |
| Do I need to review every change before the next one? | Plan mode or interactive |
| Can I describe "done" for each task? | task-mgr |
| Do I want to run this while I'm away? | task-mgr |

## The 5-Minute Walkthrough

### Step 1: Create a PRD file

A PRD is just a JSON file describing your tasks. Create `tasks/my-feature.json`:

```json
{
  "meta": {
    "title": "My Feature",
    "version": "1.0.0"
  },
  "stories": [
    {
      "id": "MF-001",
      "title": "Add configuration file parser",
      "priority": 1,
      "description": "Parse TOML config files and validate required fields",
      "acceptanceCriteria": [
        "Reads config.toml from project root",
        "Returns error if required fields are missing"
      ],
      "touchesFiles": ["src/config.rs"],
      "dependsOn": [],
      "synergyWith": ["MF-002"],
      "notes": "Use the toml crate"
    },
    {
      "id": "MF-002",
      "title": "Wire config into main",
      "priority": 2,
      "description": "Load config at startup and pass to subsystems",
      "acceptanceCriteria": [
        "Config loads before server starts",
        "Missing config exits with helpful error message"
      ],
      "touchesFiles": ["src/main.rs", "src/config.rs"],
      "dependsOn": ["MF-001"],
      "synergyWith": [],
      "notes": ""
    }
  ]
}
```

Key fields:
- **priority**: Lower number = higher priority (1 is most important)
- **dependsOn**: Task IDs that must be completed first
- **touchesFiles**: Files this task will modify (used for smart ordering)
- **synergyWith**: Related tasks that benefit from being worked on nearby

### Step 2: Import tasks into the database

```bash
task-mgr init --from-json tasks/my-feature.json
```

This creates a SQLite database at `.task-mgr/tasks.db` and loads your tasks.

### Step 3: See what's there

```bash
# List all tasks
task-mgr list

# Show details for one task
task-mgr show MF-001

# See overall progress
task-mgr stats
```

### Step 4: Work through tasks manually

If you want to work through tasks yourself (without the autonomous loop):

```bash
# Start a run session
RUN_ID=$(task-mgr run begin --format json | jq -r '.run_id')

# Ask task-mgr what to work on next (it picks the best candidate)
task-mgr next --claim --run-id "$RUN_ID"

# After you finish the task
task-mgr complete MF-001 --run-id "$RUN_ID"

# If you get stuck
task-mgr fail MF-001 --error "Can't find the toml crate docs" --run-id "$RUN_ID"

# When you're done for the day
task-mgr run end --run-id "$RUN_ID" --status completed
```

### Step 5: Run the autonomous loop (the main event)

This is where task-mgr really shines — it drives Claude Code to complete tasks automatically:

```bash
# Run with a 2-hour time budget, auto-confirm prompts
task-mgr loop tasks/my-feature.json --yes --hours 2
```

What happens behind the scenes:
1. task-mgr picks the highest-priority task with satisfied dependencies
2. It builds a rich prompt with task details, file context, and relevant learnings
3. Claude Code executes the task
4. task-mgr detects whether it succeeded or failed
5. It records learnings and moves to the next task
6. Repeat until all tasks are done or time runs out

### Step 6: Check progress

While the loop is running (or after):

```bash
# Dashboard view
task-mgr status tasks/my-feature.json

# Detailed stats
task-mgr stats --verbose

# See what happened
task-mgr history
```

## Using the Claude Code Skills (PRD & Task Generation)

You don't have to write PRD JSON by hand. If you're using Claude Code, two built-in skills automate this:

### Generate a PRD from a rough idea

Inside Claude Code, type:

```
/prd "Add batch execution mode with configurable concurrency"
```

This produces a structured PRD markdown file at `tasks/prd-batch-mode.md` with requirements, edge cases, approaches, and risk analysis.

### Convert a PRD into a task list

```
/tasks tasks/prd-batch-mode.md
```

This produces:
- `tasks/batch-mode.json` — The task list ready for `task-mgr init`
- `tasks/batch-mode-prompt.md` — A system prompt for the autonomous loop

Then run the loop:

```bash
task-mgr loop tasks/batch-mode.json --yes
```

## Common Commands Cheat Sheet

```bash
# Setup
task-mgr init --from-json tasks/feature.json   # Import tasks
task-mgr doctor --auto-fix                      # Health check & repair

# Working with tasks
task-mgr list                                   # See all tasks
task-mgr list --status todo                     # Filter by status
task-mgr next                                   # What should I do next?
task-mgr complete TASK-001                      # Mark done
task-mgr fail TASK-001 --error "reason"         # Mark blocked
task-mgr skip TASK-001                          # Defer for later
task-mgr reset TASK-001                         # Back to todo

# Autonomous loop
task-mgr loop tasks/feature.json --yes          # Run the loop
task-mgr status tasks/feature.json              # Check progress
task-mgr batch 'tasks/*.json' --yes             # Run multiple PRDs

# Learnings (institutional memory)
task-mgr learn --outcome failure \
  --title "Missing bundled feature" \
  --content "Build fails without bundled SQLite" \
  --solution "Add rusqlite with bundled feature" \
  --tags rust,sqlite --confidence high
task-mgr recall --for-task TASK-005             # Find relevant learnings
task-mgr learnings                              # List all learnings

# Maintenance
task-mgr export --to-json tasks/feature.json    # Export DB to JSON
task-mgr review                                 # Review blocked/skipped tasks
task-mgr stats                                  # Progress summary
```

## Controlling a Running Loop

You can steer a running loop without stopping it:

| Action | How |
|--------|-----|
| **Pause** | `touch .task-mgr/.pause` (remove the file to resume) |
| **Stop gracefully** | `touch .task-mgr/.stop` |
| **Send instructions** | Write to `.task-mgr/steering.md` — the loop reads it each iteration |

## Troubleshooting

**"database is locked"** — Another task-mgr process is running. Check with `ps aux | grep task-mgr`. Only one process should access the DB at a time.

**Tasks stuck in `in_progress`** — Run `task-mgr doctor --auto-fix` to reset stale tasks.

**Loop keeps crashing** — task-mgr has built-in crash recovery with exponential backoff. After 3 consecutive crashes it aborts. Check the task's error with `task-mgr show TASK-ID` and record a learning so future runs can avoid the same issue.

**Dependencies seem wrong** — Use `task-mgr show TASK-ID` to see which dependencies are blocking. Use `task-mgr irrelevant BLOCKER-ID` if a dependency is no longer needed.

## Next Steps

- Read the full [README](../README.md) for the task selection algorithm and architecture
- See [docs/INTEGRATION.md](INTEGRATION.md) for integrating task-mgr into custom scripts or CI
- See [docs/LEARNINGS.md](LEARNINGS.md) for best practices on recording useful learnings
- See [docs/ARCHITECTURE.md](ARCHITECTURE.md) for how the codebase is structured
