# /tm-next - Get Next Recommended Task

Show the next recommended task based on task-mgr's scoring algorithm (priority, file locality, dependencies, synergy).

## Usage

```
/tm-next                      # Get recommendation
/tm-next --claim              # Claim the task for current run
/tm-next --verbose            # Show scoring breakdown
```

## Instructions

### Step 1: Get Recommendation

```bash
# Read-only recommendation with scoring details
task-mgr --verbose next

# With file locality (prefer tasks touching recently modified files)
task-mgr --verbose next --after-files "$(git diff --name-only HEAD~1 2>/dev/null | tr '\n' ',')"
```

### Step 2: Show Task Details

If a task is recommended, show its full details:

```bash
task-mgr show <TASK-ID>
```

### Step 3: Show Relevant Learnings

Query learnings relevant to the recommended task:

```bash
task-mgr recall --for-task <TASK-ID> --limit 5
```

### Presentation

Show the user:
1. Recommended task ID, title, and priority
2. Acceptance criteria
3. Files that will be touched
4. Any relevant learnings from institutional memory
5. Dependencies (completed and pending)

If the user wants to claim it:
```bash
task-mgr next --claim --run-id <RUN-ID>
```
