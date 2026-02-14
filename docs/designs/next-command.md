# Next Command Integration Design

## Overview

The `next` command is the core interface for AI agent loops. It integrates three subsystems:

1. **Task Selection** - Smart scoring algorithm (implemented in US-013)
2. **Task Claiming** - Update task status and link to run tracking
3. **Learnings Retrieval** - Find relevant institutional memory for the task

This document describes how these subsystems integrate and defines the complete JSON output schema.

## Integration Architecture

```
┌───────────────────────────────────────────────────────────────────┐
│                           next() entry point                       │
│                                                                     │
│  ┌─────────────────┐    ┌─────────────────┐    ┌────────────────┐ │
│  │ Task Selection  │───▶│ Task Claiming   │───▶│ Learnings      │ │
│  │ (select_next_   │    │ (if --claim)    │    │ (recall_       │ │
│  │  task)          │    │                 │    │  learnings)    │ │
│  └─────────────────┘    └─────────────────┘    └────────────────┘ │
│          │                       │                      │         │
│          ▼                       ▼                      ▼         │
│   SelectionResult        ClaimResult            RecallResult      │
│   - task                 - task_id              - learnings       │
│   - batch_tasks          - run_id               - count           │
│   - selection_reason     - iteration            - metadata        │
│   - eligible_count                                                │
│                                                                     │
│                              │                                     │
│                              ▼                                     │
│                        NextResult                                  │
│                  (combined output)                                │
└───────────────────────────────────────────────────────────────────┘
```

## Data Flow

### Step 1: Task Selection

The `select_next_task()` function (from US-013) runs the scoring algorithm:

```rust
let selection = select_next_task(dir, &after_files, &recently_completed)?;
```

**Inputs:**
- `dir`: Database directory
- `after_files`: Files modified in previous iteration (for locality scoring)
- `recently_completed`: Task IDs recently completed (for synergy/conflict scoring)

**Output:** `SelectionResult` containing the best task or None.

### Step 2: Task Claiming (Optional)

If `--claim` flag is provided, update the task state:

```rust
if claim {
    claim_task(conn, &task.id, run_id)?;
}
```

**Operations:**
1. Update task status: `todo` → `in_progress`
2. Set `started_at` to current timestamp
3. If `--run-id` provided:
   - Insert `run_tasks` entry linking task to run
   - Set `run_tasks.status` to `started`
   - Set `run_tasks.started_at` to current timestamp
4. Increment `global_state.iteration_counter`

### Step 3: Learnings Retrieval

After task selection, query relevant learnings:

```rust
let recall_params = RecallParams {
    for_task: Some(task.id.clone()),
    limit: 5, // Default limit
    ..Default::default()
};
let recall_result = recall_learnings(conn, recall_params)?;
```

**Matching criteria (from learnings-recall.md):**
- File pattern matching: +10 points per matching file
- Task type prefix matching: +5 points
- Error pattern matching: +2 points (if task has errors)

## JSON Output Schema

### Complete NextResult Structure

```json
{
  "task": {
    "id": "US-014",
    "title": "Implement next command with claim and learnings",
    "description": "Complete the next command...",
    "priority": 23,
    "status": "in_progress",
    "acceptance_criteria": ["...", "..."],
    "notes": "The next command is the core interface...",
    "files": ["src/commands/next.rs"],
    "batch_with": [],
    "score": {
      "total": 977,
      "priority": 977,
      "file_overlap": 0,
      "synergy": 0,
      "conflict": 0,
      "file_overlap_count": 0,
      "synergy_from": [],
      "conflict_from": []
    }
  },
  "batch_tasks": [],
  "learnings": [
    {
      "id": 42,
      "title": "next command scoring pattern",
      "outcome": "pattern",
      "confidence": "high",
      "content": "When implementing scoring, use weighted factors...",
      "applies_to_files": ["src/commands/next.rs"],
      "applies_to_task_types": ["US-"]
    }
  ],
  "selection": {
    "reason": "Selected task US-014 with score 977",
    "eligible_count": 3
  },
  "claim": {
    "claimed": true,
    "run_id": "abc-123-uuid",
    "iteration": 42
  }
}
```

