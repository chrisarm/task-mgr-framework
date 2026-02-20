# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Model Selection — Phase 1 Review Fixes + Phase 2 Engine Integration** for **task-mgr**.

## Problem Statement

Phase 1 established the data model and pure model resolution logic. A thorough code review found 2 P1 bugs and 4 P2 improvements that must be fixed before Phase 2 can safely build on them. Phase 2 itself — wiring model selection into the live loop engine — is unimplemented: the `spawn_claude` subprocess receives no `--model` flag, the iteration header doesn't show the active model, and crash recovery doesn't escalate models.

This work covers both scopes:
- **Part A**: Fix 6 code review findings in the Phase 1 foundation
- **Part B**: Wire model resolution, subprocess flags, escalation policy, crash recovery, and observability into the engine

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
| `tasks/model-selection-phase2-integration.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/model-selection-phase2-integration-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings (auto-appended, needs periodic curation) |

When review tasks add new tasks, they modify `tasks/model-selection-phase2-integration.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `tasks/model-selection-phase2-integration.json`
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
11. If checks pass, commit with message: `feat: [Story ID] - [Story Title]`
12. Update `tasks/model-selection-phase2-integration.json` to set `passes: true` for the completed story
13. Append progress to `tasks/progress.txt` (include approach chosen and any edge cases discovered)
14. For TEST-xxx tasks: ensure 80%+ coverage for new methods; use `assert_eq!` for string outputs

---

## Smart Task Selection

