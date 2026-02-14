# Learnings Recall Algorithm Design

## Overview

The learnings recall algorithm retrieves relevant learnings from the institutional memory system based on various query criteria. When an AI agent is about to work on a task, it can request learnings that might help based on:
- Files the task touches
- Task type prefix (US-, FIX-, SEC-, etc.)
- Free-text search on title/content
- Tags
- Outcome type (failure, success, workaround, pattern)

## Goals

1. **Relevance-based retrieval** - Return learnings most likely to help with the current task
2. **Multiple query modes** - Support file patterns, task-based lookup, text search, and tag filtering
3. **Recency preference** - Prioritize most recently useful learnings
4. **Configurable limits** - Control result count to fit context window
5. **Update statistics** - Track when learnings are shown for future ranking

## Database Schema (Existing)

The learnings table has several fields relevant to recall:

```sql
learnings (
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK(outcome IN ('failure', 'success', 'workaround', 'pattern')),
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    applies_to_files TEXT,        -- JSON array of file patterns
    applies_to_task_types TEXT,   -- JSON array of task type prefixes
    applies_to_errors TEXT,       -- JSON array of error patterns
    confidence TEXT NOT NULL DEFAULT 'medium',
    times_shown INTEGER NOT NULL DEFAULT 0,
    times_applied INTEGER NOT NULL DEFAULT 0,
    last_shown_at TEXT,
    last_applied_at TEXT
)

learning_tags (
    learning_id INTEGER REFERENCES learnings(id),
    tag TEXT NOT NULL,
    UNIQUE(learning_id, tag)
)
```

## CLI Parameters

From `cli.rs`, the Recall command accepts:
- `--query`: Free-text search on title and content
- `--for-task`: Task ID to find matching learnings
- `--tags`: Comma-separated tag filters
- `--outcome`: Filter by outcome type
- `--limit`: Maximum results (default 5)

## Matching Strategies

### 1. File Pattern Matching

**Use case**: Find learnings relevant to files a task touches.

**Approach**: SQLite GLOB pattern matching.

The `applies_to_files` field stores JSON arrays like `["src/db/*.rs", "Cargo.toml"]`. We need to check if any file the task touches matches any pattern in the learning.

**SQL Approach**:
```sql
-- For each task file, check if it matches any pattern in the learning
-- This requires iterating patterns which isn't efficient in pure SQL
-- Better to do pattern matching in Rust code
```

**Rust Approach** (preferred):
1. Load learnings with non-null `applies_to_files`
2. For each learning, parse JSON array into `Vec<String>`
3. For each task file, check if it matches any pattern using `glob::Pattern`
4. Return learnings where any pattern matched

### 2. Task Type Prefix Matching

**Use case**: Find learnings that apply to a specific task type (US-, FIX-, SEC-).

**Approach**: Prefix matching with LIKE.

The `applies_to_task_types` field stores prefixes like `["US-", "FIX-"]`. We check if the task ID starts with any prefix.

**SQL Approach**:
```sql
-- Load learnings where any stored prefix matches the task ID
SELECT * FROM learnings
WHERE applies_to_task_types IS NOT NULL
  AND (
    applies_to_task_types LIKE '%"US-"%'  -- Check if JSON contains prefix
  )
```

This SQL approach is fragile because it searches JSON as text. Better to do in Rust.

**Rust Approach** (preferred):
1. Extract task type prefix from task ID (e.g., "US-" from "US-001")
2. Load learnings with non-null `applies_to_task_types`
3. Parse JSON arrays and check if any prefix matches the task's prefix

### 3. Free-Text Search

**Use case**: Search learnings by keyword in title or content.

**SQL Approach** (LIKE-based, Phase 1):
```sql
SELECT * FROM learnings
WHERE title LIKE '%keyword%' OR content LIKE '%keyword%'
ORDER BY last_applied_at DESC NULLS LAST, created_at DESC
LIMIT ?
```

**Future Enhancement** (FTS5, Phase 2):
```sql
-- With FTS5 virtual table
SELECT l.* FROM learnings l
JOIN learnings_fts f ON l.id = f.rowid
WHERE learnings_fts MATCH 'keyword'
ORDER BY bm25(learnings_fts), l.last_applied_at DESC NULLS LAST
LIMIT ?
```

For Phase 1, we'll use LIKE matching. FTS5 provides much better search quality and is deferred to Phase 2 (US-FTS).

### 4. Tag Filtering

**Use case**: Filter learnings by categorization tags.