### Schema Field Definitions

| Field | Type | Description |
|-------|------|-------------|
| `task` | `Object \| null` | The selected task with full details |
| `task.id` | `string` | Task identifier |
| `task.title` | `string` | Task title |
| `task.description` | `string?` | Full task description |
| `task.priority` | `i32` | Task priority (1 = highest) |
| `task.status` | `string` | Current status (`todo`, `in_progress`, etc.) |
| `task.acceptance_criteria` | `string[]` | List of acceptance criteria |
| `task.notes` | `string?` | Additional notes |
| `task.files` | `string[]` | Files this task touches |
| `task.batch_with` | `string[]` | Task IDs in batchWith relationship |
| `task.score` | `Object` | Score breakdown for transparency |
| `batch_tasks` | `string[]` | Eligible batch tasks (todo status) |
| `learnings` | `Object[]` | Relevant learnings for this task |
| `selection` | `Object` | Selection metadata |
| `selection.reason` | `string` | Human-readable selection reason |
| `selection.eligible_count` | `i32` | Number of tasks considered |
| `claim` | `Object?` | Claim metadata (only if `--claim`) |
| `claim.claimed` | `bool` | Whether task was claimed |
| `claim.run_id` | `string?` | Run ID if tracking |
| `claim.iteration` | `i64` | Global iteration counter |

### Empty Response (No Tasks)

```json
{
  "task": null,
  "batch_tasks": [],
  "learnings": [],
  "selection": {
    "reason": "No eligible tasks found - all tasks are complete or blocked",
    "eligible_count": 0
  },
  "claim": null
}
```

## CLI Flags

From `cli.rs`, the Next command accepts:

| Flag | Type | Description |
|------|------|-------------|
| `--after-files` | `Vec<String>` | Files modified in previous iteration |
| `--claim` | `bool` | Claim the task (set status to in_progress) |
| `--run-id` | `Option<String>` | Link to run tracking |

## Implementation Plan

### Structs to Add

```rust
/// Result of the next command.
#[derive(Debug, Clone, Serialize)]
pub struct NextResult {
    /// The selected task (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<NextTaskOutput>,
    /// Eligible batch tasks
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub batch_tasks: Vec<String>,
    /// Relevant learnings for this task
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub learnings: Vec<LearningSummary>,
    /// Selection metadata
    pub selection: SelectionMetadata,
    /// Claim metadata (only if --claim)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimMetadata>,
}

/// Task output with score breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct NextTaskOutput {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub priority: i32,
    pub status: String,
    pub acceptance_criteria: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub files: Vec<String>,
    pub batch_with: Vec<String>,
    pub score: ScoreOutput,
}

/// Score breakdown for transparency.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreOutput {
    pub total: i32,
    pub priority: i32,
    pub file_overlap: i32,
    pub synergy: i32,
    pub conflict: i32,
    pub file_overlap_count: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub synergy_from: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflict_from: Vec<String>,
}

/// Learning summary for next output.
#[derive(Debug, Clone, Serialize)]
pub struct LearningSummary {
    pub id: i64,
    pub title: String,
    pub outcome: String,
    pub confidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applies_to_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applies_to_task_types: Option<Vec<String>>,
}

/// Selection metadata.
#[derive(Debug, Clone, Serialize)]
pub struct SelectionMetadata {
    pub reason: String,
    pub eligible_count: usize,
}

/// Claim metadata.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimMetadata {
    pub claimed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub iteration: i64,
}
```

### Functions to Add

