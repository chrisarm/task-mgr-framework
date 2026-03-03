# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Stream-JSON Learning Extraction** for **task-mgr**.

## Problem Statement

The learning extraction system receives only `--print` text output from Claude iterations. `--print` captures only the final assistant text response — not tool calls, file reads/writes, errors, or intermediate reasoning. Successful iterations typically end with just `<completed>task-id</completed>`, providing zero useful content for learning extraction. This enhancement switches loop iteration spawns to `--output-format stream-json --no-session-persistence`, capturing the full conversation (tool calls, results, errors) inline during the iteration for richer learning extraction.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Read `qualityDimensions`** on the task — these define what "good" looks like
2. **Read `edgeCases`/`invariants`/`failureModes`** on TEST-INIT tasks — each must be handled and tested
3. **State assumptions, consider 2-3 approaches**, pick the best
4. **After coding, self-critique**: "Is this correct for all edge cases? Is it idiomatic? Is it efficient?" — revise if improvements exist

---

## Priority Philosophy

What matters most, in order:

1. **PLAN** - Anticipate edge cases. Tests verify boundaries work correctly
2. **FUNCTIONING CODE** - Pragmatic, reliable code that works according to plan
3. **CORRECTNESS** - Code compiles, type-checks, all tests pass deterministically
4. **CODE QUALITY** - Clean code, good patterns, no warnings
5. **POLISH** - Documentation, formatting, minor improvements

**Key Principles:**

- **Tests first**: Write initial tests before implementation to define expected behavior
- **Approach before code**: Consider 2-3 approaches with tradeoffs, pick the best, then implement
- **Self-critique after code**: Review your own implementation for correctness, style, and performance before moving on
- **Quality dimensions explicit**: Read `qualityDimensions` on the task — these define what "good" looks like
- Test boundaries and exceptions—edge cases are where bugs hide
- Handle `Option`/`Result` explicitly; avoid `unwrap()` in production—use `expect()` with messages or proper error propagation
- Implementation goal: make the initial tests pass, then expand coverage

**Prohibited outcomes:**

- Tests that only assert "no crash" or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context

---

## Task Files (IMPORTANT)

These are the files you will read and modify during the loop:

| File | Purpose |
|------|---------|
| `tasks/session-learning-extraction.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/session-learning-extraction-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings (auto-appended, needs periodic curation) |

When review tasks add new tasks, they modify `tasks/session-learning-extraction.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `tasks/session-learning-extraction.json`
2. Read the progress log at `tasks/progress.txt` (if exists)
3. Read `tasks/long-term-learnings.md` for curated project patterns (persists across branches)
4. Read `CLAUDE.md` for project patterns
5. Verify you're on the correct branch from PRD `branchName`
6. **Select the best task** using Smart Task Selection below
7. **Pre-implementation review** (before writing code):
   a. Read the task's `qualityDimensions` if present — these define what "good" looks like
   b. Read `edgeCases`, `invariants`, and `failureModes` on TEST-INIT tasks
   c. State your assumptions explicitly — hidden assumptions create bugs
   d. Consider 2-3 implementation approaches with tradeoffs (even briefly), pick the best
   e. For each known edge case, plan how it will be handled BEFORE coding
   f. Document your chosen approach in a brief comment in `progress.txt`
8. **Implement** that single user story, following your chosen approach
9. **Self-critique** (after implementation, before quality checks):
   - Review for correctness, idiomatic style, and performance. Revise if improvements exist
   - Check each `qualityDimensions` constraint: does the code satisfy it?
   - If the implementation can exit early, avoid redundant work, or be simplified — revise now
10. Run quality checks (see below)
11. If checks pass, commit with message: `feat: FULL-STORY-ID-completed - [Story Title]`
    For multiple tasks: `feat: ID1-completed, ID2-completed - [Title]`
12. Output `<completed>FULL-STORY-ID</completed>` — the loop will mark the task done and update the PRD automatically
13. Append progress to `tasks/progress.txt` (include approach chosen and any edge cases discovered)
14. For TEST-xxx tasks: ensure 80%+ coverage for new methods; use `assert_eq!` for string outputs

---

## Smart Task Selection

Tasks have relationship fields:

