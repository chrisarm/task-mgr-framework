# /tm-learn - Record a Learning

Record a learning to task-mgr's institutional memory so future iterations benefit from this discovery.

## Usage

```
/tm-learn "title of the learning"
/tm-learn                           # Interactive mode
```

## Instructions

Help the user record a learning via `task-mgr learn`. Determine the right parameters by analyzing context.

### Step 1: Determine Outcome Type

Based on the conversation context, classify the learning:

| Outcome | When to Use |
|---------|-------------|
| `failure` | Something broke, errored, or produced wrong results |
| `success` | A deliberate approach that worked well |
| `workaround` | A non-obvious fix for a known limitation |
| `pattern` | A reusable technique or convention discovered |

### Step 2: Extract Details

From the conversation, identify:

- **Title**: 1-line summary (what was learned)
- **Content**: Detailed explanation (when/why/how)
- **Root cause** (failure/workaround): Why this happened
- **Solution** (success/workaround/pattern): What to do about it
- **Files**: Glob patterns for relevant files
- **Tags**: Categorization tags (language, domain, concept)
- **Confidence**: high (verified), medium (likely correct), low (tentative)
- **Task ID**: Current task if known (check git branch for task prefix)
- **Run ID**: Current run if in a loop (check `task-mgr stats`)

### Step 3: Determine Task Context

```bash
# Check if we're in a loop run
task-mgr stats 2>/dev/null | head -5

# Check current branch for task context
git branch --show-current
```

### Step 4: Record the Learning

Build and execute the `task-mgr learn` command:

```bash
task-mgr learn <outcome> \
  --title "..." \
  --content "..." \
  [--root-cause "..."] \
  [--solution "..."] \
  [--task-id "..."] \
  [--run-id "..."] \
  [--files "glob1,glob2"] \
  [--tags "tag1,tag2"] \
  --confidence <high|medium|low>
```

### Step 5: Confirm

Show the user the recorded learning ID and a summary.

## Tag Conventions

| Category | Tags |
|----------|------|
| Language | `rust`, `sql`, `bash`, `json`, `elixir`, `python` |
| Domain | `cli`, `database`, `async`, `testing`, `deployment` |
| Concept | `error-handling`, `serialization`, `concurrency`, `migrations` |
| Issue | `compiler-error`, `runtime-error`, `performance`, `race-condition` |

## Examples

```
/tm-learn "SQLite COALESCE needed for nullable aggregates"
/tm-learn "rusqlite bundled feature required for cross-compilation"
/tm-learn
```