```rust
/// Main entry point for the next command.
pub fn next(
    dir: &Path,
    after_files: &[String],
    claim: bool,
    run_id: Option<&str>,
) -> TaskMgrResult<NextResult> {
    let conn = open_connection(dir)?;

    // Step 1: Select best task
    let selection = select_next_task(dir, after_files, &[])?;

    // Return early if no task selected
    let Some(scored_task) = selection.task else {
        return Ok(NextResult {
            task: None,
            batch_tasks: vec![],
            learnings: vec![],
            selection: SelectionMetadata {
                reason: selection.selection_reason,
                eligible_count: selection.eligible_count,
            },
            claim: None,
        });
    };

    // Step 2: Claim task if requested
    let claim_metadata = if claim {
        Some(claim_task(&conn, &scored_task.task.id, run_id)?)
    } else {
        None
    };

    // Step 3: Retrieve relevant learnings
    let recall_params = RecallParams {
        for_task: Some(scored_task.task.id.clone()),
        limit: 5,
        ..Default::default()
    };
    let recall_result = recall_learnings(&conn, recall_params)?;

    // Build output
    Ok(NextResult {
        task: Some(build_task_output(&scored_task)),
        batch_tasks: selection.batch_tasks,
        learnings: recall_result.learnings.into_iter()
            .map(|l| LearningSummary::from(l))
            .collect(),
        selection: SelectionMetadata {
            reason: selection.selection_reason,
            eligible_count: selection.eligible_count,
        },
        claim: claim_metadata,
    })
}

/// Claim a task by setting status to in_progress.
fn claim_task(
    conn: &Connection,
    task_id: &str,
    run_id: Option<&str>,
) -> TaskMgrResult<ClaimMetadata> {
    // Update task status
    conn.execute(
        "UPDATE tasks SET status = 'in_progress', started_at = datetime('now') WHERE id = ?1",
        [task_id],
    )?;

    // Link to run if run_id provided
    if let Some(rid) = run_id {
        conn.execute(
            r#"
            INSERT INTO run_tasks (run_id, task_id, status, started_at)
            VALUES (?1, ?2, 'started', datetime('now'))
            "#,
            [rid, task_id],
        )?;
    }

    // Increment global iteration counter
    conn.execute(
        "UPDATE global_state SET iteration_counter = iteration_counter + 1, last_task_id = ?1, updated_at = datetime('now')",
        [task_id],
    )?;

    // Get current iteration
    let iteration: i64 = conn.query_row(
        "SELECT iteration_counter FROM global_state WHERE id = 1",
        [],
        |row| row.get(0),
    )?;

    Ok(ClaimMetadata {
        claimed: true,
        run_id: run_id.map(String::from),
        iteration,
    })
}
```

## Edge Cases

### 1. No Eligible Tasks

**Scenario:** All tasks are complete, blocked, or in non-todo status.

**Behavior:** Return `NextResult` with `task: null` and explanatory reason.

```json
{
  "task": null,
  "selection": {
    "reason": "No eligible tasks found - all tasks are complete or blocked by dependencies",
    "eligible_count": 0
  }
}
```

### 2. Run Tracking Failures

**Scenario:** `--run-id` provided but run doesn't exist or is not active.

**Behavior:** Return error before claiming task. Don't partially claim.

```rust
if let Some(rid) = run_id {
    // Verify run exists and is active
    let run_status: String = conn.query_row(
        "SELECT status FROM runs WHERE run_id = ?1",
        [rid],
        |row| row.get(0),
    ).map_err(|_| TaskMgrError::run_not_found(rid))?;

    if run_status != "active" {
        return Err(TaskMgrError::invalid_state("run", rid, "active", &run_status));
    }
}
```

### 3. Learning Recall Errors

**Scenario:** Error during learnings retrieval (e.g., malformed JSON in DB).

**Behavior:** Log warning, return empty learnings list. Don't fail the entire command.

```rust
let learnings = match recall_learnings(&conn, recall_params) {
    Ok(result) => result.learnings,
    Err(e) => {
        eprintln!("Warning: failed to retrieve learnings: {}", e);
        vec![]
    }
};
```

