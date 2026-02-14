# Task Selection Algorithm Design

## Overview

The task selection algorithm determines which task an AI agent should work on next in the task-mgr CLI tool. It balances multiple factors: task priority, file locality, relationship hints (synergy/conflict), and batch grouping.

## Goals

1. **Deterministic ordering** - Same inputs produce same selection
2. **Context preservation** - Prefer tasks that share files with recent work
3. **Dependency respect** - Never select tasks with unsatisfied dependencies
4. **Soft hints** - Use synergy/conflict relationships as scoring adjustments
5. **Batch awareness** - Identify related tasks for combined implementation

## Candidate Approaches Considered

### Approach 1: Pure Priority-Based Selection
Select the highest priority task that has all dependencies satisfied.

**Pros:**
- Simple to implement
- Predictable behavior
- Easy to reason about

**Cons:**
- Ignores file locality (context switching overhead)
- Doesn't leverage relationship metadata
- No benefit from synergy hints

### Approach 2: File Locality Only
Select tasks that share the most files with recently modified files.

**Pros:**
- Minimizes context switching
- Natural grouping of related work

**Cons:**
- Ignores priority completely
- Could leave high-priority work undone
- No dependency handling

### Approach 3: Weighted Multi-Factor Scoring (Chosen)
Combine priority, file overlap, and relationship hints into a single score.

**Pros:**
- Balances all factors
- Configurable weights for tuning
- Respects both priority and locality
- Uses soft hints without over-weighting them

**Cons:**
- More complex scoring logic
- Weights require tuning
- Harder to predict which task will be selected

## Chosen Design: Weighted Multi-Factor Scoring

### Scoring Formula

```
total_score = priority_score + file_score + synergy_score + conflict_score

where:
  priority_score = 1000 - priority          (range: ~900-999 for typical priorities)
  file_score     = 10 * file_overlap_count  (10 points per matching file)
  synergy_score  = 3 * synergy_count        (3 points per synergy relationship)
  conflict_score = -5 * conflict_count      (-5 penalty per conflict relationship)
```

### Weight Constants

| Constant | Value | Rationale |
|----------|-------|-----------|
| `PRIORITY_BASE` | 1000 | Ensures priority is the dominant factor |
| `FILE_OVERLAP_SCORE` | 10 | 3 file overlaps (~30) overcomes ~3 priority levels |
| `SYNERGY_BONUS` | 3 | Mild preference, doesn't override priority |
| `CONFLICT_PENALTY` | -5 | Mild avoidance, doesn't completely block |

### Design Rationale

1. **Priority dominance**: A priority-1 task scores ~999, priority-50 scores ~950. This ~50-point range means file overlap can meaningfully influence selection but not completely override priority.

2. **File overlap significance**: 10 points per file means 3+ file overlaps can shift selection by ~30 points, roughly 3 priority levels. This makes locality meaningful without being overwhelming.

3. **Synergy as tie-breaker**: At 3 points, synergy only matters when tasks are close in other scores. It's a "prefer this if all else is equal" hint.

4. **Conflict as soft avoidance**: At -5 points, conflict discourages immediate selection but doesn't block. If a conflicting task is truly the best choice, it can still win.

### Tiebreaking

When total scores are equal:
1. Select the task with lower priority number (higher priority)
2. If still tied, deterministic ordering by task ID

```rust
scored_tasks.sort_by(|a, b| {
    b.total_score
        .cmp(&a.total_score)
        .then_with(|| a.task.priority.cmp(&b.task.priority))
});
```

## Eligibility Filtering

Before scoring, tasks are filtered to only eligible candidates:

### Hard Constraints
1. **Status must be `todo`** - Only unstarted tasks are considered
2. **All `dependsOn` tasks must be satisfied** - Either `done` or `irrelevant`

### Dependency Satisfaction
A dependency is considered satisfied if the target task has status:
- `done` - Successfully completed
- `irrelevant` - Deliberately marked as no longer needed

This allows the dependency chain to unblock when a prerequisite is either completed or determined to be unnecessary.

## Batch Task Handling