**SQL Approach**:
```sql
SELECT DISTINCT l.* FROM learnings l
JOIN learning_tags lt ON l.id = lt.learning_id
WHERE lt.tag IN (?, ?, ?)  -- Multiple tags
ORDER BY l.last_applied_at DESC NULLS LAST, l.created_at DESC
LIMIT ?
```

### 5. Outcome Filtering

**Use case**: Filter learnings by outcome type (failure, success, etc.).

**SQL Approach**:
```sql
SELECT * FROM learnings
WHERE outcome = ?
ORDER BY last_applied_at DESC NULLS LAST, created_at DESC
LIMIT ?
```

## Combined Query Strategy

When multiple filters are provided, they should be combined with AND logic:
- `--query` AND `--outcome` means text matches AND outcome type matches
- `--for-task` AND `--tags` means task-relevant AND has any of the specified tags

### Query Building

```rust
fn build_recall_query(params: &RecallParams) -> (String, Vec<&dyn ToSql>) {
    let mut conditions = Vec::new();
    let mut params = Vec::new();

    if let Some(query) = &params.query {
        conditions.push("(title LIKE ? OR content LIKE ?)");
        let pattern = format!("%{}%", query);
        params.push(&pattern);
        params.push(&pattern);
    }

    if let Some(outcome) = &params.outcome {
        conditions.push("outcome = ?");
        params.push(outcome.as_db_str());
    }

    // Tags require a subquery or JOIN
    if let Some(tags) = &params.tags {
        let placeholders = vec!["?"; tags.len()].join(", ");
        conditions.push(&format!(
            "id IN (SELECT learning_id FROM learning_tags WHERE tag IN ({}))",
            placeholders
        ));
        for tag in tags {
            params.push(tag);
        }
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    (where_clause, params)
}
```

## Relevance Ranking

### Phase 1: Most Recently Useful

Order learnings by when they were last applied/useful, then by creation date:

```sql
ORDER BY last_applied_at DESC NULLS LAST, created_at DESC
```

This ensures:
1. Learnings that were recently applied (marked useful) appear first
2. Never-applied learnings are sorted by recency of creation
3. The `NULLS LAST` ensures never-applied learnings don't get priority

### Phase 2 (Deferred): UCB Bandit Ranking

The UCB (Upper Confidence Bound) algorithm balances exploitation (proven learnings) with exploration (new learnings). This is deferred to US-021 in Phase 2.

## Task-Based Recall (`--for-task`)

When recalling learnings for a specific task:

1. **Look up the task** to get:
   - Task ID (for prefix extraction)
   - Task files (from `task_files` table)

2. **Build matching criteria**:
   - Extract task type prefix (e.g., "US-" from "US-001")
   - Get list of task files

3. **Score learnings**:
   - +10 for file pattern match
   - +5 for task type prefix match
   - +2 for error pattern match (if task has errors)

4. **Filter and rank**:
   - Include learnings that match ANY criterion
   - Order by match score DESC, then last_applied_at DESC

### Implementation Pseudocode

```rust
fn recall_for_task(conn: &Connection, task_id: &str) -> TaskMgrResult<Vec<Learning>> {
    // Get task info
    let task = get_task(conn, task_id)?;
    let task_files = get_task_files(conn, task_id)?;
    let task_prefix = extract_prefix(task_id); // "US-001" -> "US-"
    let task_error = task.last_error.as_ref();

    // Load candidate learnings (those with any applicability metadata)
    let candidates = load_learnings_with_applicability(conn)?;

    // Score each learning
    let mut scored: Vec<(Learning, i32)> = Vec::new();
    for learning in candidates {
        let mut score = 0;

        // File pattern matching
        if let Some(patterns) = &learning.applies_to_files {
            for file in &task_files {
                if patterns.iter().any(|p| glob_match(p, file)) {
                    score += 10;
                    break;
                }
            }
        }

        // Task type matching
        if let Some(prefixes) = &learning.applies_to_task_types {
            if prefixes.iter().any(|p| task_prefix.starts_with(p)) {
                score += 5;
            }
        }

        // Error pattern matching
        if let (Some(error), Some(patterns)) = (task_error, &learning.applies_to_errors) {
            if patterns.iter().any(|p| error.contains(p)) {
                score += 2;
            }
        }

        if score > 0 {
            scored.push((learning, score));
        }
    }

    // Sort by score DESC, then by last_applied_at DESC
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| compare_option_dates(&b.0.last_applied_at, &a.0.last_applied_at))
    });

    // Return top N
    Ok(scored.into_iter().map(|(l, _)| l).take(limit).collect())
}
```