### 4. Task Already Claimed

**Scenario:** Task selected by `next` is already `in_progress` (race condition or stale state).

**Behavior:** With `--claim`, this shouldn't happen because we only select `todo` tasks. If it occurs:
- Return the task anyway (selection result)
- Skip the claim operation
- Set `claim.claimed: false` in output

### 5. Empty Batch Group

**Scenario:** Task has `batchWith` relationships, but all targets are already done.

**Behavior:** `batch_tasks` is empty. This is normal - the primary task is still returned.

### 6. Multiple Calls Without Completion

**Scenario:** `next --claim` called multiple times without completing tasks.

**Behavior:**
- Each call selects a different task (since claimed tasks are `in_progress`, not `todo`)
- Multiple tasks end up in `in_progress` state
- `doctor` command can detect and fix this later

### 7. Learning Content Truncation

**Scenario:** Learning content is very long (>1000 chars).

**Behavior:** In JSON output, include full content. For text format, truncate with ellipsis.

```rust
fn truncate_content(content: &str, max_len: usize) -> String {
    if content.len() <= max_len {
        content.to_string()
    } else {
        format!("{}...", &content[..max_len])
    }
}
```

## Text Output Format

For `--format text`, format the output as:

```
Next Task: US-014 - Implement next command with claim and learnings
============================================================
Priority: 23
Score:    977 (priority: 977, file_overlap: 0, synergy: 0, conflict: 0)

Description:
  Complete the next command to claim tasks, increment iteration counter,
  and include relevant learnings in output.

Acceptance Criteria:
  [ ] Extend next.rs next() function as main entry point
  [ ] If --claim flag, update task status to in_progress...
  ...

Files:
  - src/commands/next.rs

Claimed: Yes (run: abc-123, iteration: 42)

Relevant Learnings (2):
  1. [Pattern] next command scoring pattern (high confidence)
     When implementing scoring, use weighted factors...

  2. [Failure] Selection algorithm edge case (medium confidence)
     Watch out for circular dependencies...

Eligible Tasks: 3
```

## Integration with Run Lifecycle

The `next` command integrates with run tracking when `--run-id` is provided:

1. **Begin Run** (`run begin`):
   - Creates run record with `status='active'`
   - Returns `run_id` to pass to subsequent commands

2. **Next with Claim** (`next --claim --run-id <id>`):
   - Creates `run_tasks` entry linking task to run
   - Increments iteration counter in `global_state`

3. **Complete Task** (`complete <task_id> --run-id <id>`):
   - Updates `run_tasks` entry with `ended_at`, `duration_seconds`
   - Sets `run_tasks.status = 'completed'`

4. **End Run** (`run end --run-id <id> --status completed`):
   - Sets `runs.ended_at`
   - Sets `runs.status = 'completed'` or `'aborted'`

## Testing Strategy

### Unit Tests

1. `test_next_no_tasks` - Empty database returns null task
2. `test_next_selects_task` - Returns task with correct fields
3. `test_next_with_claim` - Updates task status and iteration
4. `test_next_with_run_id` - Creates run_tasks entry
5. `test_next_includes_learnings` - Retrieves relevant learnings
6. `test_next_batch_tasks` - Includes eligible batch tasks

### Integration Tests

1. `test_next_end_to_end` - Full workflow: init → next → complete → next
2. `test_next_with_run_tracking` - Run begin → next --claim → complete → run end
3. `test_next_output_json` - Verify JSON structure matches schema
4. `test_next_output_text` - Verify text format is readable

## Dependencies

This design depends on:
- **US-013** (task selection algorithm) - Completed ✅
- **US-020** (learnings recall) - Completed ✅
- **US-007** (run model) - Completed ✅
- **US-005-40** (database schema) - Completed ✅

This design enables:
- **US-015** (complete command) - Needs claim state
- **US-016** (fail command) - Needs claim state
- **REVIEW-004** (code review) - Reviews this integration