Tasks have relationship fields:
```json
{
  "touchesFiles": ["src/module/file.rs"],
  "dependsOn": ["FEAT-001"],     // HARD: Must complete first
  "synergyWith": ["FEAT-002"],   // SOFT: Share context
  "batchWith": [],               // DIRECTIVE: Do together
  "conflictsWith": []            // AVOID: Don't sequence
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

## Architect Decisions (IMPORTANT)

These decisions were made during plan review and MUST be followed:

1. **Crash escalation with None model**: When crash recovery fires and resolved_model is None, assume SONNET_MODEL as the baseline and escalate to OPUS_MODEL. Do NOT call `escalate_model(None)` which returns None — instead set `effective_model = Some(OPUS_MODEL.to_string())` directly.

2. **Escalation policy scope**: The escalation policy template is injected for ALL non-Opus tiers, **including Default/None**. The only tier that SKIPS escalation policy is Opus. Rationale: CLI default could be sonnet; escalation policy is informational and safe.

3. **IterationParams refactor**: Extract `run_iteration()`'s 17 positional parameters into an `IterationParams` struct as a prerequisite (FEAT-001) before FEAT-007 adds `default_model`.

4. **Model resolution via BuildPromptParams**: The `default_model` is threaded from `read_prd_metadata()` → `run_loop()` → `IterationParams.default_model` → `BuildPromptParams.default_model`. It is NOT queried inside `build_prompt()`.

5. **Synergy cluster uses synergyWith relationships**: NOT file overlap. Query: `SELECT t.model, t.difficulty FROM tasks t INNER JOIN task_relationships tr ON tr.related_id = t.id WHERE tr.task_id = ? AND tr.rel_type = 'synergyWith' AND t.status IN ('todo', 'in_progress')`.

---

## Key Model Constants

From `src/loop_engine/model.rs`:
```rust
pub const OPUS_MODEL: &str = "claude-opus-4-6";
pub const SONNET_MODEL: &str = "claude-sonnet-4-6";
pub const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
```

---

## Quality Checks (REQUIRED)

Run from project root:

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

## Error Handling Guidelines

- Never use `unwrap()` in production code
- Use `expect("descriptive message")` for programmer errors
- Use `?` operator with proper `Result` propagation
- Handle `Option::None` explicitly with meaningful defaults or errors

---

## Integration Verification Protocol (CRITICAL)

**New code must be fully wired in.** After implementing new code, verify:

1. **Export Chain Complete**: Module exported from parent, trace up to crate root
2. **Registration/Wiring**: New handlers/tools/routes registered where required
3. **Call Site Verification**: All places that SHOULD call the new code actually do
4. **Dead Code Detection**: `cargo check` / `cargo clippy` show no "unused" warnings for new code
5. **Trace Entry Point**: Can trace from `run_loop()` → `run_iteration()` → new code

### Common Wiring Points for This Feature

| New Code | Must Be Called From | How |
|----------|-------------------|-----|
| `spawn_claude(..., model)` | `engine.rs:run_iteration()` | Replace `None` with `effective_model.as_deref()` |
| `print_iteration_header(..., model)` | `engine.rs:run_iteration()` | Replace `None` with `effective_model.as_deref()` |
| `log_iteration(..., model)` | `engine.rs:run_loop()` | Replace `None` with `result.effective_model.as_deref()` |
| `BuildPromptParams.default_model` | `engine.rs:run_iteration()` | Thread from `IterationParams.default_model` |
| `PromptResult.resolved_model` | `engine.rs:run_iteration()` | Read after `build_prompt()`, apply crash escalation |
| `resolve_synergy_cluster_model()` | `prompt.rs:build_prompt()` | Called after task selection |
| `load_escalation_template()` | `prompt.rs:build_prompt()` | Called when resolved_model tier != Opus |

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found. The claude-loop reads the JSON at each iteration start, so newly added tasks will be picked up automatically.

### CODE-REVIEW-1 (Priority 13, adds tasks at 14-16)

**Purpose**: Catch quality, security, and **integration/wiring** issues.

**Wiring Issues Create WIRE-FIX Tasks** at priority 14-16.
**CRITICAL**: Add each CODE-FIX-xxx and WIRE-FIX-xxx to MILESTONE-1's `dependsOn` array.

### REFACTOR-REVIEW-1 (Priority 17, adds tasks at 18-19) — Before MILESTONE-1

Add REFACTOR-1-xxx tasks at priority 18-19. Add to MILESTONE-1 dependsOn.

### REFACTOR-REVIEW-2 (Priority 39, adds tasks at 40-44) — Before MILESTONE-2

Add REFACTOR-2-xxx tasks at priority 40-44. Add to MILESTONE-2 dependsOn.

### REFACTOR-REVIEW-3 (Priority 70, adds tasks at 71-85) — Before MILESTONE-FINAL

Add REFACTOR-3-xxx tasks at priority 71-85. Add to MILESTONE-FINAL dependsOn.

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

## Learnings Guidelines

**Read curated learnings first:**
- Before starting work, check `tasks/long-term-learnings.md` for project patterns
- These are curated, categorized learnings that persist across branches

**Write concise learnings** (1-2 lines each):
- GOOD: "`model_tier(Some(''))` returns Default, not an error"
- BAD: Lengthy paragraph explaining how model_tier works

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
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Add to JSON and commit: `chore: Add blocker task CLARIFY-001`
4. Output:
```
<promise>BLOCKED</promise>
```

---

## Milestones

Milestones (MILESTONE-xxx) are gate tasks:
1. Check all `dependsOn` tasks have `passes: true`
2. Run verification commands in acceptance criteria
3. Only mark `passes: true` when ALL criteria met

---

## Reference: Prompt Section Ordering

When implementing FEAT-004 (model resolution) and FEAT-005 (escalation template), follow this ordering:

1. Steering (from steering.md)
2. Session Guidance (from .pause interactions)
3. Reorder Hint (from previous iteration)
4. Source Context (from touchesFiles)
5. Completed Dependencies
6. Synergy Tasks
7. Current Task (JSON block — includes model/difficulty/escalationNote)
8. Relevant Learnings
9. Non-code task completion instruction (if applicable)
10. **Escalation Policy (NEW — skip only for Opus tier)**
11. Reorder instruction
12. Base Prompt (from prompt.md)

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md` section 8