```json
{
  "touchesFiles": ["src/module/file.rs"],
  "dependsOn": ["FEAT-001"],
  "synergyWith": ["FEAT-002"],
  "batchWith": [],
  "conflictsWith": []
}
```

### Selection Algorithm

1. **Filter eligible**: `passes: false` AND all `dependsOn` complete
2. **Check synergy**: Prefer tasks where `synergyWith` contains the previous task's ID
3. **Check file overlap**: Prefer tasks with `touchesFiles` matching previous iteration's files
4. **Avoid conflicts**: Skip tasks in `conflictsWith` of recently completed tasks
5. **Tie-breaker**: If priorities tie, choose the one with most file overlap
6. **Fall back**: Pick highest priority (lowest number)

---

## Technical Context

### Key Files

- `src/loop_engine/claude.rs` — `spawn_claude()` function and `ClaudeResult` struct (main changes here)
- `src/loop_engine/engine.rs` — `run_iteration()`, learning extraction at ~line 506
- `src/learnings/ingestion/mod.rs` — `extract_learnings_from_output()`
- `src/learnings/ingestion/extraction.rs` — `build_extraction_prompt()`, `MAX_OUTPUT_CHARS`
- `src/commands/curate/mod.rs` — dedup curation spawn
- `src/commands/curate/enrich.rs` — enrichment spawn

### Stream-JSON Format

Each stdout line from `claude --output-format stream-json` is a JSON object with a `type` field:

```json
// Assistant message with content blocks
{"type":"assistant","message":{"content":[
  {"type":"text","text":"Let me read the file."},
  {"type":"tool_use","id":"toolu_abc","name":"Read","input":{"file_path":"/src/main.rs"}},
  {"type":"thinking","thinking":"..."}
]},"model":"claude-sonnet-4-6","error":null}

// User message (tool results)
{"type":"user","message":{"content":[
  {"type":"tool_result","tool_use_id":"toolu_abc","content":"fn main() {...}","is_error":false}
]}}

// Final result (always last line)
{"type":"result","subtype":"success","result":"<completed>TASK-ID</completed>","session_id":"..."}

// System init (skip)
{"type":"system","subtype":"init","data":{...}}
```

### Key Design: Two Code Paths in Tee Loop

- `stream_json=false` (utility spawns): existing behavior — read lines, tee to stderr, collect into `output`
- `stream_json=true` (loop iterations): parse JSON per line, extract text for stderr display, collect formatted conversation, extract `result.result` into `output`

### Existing Patterns

- `spawn_claude` uses `CLAUDE_BINARY=echo` in tests to capture args
- Graceful degradation pattern: `eprintln!("Warning: ...")` + continue/fallback
- Learning extraction uses `MAX_OUTPUT_CHARS = 50_000` truncation

---

## Quality Checks (REQUIRED)

Run from project root.

```bash
# 1. Format check
cargo fmt --check

# 2. Type check
cargo check

# 3. Linting
cargo clippy -- -D warnings

# 4. Tests
cargo test
```

**If checks fail:**

- Fix the issue (apply linter suggestions unless they conflict with philosophy)
- Re-run all checks
- Do NOT commit broken code

---

## Integration Verification Protocol (CRITICAL)

**New code must be fully wired in.**

### After Implementing New Code, Verify:

1. **Call Sites**: All 4 `spawn_claude` callers updated with `stream_json` param
2. **Engine Wiring**: `engine.rs` passes `stream_json=true` and uses `conversation` field for learning extraction
3. **No Dead Code**: `cargo check` shows no unused warnings for new code
4. **Arg Ordering**: `--output-format stream-json` appears before `-p` in args

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found.

### CODE-REVIEW-1 (Priority 13)

Review implementation for quality, security, and integration wiring. For each issue: add CODE-FIX-xxx or WIRE-FIX-xxx task to JSON, add to MILESTONE-1 dependsOn.

### REFACTOR-REVIEW-1/2/3

Review for DRY, complexity, coupling. Add REFACTOR-x-xxx tasks as needed.

---

## Progress Report Format

APPEND to `tasks/progress.txt`:

```
## [Date/Time] - [Story ID]
- What was implemented
- Files changed
- **Learnings:** (patterns, gotchas)
---
```

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in `tasks/progress.txt`
2. Create clarification task
3. Output `<promise>BLOCKED</promise>`

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