### Identification
When a task is selected, the algorithm also identifies batch candidates:
- Tasks listed in the selected task's `batchWith` relationship
- That are also in `todo` status

### Output
The selection result includes:
- `task`: The primary selected task
- `batch_tasks`: IDs of eligible batch candidates
- The caller decides whether to implement all together

## Edge Cases

### 1. No Tasks Available
**Scenario**: Database has no tasks, or all tasks are complete.
**Behavior**: Return `SelectionResult` with `task: None` and appropriate message.

### 2. All Tasks Blocked
**Scenario**: All `todo` tasks have unsatisfied dependencies.
**Behavior**: Return `SelectionResult` with `task: None`. The message indicates tasks exist but are blocked.

### 3. Empty Batch Groups
**Scenario**: Task has `batchWith` relationships, but all targets are already done.
**Behavior**: `batch_tasks` is empty. Primary task is still selected.

### 4. Circular Dependencies
**Scenario**: Task A depends on Task B, Task B depends on Task A.
**Behavior**: Both tasks remain blocked forever. This is a PRD authoring error.
**Mitigation**: The `doctor` command could detect and report circular dependencies.

### 5. Self-Referential Relationships
**Scenario**: Task has itself in `dependsOn` or `batchWith`.
**Behavior**: The task blocks itself (for dependsOn) or lists itself in batch (harmless).
**Mitigation**: Consider validating during import.

### 6. Ties with Multiple High-Scoring Tasks
**Scenario**: Multiple tasks have identical total scores.
**Behavior**: Deterministic tiebreaking by priority, then task ID.

### 7. Very Large File Overlap
**Scenario**: A task touches 20 files that all overlap with `after_files`.
**Behavior**: +200 file score dominates. This is intentional - high locality should win.

### 8. Maximum Priority Conflict
**Scenario**: Priority-1 task has 10 conflict relationships with recently completed tasks.
**Behavior**: Score = 999 - 50 = 949. Still likely to be selected unless alternatives exist.

## Pseudocode

```pseudocode
function select_next_task(after_files, recently_completed):
    # Get state from database
    completed_ids = tasks WHERE status IN ('done', 'irrelevant')
    todo_tasks = tasks WHERE status = 'todo'
    relationships = load all task_relationships
    task_files = load all task_files

    # Filter to eligible tasks
    eligible = []
    for task in todo_tasks:
        deps = relationships[task.id].dependsOn
        if all(dep in completed_ids for dep in deps):
            eligible.append(task)

    if empty(eligible):
        return NO_TASK_RESULT

    # Score each task
    scored = []
    for task in eligible:
        files = task_files[task.id]

        # Calculate scores
        priority_score = 1000 - task.priority
        file_score = 10 * count(files INTERSECT after_files)
        synergy_score = 3 * count(relationships[task.id].synergyWith INTERSECT recently_completed)
        conflict_score = -5 * count(relationships[task.id].conflictsWith INTERSECT recently_completed)

        total = priority_score + file_score + synergy_score + conflict_score
        scored.append((task, total))

    # Sort and select
    sort scored BY total DESC, priority ASC

    selected = scored[0]
    batch_candidates = get_eligible_batch_tasks(selected.batchWith)

    return SelectionResult(selected, batch_candidates)
```

## Implementation Notes

1. **HashMap for relationships**: Store relationships grouped by task_id for O(1) lookup.

2. **HashSet for after_files**: Convert input list to set for O(1) membership testing.

3. **Single-pass scoring**: Iterate eligible tasks once, calculating all scores inline.

4. **Database queries**: Use bulk queries (one per relationship type) rather than per-task queries.

## Future Enhancements

1. **Configurable weights**: Allow weights to be adjusted via CLI flags or config.

2. **Verbose scoring output**: `--verbose` flag to show scores for top N candidates.

3. **Scoring decay**: Recently completed tasks could have diminishing synergy/conflict influence over time.

4. **Learning integration**: Boost scores for tasks similar to successfully completed learnings.

5. **Circular dependency detection**: `doctor` command enhancement to identify and report cycles.