## Edge Cases

### 1. No Matches
**Scenario**: Query returns no matching learnings.
**Behavior**: Return empty vector. The caller should handle gracefully.

### 2. Too Many Matches
**Scenario**: Query matches hundreds of learnings.
**Behavior**: Apply limit (default 5) after ranking. Top N most relevant returned.

### 3. Empty Applicability Fields
**Scenario**: Learning has no `applies_to_files`, `applies_to_task_types`, etc.
**Behavior**: Such learnings only match via text search or tags, not via task-based recall.

### 4. Malformed JSON in Applicability Fields
**Scenario**: `applies_to_files` contains invalid JSON.
**Behavior**: Skip that field (graceful degradation). Log warning in verbose mode.

### 5. Case Sensitivity
**Scenario**: Query "ERROR" should match "error".
**Behavior**: LIKE queries are case-insensitive in SQLite by default for ASCII. Consider `LOWER()` for consistency.

### 6. Partial Tag Matches
**Scenario**: Tag filter "rust" should not match "rusty".
**Behavior**: Tags must match exactly. Use exact equality, not LIKE.

### 7. Empty Query with Filters
**Scenario**: `recall --outcome failure --limit 10` (no text query).
**Behavior**: Return all failure learnings, ordered by recency, limited to 10.

## Updating Statistics

When learnings are shown to an agent, update tracking fields:

```sql
UPDATE learnings
SET times_shown = times_shown + 1,
    last_shown_at = datetime('now')
WHERE id IN (?, ?, ?, ?, ?)
```

This supports future UCB ranking by tracking how often learnings are shown.

## SQL Queries Summary

### Query 1: Text Search
```sql
SELECT * FROM learnings
WHERE (title LIKE ? OR content LIKE ?)
ORDER BY last_applied_at DESC NULLS LAST, created_at DESC
LIMIT ?
```

### Query 2: Outcome Filter
```sql
SELECT * FROM learnings
WHERE outcome = ?
ORDER BY last_applied_at DESC NULLS LAST, created_at DESC
LIMIT ?
```

### Query 3: Tag Filter
```sql
SELECT DISTINCT l.* FROM learnings l
JOIN learning_tags lt ON l.id = lt.learning_id
WHERE lt.tag IN (?, ?, ?)
ORDER BY l.last_applied_at DESC NULLS LAST, l.created_at DESC
LIMIT ?
```

### Query 4: Combined Filters
```sql
SELECT DISTINCT l.* FROM learnings l
LEFT JOIN learning_tags lt ON l.id = lt.learning_id
WHERE (l.title LIKE ? OR l.content LIKE ?)
  AND l.outcome = ?
  AND lt.tag IN (?, ?)
ORDER BY l.last_applied_at DESC NULLS LAST, l.created_at DESC
LIMIT ?
```

### Query 5: Learnings with Applicability (for task-based recall)
```sql
SELECT * FROM learnings
WHERE applies_to_files IS NOT NULL
   OR applies_to_task_types IS NOT NULL
   OR applies_to_errors IS NOT NULL
```

### Query 6: Update Show Statistics
```sql
UPDATE learnings
SET times_shown = times_shown + 1,
    last_shown_at = datetime('now')
WHERE id IN (?, ?, ?, ?, ?)
```

## Implementation Structure

```
src/learnings/
├── mod.rs          # Re-exports
├── crud.rs         # Create/read operations (exists)
└── recall.rs       # Recall operations (new)
    ├── RecallParams        # Input parameters struct
    ├── RecallResult        # Output struct with learnings
    ├── recall_learnings()  # Main entry point
    ├── recall_by_text()    # Text search helper
    ├── recall_by_tags()    # Tag filter helper
    ├── recall_for_task()   # Task-based matching
    └── update_shown()      # Update statistics
```

## Future Enhancements

1. **FTS5 Integration** (US-FTS): Replace LIKE with full-text search for better relevance.

2. **UCB Bandit Ranking** (US-021): Balance exploitation and exploration in ranking.

3. **Semantic Similarity**: Use embedding vectors for semantic matching (requires additional infrastructure).

4. **Error Pattern Matching**: Enhanced error pattern matching with regex support.

5. **Learning Decay**: Down-rank very old learnings that haven't been applied recently.

6. **Context-Aware Recall**: Consider current run context and recently completed tasks.
