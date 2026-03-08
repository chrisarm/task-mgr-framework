# /tm-status - Show Task-Mgr Status

Display project status, task progress, and active run information.

## Usage

```
/tm-status                    # Overview of all projects
/tm-status tasks/my-prd.json  # Status for specific PRD
```

## Instructions

Show the user a comprehensive status view by running relevant task-mgr commands.

### Step 1: Project Overview

```bash
# Show status dashboard
task-mgr status

# Or for a specific PRD
task-mgr status <prd-file> --verbose
```

### Step 2: Task Breakdown (if user wants detail)

```bash
# List tasks by status
task-mgr list --status todo
task-mgr list --status in_progress
task-mgr list --status done

# Show stats summary
task-mgr stats
```

### Step 3: Active Runs

```bash
# Show run history
task-mgr history --limit 5
```

### Step 4: Learning Stats

```bash
# List recent learnings
task-mgr learnings --recent 5
```

### Presentation

Summarize in a concise table:
- Total tasks / completed / remaining / blocked
- Active run info (if any)
- Recent learnings count
- Next recommended task
