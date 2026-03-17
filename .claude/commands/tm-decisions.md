# /tm-decisions - Manage Key Architectural Decisions

Interactively review and resolve pending key decisions captured during loop runs.

## Usage

```
/tm-decisions              # Review all pending decisions
/tm-decisions <id>         # Jump to a specific decision by ID
```

## Instructions

### Step 1: Fetch Pending Decisions

Run:

```bash
task-mgr --format json decisions list
```

Parse the JSON output. The `decisions` array contains pending and deferred decisions. Each entry has:
- `id` — numeric ID used in CLI commands
- `title` — short description of the decision
- `status` — `pending` or `deferred`
- `options` — array of `{ label, description }` objects

### Step 2: Handle No Decisions

If `decisions` is empty, tell the user:

> No pending key decisions. Decisions are recorded when the loop agent tags an important architectural fork with `<key-decision>`.

Stop here.

### Step 3: Present Each Decision

For each decision, display it clearly before asking what to do:

```
Decision #<id> [<STATUS>]
  <title>

  Options:
    A) <options[0].label> — <options[0].description>
    B) <options[1].label> — <options[1].description>
    ...

  What would you like to do?
    • Type A/B/... to resolve (select an option)
    • Type "decline" to mark as not needed
    • Type "skip" to leave pending for now
    • Type "revert" to reopen a resolved decision (use task-mgr decisions list --all first)
```

If the user was invoked with a specific ID (`/tm-decisions <id>`), jump directly to that decision and skip others.

### Step 4: Execute the User's Choice

Based on the user's response:

**Resolve** (user types a letter like `A`, `B`, or a label substring):
```bash
task-mgr decisions resolve <id> <option>
```
Example: `task-mgr decisions resolve 3 A` or `task-mgr decisions resolve 3 SQLite`

**Decline** (user types "decline", optionally with a reason):
```bash
task-mgr decisions decline <id> --reason '<reason>'
```
If no reason given: `task-mgr decisions decline <id>`

**Skip** (user types "skip"):
Move to the next decision without running any command.

**Revert** (user types "revert"):
```bash
task-mgr decisions revert <id>
```
Note: This only works on already-resolved decisions. Use `task-mgr decisions list --all` to see resolved ones.

### Step 5: Continue

After each decision, proceed to the next one automatically. When all decisions have been addressed (resolved, declined, or skipped), show a summary:

```
Done! Reviewed <N> decision(s):
  - Resolved: <count>
  - Declined: <count>
  - Skipped: <count>
```

## Reference

```bash
# List pending decisions (JSON)
task-mgr --format json decisions list

# List all decisions including resolved
task-mgr decisions list --all

# Resolve by option letter or label
task-mgr decisions resolve <id> A
task-mgr decisions resolve <id> "Option Label"

# Decline (mark as not needed)
task-mgr decisions decline <id>
task-mgr decisions decline <id> --reason 'Not relevant for MVP'

# Reopen a resolved decision
task-mgr decisions revert <id>
```
