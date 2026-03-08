# /tm-recall - Query Relevant Learnings

Search task-mgr's institutional memory for learnings relevant to your current work.

## Usage

```
/tm-recall "search query"
/tm-recall --for-task FEAT-001
/tm-recall                        # Auto-detect from context
```

## Instructions

Help the user find relevant learnings from the task-mgr database.

### Step 1: Determine Search Strategy

Choose the best approach based on context:

1. **Task-based** (if working on a specific task): `--for-task` matches by file patterns and task type
2. **Query-based** (if investigating a topic): `--query` searches title and content
3. **Tag-based** (if browsing a category): `--tags` filters by tags
4. **Combined**: Mix filters for precise results

### Step 2: Execute Search

```bash
# By current task (matches files and task type)
task-mgr recall --for-task <TASK-ID> --limit 10

# By text query
task-mgr recall --query "search terms" --limit 10

# By tags
task-mgr recall --tags "rust,error-handling"

# By outcome type
task-mgr recall --outcome failure --limit 5

# Combined
task-mgr recall --query "sqlite" --tags "database" --limit 10

# JSON for detailed output
task-mgr --format json recall --query "..." --limit 5
```

### Step 3: Present Results

Show the learnings with their IDs. Remind the user:

- **If a learning is helpful**: Run `task-mgr apply-learning <id>` to boost its ranking
- **If a learning is wrong**: Run `task-mgr invalidate-learning <id>` to degrade it

### Auto-Detection

If no arguments given, try to infer context:

```bash
# Check current branch for task prefix
git branch --show-current

# Check recently modified files
git diff --name-only HEAD~1 2>/dev/null
```

Use branch name to extract task prefix, modified files to search by file patterns.
