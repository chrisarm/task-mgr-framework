# /tasks - Convert PRD to Claude Loop Task List

Convert a markdown PRD into JSON task list and prompt file for task-mgr loop execution.

## Usage

```
/tasks .task-mgr/tasks/prd-{feature}.md
/tasks                          # Will prompt for PRD path
```

## Instructions

You are converting a human-readable PRD into machine-executable task artifacts for the Claude Loop autonomous agent system.

> **CRITICAL — These 4 principles must be embedded in every task and the prompt file:**
>
> 1. **Quality dimensions explicit** — every implementation task carries `qualityDimensions` (correctness, performance, style) from PRD section 2.5. The agent must know what "good" looks like, not just what to build.
> 2. **Edge cases = test cases** — every PRD Known Edge Case becomes an `edgeCases` entry on a TEST-INIT task. 1:1 mapping, no exceptions. Unnamed edge cases get discovered in production.
> 3. **Approach before code** — the agent considers 2-3 approaches, picks the best, then implements. This collapses re-implementation cycles.
> 4. **Self-critique after code** — the agent reviews its own implementation for correctness, style, and performance before moving on. This catches issues that would otherwise require a full re-do.

### Step 1: Read and Parse the PRD

Load the specified PRD file and extract:

- Feature title and type (feature/bug/enhancement/refactor)
- User stories with acceptance criteria
- Functional requirements
- Technical considerations (affected files)
- Non-goals (scope boundaries)

### Step 1.5: Resolve Current Model IDs

Do **not** hardcode model IDs — they change with each Claude release and must be read fresh each time you generate a task list.

#### Current model list: (as of 2026-02-23)

- **Opus** → value of `OPUS_MODEL` = `claude-opus-4-6`
- **Sonnet** → value of `SONNET_MODEL` = `claude-sonnet-4-6`
- **Haiku** → value of `HAIKU_MODEL` = `claude-haiku-4-5`

**Model assignment rubric** (set `model` field on tasks that need a specific tier; omit for tasks that should use the PRD-level default):

| Task type                                                    | Assign `model`             | Rationale                                        |
| ------------------------------------------------------------ | -------------------------- | ------------------------------------------------ |
| `ANALYSIS-xxx`                                               | opus                       | Deep semantic and consumer analysis              |
| `CODE-REVIEW-xxx`                                            | opus                       | Nuanced quality/security judgment                |
| `REFACTOR-REVIEW-xxx`                                        | opus                       | Architectural judgment calls                     |
| `MILESTONE-xxx`                                              | sonnet                     | Comprehensive verification, runs full test suite |
| `VERIFY-xxx`                                                 | opus                       | Final validation gate, thoroughness required     |
| `FEAT-xxx`, `FIX-xxx`, `TEST-xxx`, `INT-xxx`, `WIRE-FIX-xxx` | _(omit — use PRD default)_ | Standard implementation work                     |

**`timeoutSecs` assignment** (set on tasks that run the full test suite):

| Task type       | `timeoutSecs` | Rationale                                          |
| --------------- | ------------- | -------------------------------------------------- |
| `MILESTONE-xxx` | 1800          | Full `cargo test` + fixture suite can take 20+ min |
| `VERIFY-xxx`    | 1800          | Same — runs complete test suite                    |
| All others      | _(omit)_      | Uses loop default (12 min)                         |

Set the resolved opus model as the PRD-level `"model"` field:

```json
{
  "version": "1.0",
  "model": "<resolved-sonnet-id>",
  ...
}
```

This makes sonnet the iteration default, with opus tasks explicitly overriding per-task.

---

### Step 1.6: Extract Quality Dimensions and Edge Cases

From the PRD's **Section 2.5 (Quality Dimensions)**, extract:

- **Correctness requirements** → become `qualityDimensions.correctness` on each relevant implementation task
- **Performance requirements** → become `qualityDimensions.performance` on each relevant implementation task
- **Style requirements** → become `qualityDimensions.style` on each relevant implementation task
- **Known edge cases** → become `edgeCases` entries on TEST-INIT tasks

Every edge case in the PRD table MUST appear as an `edgeCases` entry on at least one TEST-INIT task. This ensures the implementing agent is forced to handle it rather than hoping to discover it independently.

---

### Step 2: Explore the Codebase

For each user story and requirement, use Glob and Grep to determine:

**touchesFiles**: Which files will be modified?

```bash
# Search for relevant modules
Glob: "**/*.rs" with pattern matching feature keywords
Grep: Search for existing related code
```

**dependsOn**: What's the implementation order?

- Schema/types first → Backend logic → API endpoints → UI
- Base functionality → Extensions
- Core → Tests

**synergyWith**: Which tasks share context?

- Tasks modifying the same file
- Tasks with related functionality

**conflictsWith**: Which tasks shouldn't run back-to-back?

- Tasks that might cause merge conflicts
- Tasks with incompatible intermediate states

### Step 3: Validate Story Sizing

For each user story, check complexity indicators:

**Warn if too large (suggest splitting):**

- More than 4 acceptance criteria that modify code
- Touches more than 4 files
- Description exceeds 150 words
- Spans multiple architectural layers

**Ideal story characteristics:**

- 1-3 acceptance criteria
- 1-2 files modified
- Can be completed in one iteration
- Clear "done" state

### Step 4: Generate Story IDs

Use context-appropriate prefixes:

- `ANALYSIS-xxx` - Consumer and semantic analysis (priority 0, blocks implementation)
- `FEAT-xxx` - New features
- `FIX-xxx` - Bug fixes
- `ENV-xxx` - Environment/configuration
- `PB-xxx` - PromptBuilder (domain-specific)
- `TEST-xxx` - Test implementation
- `INT-xxx` - Integration verification
- `WIRE-xxx` - Integration wiring tasks (spawned by CODE-REVIEW when code not fully integrated)
- `WIRE-FIX-xxx` - Fix wiring issues (exports, registration, call sites)
- `VERIFY-xxx` - Validation tasks
- `MILESTONE-xxx` - Gate checkpoints
- `CODE-REVIEW-xxx` - Code review tasks
- `REFACTOR-xxx` - Refactoring tasks
- `POLISH-xxx` - Documentation/cleanup

### Step 4.5: Identify Behavior-Modifying Tasks

For Bug Fixes, Enhancements, and Refactors, check if any task modifies existing behavior:

**A task modifies behavior if:**

- It changes the return value or side effects of an existing function
- It changes when/how data is cached, stored, or retrieved
- It changes control flow or routing logic
- It changes error handling or exception propagation

**For behavior-modifying tasks:**

1. **Check if PRD has Consumer Impact Table**: If the PRD already contains analysis from `/analyze`, use that data
2. **If no analysis exists**: Run `/analyze "{behavior being changed}"` now
3. Set `modifiesBehavior: true` in the task JSON
4. Create an `ANALYSIS-xxx` task with priority 0 that blocks the implementation task
5. Populate `consumerAnalysis` from the `/analyze` output

**AUTO-INVOKE**: If the PRD lacks a Consumer Impact Table for a behavior-modifying story, run:

```
/analyze "{function or behavior from the story}"
```

**ANALYSIS Task Requirements:**

- Priority: 0 (runs first)
- dependsOn: [] (no dependencies)
- Acceptance criteria: "Consumer Impact Table generated, all consumers identified, impact assessed"
- Description: Reference `/analyze` skill output

**If `/analyze` recommends SPLIT:**

- Do NOT create the original task
- Create separate tasks for each semantic context (e.g., FIX-001a, FIX-001b)
- Each split task should have its own `consumerAnalysis` scoped to its context

### Step 5: Create JSON Task File

Generate `.task-mgr/tasks/{feature}.json` following this schema:

```json
{
  "version": "1.0",
  "project": "{{PROJECT_NAME}}",
  "model": "<resolved-sonnet-id>",
  "branchName": "feat/{feature-name}",
  "externalGitRepo": "{{EXTERNAL_GIT_REPO_OR_OMIT}}",
  "mergeStrategy": "Merge to main after MILESTONE-FINAL passes. Squash commits optional.",
  "description": "{Feature description from PRD}",
  "priorityPhilosophy": {
    "description": "The hierarchy of what matters most when implementing tasks",
    "hierarchy": [
      "1. PLAN - Anticipate edge cases. Tests verify boundaries work correctly",
      "2. FUNCTIONING CODE - Pragmatic, reliable code that works according to the plan",
      "3. CORRECTNESS - Code compiles, type-checks, passes all tests deterministically",
      "4. CODE QUALITY - Clean code, good patterns, no warnings",
      "5. POLISH - Documentation, formatting, minor improvements"
    ],
    "principles": [
      "Ship solid working code first, then use tests to improve it",
      "Test the boundaries and exceptions - edge cases are where bugs hide",
      "Handle Option/Result explicitly; avoid unwrap() in production code"
    ]
  },
  "globalAcceptanceCriteria": {
    "description": "These criteria apply to ALL implementation tasks",
    "criteria": [
      "Rust: No warnings in `cargo check` output",
      "Rust: No warnings in `cargo clippy` output",
      "Rust: All tests pass with `cargo test`",
      "Rust: `cargo fmt --check` passes",
      "Python: `ruff check` passes",
      "Python: `mypy --strict` passes",
      "No breaking changes to existing APIs unless explicitly required"
    ]
  },
  "reviewGuidelines": {
    "priorityGuidelines": {
      "critical": "1-10: Blocks further work, fix immediately",
      "high": "11-20: Fix before phase completion",
      "medium": "21-50: Fix in current phase if time permits",
      "low": "51-99: Defer to hardening phase"
    }
  },
  "userStories": [
    {
      "id": "FEAT-001",
      "title": "Story title from PRD",
      "description": "What this story accomplishes",
      "acceptanceCriteria": [
        "Specific, testable criterion 1",
        "Specific, testable criterion 2"
      ],
      "priority": 1,
      "estimatedEffort": "low|medium|high",
      "passes": false,
      "notes": "Implementation hints, gotchas",
      "model": "<opus-id-if-review/milestone, omit otherwise>",
      "timeoutSecs": 1800,
      "touchesFiles": ["path/to/file.rs"],
      "dependsOn": [],
      "synergyWith": ["FEAT-002"],
      "batchWith": [],
      "conflictsWith": [],
      "modifiesBehavior": false,
      "qualityDimensions": {
        "correctness": ["What must not go wrong — from PRD section 2.5"],
        "performance": [
          "Efficiency requirements — exit early, avoid redundant work"
        ],
        "style": ["Idiomatic patterns required, anti-patterns to avoid"]
      },
      "consumerAnalysis": {
        "consumers": [
          {
            "file": "path/to/consumer.rs",
            "line": 123,
            "usage": "Routes on result.success == false",
            "impact": "BREAKS|OK|NEEDS_REVIEW",
            "mitigation": "Split into separate code paths"
          }
        ],
        "semanticDistinctions": [
          {
            "context": "LLM-invoked (user retry)",
            "currentBehavior": "Skip caching failures",
            "requiredBehavior": "Keep: skip caching failures"
          },
          {
            "context": "Auto-invoke (workflow routing)",
            "currentBehavior": "Cache all results",
            "requiredBehavior": "Keep: cache all results for routing"
          }
        ]
      },
      "edgeCases": ["(TEST-INIT only) Specific edge cases to test"],
      "invariants": ["(TEST-INIT only) Properties that must always hold"],
      "failureModes": [{ "cause": "...", "expectedBehavior": "..." }]
    }
  ]
}
```

### Step 6: Generate Prompt File

Create `.task-mgr/tasks/{feature}-prompt.md` using the template below, replacing placeholders:

- `{{PROJECT_NAME}}` - Determine from (in order of priority):
  1. `.task-mgr/tasks/project-config.json` field `"project"`
  2. `package.json` field `"name"`
  3. `Cargo.toml` field `name` in `[package]`
  4. Current directory name
- `{{EXTERNAL_GIT_REPO_OR_OMIT}}` - **REQUIRED if code lives in a different git repo than task-mgr.** Set to relative path (e.g. `"../restaurant_agent_ex"`). Without this, the loop cannot detect task completion from commits in the external repo and tasks get stuck as `in_progress` forever. Omit the field entirely if the code and task-mgr are in the same repo.
- `{{FEATURE_TITLE}}` - Feature name from PRD
- `{{FEATURE_NAME}}` - Kebab-case filename (e.g., `date-context`)
- `{{PROBLEM_STATEMENT}}` - Problem description from PRD
- `{{REFERENCE_CODE}}` - Optional: code patterns identified during exploration
- `{{FEATURE_SPECIFIC_CHECKS}}` - Optional: additional quality checks

<details>
<summary><strong>Prompt Template</strong> (click to expand)</summary>

````markdown
# Claude Code Agent Instructions

You are an autonomous coding agent implementing **{{FEATURE_TITLE}}** for **{{PROJECT_NAME}}**.

## Problem Statement

{{PROBLEM_STATEMENT}}

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

| File                               | Purpose                                                          |
| ---------------------------------- | ---------------------------------------------------------------- |
| `.task-mgr/tasks/{{FEATURE_NAME}}.json`      | **Task list (PRD)** - Read tasks, mark complete, add new tasks   |
| `.task-mgr/tasks/{{FEATURE_NAME}}-prompt.md` | This prompt file (read-only)                                     |
| `.task-mgr/tasks/progress.txt`               | Progress log - append findings and learnings                     |
| `.task-mgr/tasks/long-term-learnings.md`     | Curated learnings by category (read first)                       |
| `.task-mgr/tasks/learnings.md`               | Raw iteration learnings (auto-appended, needs periodic curation) |

When review tasks add new tasks, they modify `.task-mgr/tasks/{{FEATURE_NAME}}.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `.task-mgr/tasks/{{FEATURE_NAME}}.json`
2. Read the progress log at `.task-mgr/tasks/progress.txt` (if exists)
3. Read `.task-mgr/tasks/long-term-learnings.md` for curated project patterns (persists across branches)
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
13. Append progress to `.task-mgr/tasks/progress.txt` (include approach chosen and any edge cases discovered)
14. For TEST-xxx tasks: ensure 80%+ coverage for new methods; use `assert_eq!` for string outputs

---

## Smart Task Selection

Tasks have relationship fields:

```json
{
  "touchesFiles": ["src/module/file.rs"],
  "dependsOn": ["FEAT-001"], // HARD: Must complete first
  "synergyWith": ["FEAT-002"], // SOFT: Share context
  "batchWith": [], // DIRECTIVE: Do together
  "conflictsWith": [] // AVOID: Don't sequence
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

## Behavior Modification Protocol

Before implementing any task with `modifiesBehavior: true`:

### 1. Verify ANALYSIS Task Status

Check if an `ANALYSIS-xxx` task exists for this change:

- If ANALYSIS exists and `passes: true` → proceed to step 2
- If ANALYSIS exists and `passes: false` → work on ANALYSIS first
- If no ANALYSIS exists → create one and work on it first

### 2. Check Consumer Impact Table

Read `.task-mgr/tasks/progress.txt` and find the Consumer Impact Table from the ANALYSIS task:

- If any consumer has `Impact: BREAKS` → the task must be SPLIT
- If any consumer has `Impact: NEEDS_REVIEW` → verify before implementing
- If all consumers have `Impact: OK` → proceed with implementation

### 3. Verify Semantic Distinctions

If the ANALYSIS identified multiple semantic contexts (same code, different purposes):

- Each context may need different handling
- A single change may need to become multiple targeted changes
- Example: "LLM-invoked" vs "auto-invoke" tool calls have different caching requirements

**If you discover the task should be split:**

1. Do NOT implement the current task
2. Create new split tasks (e.g., FIX-002a, FIX-002b) with specific contexts
3. Update dependencies so original task is replaced by split tasks
4. Commit the JSON changes: `chore: Split [Task ID] for semantic contexts`
5. Mark original task with `passes: true` and note "Split into [new IDs]"

---

## Consumer Analysis Protocol

Before modifying shared code (called from multiple places):

### 1. Identify All Callers

```bash
# Search for direct callers
Grep: function_name
# Search for indirect references (configs, YAML routing)
Grep: "function_name\\|related_config_key"
# Search for tests asserting behavior
Grep: "test.*function_name\\|assert.*expected_value"
```

### 2. Create Consumer Impact Table

Document in progress.txt:

```markdown
## Consumer Impact Table for [Task ID]

| File:Line                | Usage                             | Current Behavior        | Impact | Mitigation                        |
| ------------------------ | --------------------------------- | ----------------------- | ------ | --------------------------------- |
| workflow/executor.rs:456 | Calls function for auto-invoke    | Caches all results      | OK     | No change needed                  |
| chat/handler.rs:123      | Calls function for user retry     | Skips caching failures  | OK     | No change needed                  |
| pto_cancel.yml:246       | Routes on result.success == false | Expects cached failures | BREAKS | Must keep caching for auto-invoke |
```

### 3. Decision Matrix

Based on Consumer Impact Table:

- **All OK**: Proceed with single implementation
- **Any BREAKS**: Split task by context, implement each separately
- **NEEDS_REVIEW**: Verify with tests before/after, document assumptions

---

## Quality Checks (REQUIRED)

Run from project root (or the appropriate subdirectory if monorepo).

### Rust Projects

```bash
# 1. Format check
cargo fmt --check

# 2. Type check
cargo check

# 3. Linting
cargo clippy -- -D warnings

# 4. Tests
cargo test

# 5. Security audit (if available)
cargo audit 2>/dev/null || true
```

### Python Projects

```bash
# 1. Format and lint (if using ruff)
ruff check --fix && ruff format

# 2. Type check
mypy --strict

# 3. Tests (adjust for your test runner)
pytest
# or: uv run pytest
# or: python -m pytest
```

### Other Languages

Adapt quality checks to match project tooling (check CLAUDE.md or README for project-specific commands).

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

**New code must be fully wired in.** A common failure mode is code that compiles and passes unit tests but is never called in production because it's not properly integrated.

### After Implementing New Code, Verify:

#### 1. Export Chain Complete

```bash
# Verify module is exported from parent
Grep: "pub mod {new_module}" or "pub use {new_module}"
# Trace up to crate root - every level must re-export
```

#### 2. Registration/Wiring Points

Check that new code is registered where required:

- **Routes/Handlers**: Added to router/dispatcher?
- **Tools**: Registered in tool registry?
- **Config fields**: Read and passed through?
- **DI/Services**: Registered in service container?
- **Feature flags**: Enabled for the appropriate environments?

#### 3. Call Site Verification

```bash
# Find ALL places that SHOULD call the new code
Grep: "{old_function_name}" # If replacing
Grep: "{related_pattern}"   # If adding to existing flow

# Verify new code IS called from those places
Grep: "{new_function_name}"
```

#### 4. Dead Code Detection

```bash
# Check for unused imports/functions
cargo check 2>&1 | grep -i "unused"
cargo clippy 2>&1 | grep -i "never used"
```

#### 5. Trace Entry Point to New Code

**For each production entry point**, trace whether new code is reachable:

```
Entry Point (API/CLI/Event)
    ↓
Router/Dispatcher
    ↓
Handler/Controller
    ↓
Service Layer
    ↓
>>> NEW CODE <<<  ← Is this reachable?
```

If you cannot trace a path from entry point to new code, the code is **not wired in**.

### Integration Verification Checklist

Before marking any implementation task complete:

- [ ] **Exports**: New module/function exported from parent mod.rs?
- [ ] **Imports**: Consuming modules import the new code?
- [ ] **Registration**: New handler/tool/route registered?
- [ ] **Config**: New config fields wired through from config source to usage?
- [ ] **Call sites**: All places that should use new code actually call it?
- [ ] **Old code removed**: If replacing, old implementation removed/deprecated?
- [ ] **No dead code warnings**: `cargo check` shows no unused warnings for new code?
- [ ] **Traceable path**: Can trace from entry point to new code?

### Common Wiring Failures

| Symptom                                        | Cause                                   | Fix                    |
| ---------------------------------------------- | --------------------------------------- | ---------------------- |
| Code compiles but feature doesn't work         | Not registered in dispatcher/router     | Add to registration    |
| Tests pass but production doesn't use new code | Test mocks bypass real wiring           | Verify production path |
| New config field has no effect                 | Config read but not passed to component | Wire config through    |
| Old behavior persists                          | Conditional still routes to old code    | Update routing logic   |
| "unused import" warning                        | Imported but never called               | Wire call sites        |

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks are special: they **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found. The task-mgr reads the JSON at each iteration start, so newly added tasks will be picked up automatically.

**Key principle**: Every milestone must be preceded by a refactor review to ensure code quality improves incrementally.

### CODE-REVIEW-1 (Priority 13, adds tasks at 14-16)

**Purpose**: Catch quality, security, and **integration/wiring** issues before testing phase.

**Execution**:

1. Analyze code against language idioms (Rust: borrow checker, ownership, lifetimes)
2. Check for: security issues, memory safety, error handling, unwrap() usage
3. **Verify quality dimensions were met**: For each task's `qualityDimensions`, confirm the implementation satisfies correctness, performance, and style constraints. For each `edgeCases` entry on TEST-INIT tasks, confirm it has a corresponding test
4. **CRITICAL - Verify Integration Wiring**:
   - [ ] All new code is exported and importable
   - [ ] All new handlers/tools/routes are registered
   - [ ] All new config fields are wired through
   - [ ] All call sites that should use new code actually do
   - [ ] No dead code warnings (`cargo check` / `cargo clippy`)
   - [ ] Can trace path from entry point to new code
5. Use the **rust-code-reviewer** agent when available
6. Document findings in `progress.txt`

**Wiring Issues Create WIRE-FIX Tasks**:
If new code is not properly integrated, create `WIRE-FIX-xxx` tasks:

```json
{
  "id": "WIRE-FIX-001",
  "title": "Wire: [component] not registered/exported/called",
  "description": "New code at [path] is not reachable from production. Need to: [specific wiring step]",
  "acceptanceCriteria": [
    "Code is reachable from entry point",
    "No unused warnings for new code",
    "Integration test exercises the wired path"
  ],
  "priority": 14,
  "passes": false,
  "dependsOn": [],
  "touchesFiles": ["affected/file.rs", "registration/file.rs"]
}
```

**Adding Tasks**:

- For EACH issue found, add a `CODE-FIX-xxx` or `WIRE-FIX-xxx` task to the JSON (priority 14-16)
- Task structure:
  ```json
  {
    "id": "CODE-FIX-001",
    "title": "Fix: [specific issue]",
    "description": "Address finding from CODE-REVIEW-1: [details]",
    "acceptanceCriteria": ["Issue resolved", "No new warnings"],
    "priority": 14,
    "passes": false,
    "dependsOn": [],
    "touchesFiles": ["affected/file.rs"]
  }
  ```
- **CRITICAL**: Add each CODE-FIX-xxx and WIRE-FIX-xxx to MILESTONE-1's `dependsOn` array
- Commit JSON changes: `chore: CODE-REVIEW-1 - Add CODE-FIX/WIRE-FIX tasks`
- Commit and output `<completed>CODE-REVIEW-1</completed>` once review complete AND all tasks added

**If no issues found**: Output `<completed>CODE-REVIEW-1</completed>` with note "No issues found"

### REFACTOR-REVIEW-1 (Priority 17, adds tasks at 18-19) - Before MILESTONE-1

**Purpose**: Ensure implementation code is maintainable before testing begins.

**Look for**:

- **Duplication**: Same logic repeated (DRY violations)
- **Complexity**: Functions >30 lines
- **Coupling**: Modules that know too much about each other
- **Rigidity**: Code hard to change or extend

**Adding Tasks**:

- For EACH issue found, add a `REFACTOR-1-xxx` task to the JSON (priority 18-19)
- Task structure:
  ```json
  {
    "id": "REFACTOR-1-001",
    "title": "Refactor: [specific improvement]",
    "description": "Address finding from REFACTOR-REVIEW-1: [details]",
    "acceptanceCriteria": ["Improvement implemented", "Tests still pass"],
    "priority": 18,
    "passes": false,
    "dependsOn": [],
    "touchesFiles": ["affected/file.rs"]
  }
  ```
- **CRITICAL**: Add each REFACTOR-1-xxx to MILESTONE-1's `dependsOn` array
- Commit JSON changes: `chore: REFACTOR-REVIEW-1 - Add refactor tasks`
- Commit and output `<completed>REFACTOR-REVIEW-1</completed>` once review complete AND all tasks added

**If no issues found**: Output `<completed>REFACTOR-REVIEW-1</completed>` with note "No refactoring needed"

### REFACTOR-REVIEW-2 (Priority 39, adds tasks at 40-44) - Before MILESTONE-2

**Purpose**: Ensure test code is maintainable before integration testing.

**Look for**:

- **Test duplication**: Same setup code repeated
- **Missing helpers**: Common patterns that should be extracted
- **Unclear tests**: Test names/structure that don't clearly describe what's tested
- **Brittle tests**: Tests that depend on implementation details

**Adding Tasks**:

- For EACH issue found, add a `REFACTOR-2-xxx` task to the JSON (priority 40-44)
- **CRITICAL**: Add each REFACTOR-2-xxx to MILESTONE-2's `dependsOn` array
- Commit JSON changes: `chore: REFACTOR-REVIEW-2 - Add refactor tasks`
- Commit and output `<completed>REFACTOR-REVIEW-2</completed>` once review complete AND all tasks added

**If no issues found**: Output `<completed>REFACTOR-REVIEW-2</completed>` with note "No refactoring needed"

### REFACTOR-REVIEW-3 (Priority 65, adds tasks at 66-80) - Before MILESTONE-FINAL

**Purpose**: Final opportunity to improve code before merge.

**Look for** (comprehensive review):

- **All DRY violations** across implementation and tests
- **Complexity hotspots** that should be simplified
- **Coupling issues** between modules
- **Code clarity** - anything hard to understand
- **Pattern adherence** - see CLAUDE.md section 8

**Adding Tasks**:

- For EACH issue found, add a `REFACTOR-3-xxx` task to the JSON (priority 66-80)
- **CRITICAL**: Add each REFACTOR-3-xxx to MILESTONE-FINAL's `dependsOn` array
- Commit JSON changes: `chore: REFACTOR-REVIEW-3 - Add refactor tasks`
- Commit and output `<completed>REFACTOR-REVIEW-3</completed>` once review complete AND all tasks added

**If no issues found**: Output `<completed>REFACTOR-REVIEW-3</completed>` with note "No refactoring needed"

### TDD Test Strategy

| Phase                     | Task Type     | Focus                                              | Examples                                                                                                    |
| ------------------------- | ------------- | -------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| **Initial** (1-5)         | TEST-INIT-xxx | Happy path + edge cases + known-bad discriminators | Happy path, boundary values, empty/null, invalid input, invariant checks, one test that rejects naive stubs |
| **Comprehensive** (25-38) | TEST-xxx      | Exceptions, errors, parameterized, boundaries      | Error handling, invalid inputs, parameterized, race conditions                                              |

**TEST-INIT-xxx**: Write BEFORE implementation. Must include `edgeCases`, `invariants`, and `failureModes` fields. At least one test must discriminate correct from plausible-but-wrong implementations.

**TEST-xxx**: Write AFTER MILESTONE-1. May spawn IMPL-FIX-xxx tasks if implementation gaps are found.

### Task Flow Diagram (TDD)

```
Initial Tests (1-5) ──► Implementation (6-12) ──► CODE-REVIEW-1 (13) ──► REFACTOR-REVIEW-1 (17) ──► MILESTONE-1 (20)
       │                      │                        │                        │
       │                      │                        └─ CODE-FIX-xxx (14-16) ─┘
       │                      │                                                  └─ REFACTOR-1-xxx (18-19) ─┘
       │                      └─ "Make initial tests pass"
       └─ TEST-INIT-xxx: Edge cases + invariants + known-bad discriminators

Comprehensive Tests (25-38) ──► IMPL-FIX-xxx (39-42) ──► REFACTOR-REVIEW-2 (43) ──► MILESTONE-2 (50)
              │                        │                        │
              │                        │                        └─ REFACTOR-2-xxx (44-48) ─┘
              │                        └─ "Fix issues revealed by tests"
              └─ TEST-xxx: Exceptions, errors, parameterized tests

Integration (55-65) ──► REFACTOR-REVIEW-3 (70) ──► VERIFY (90) ──► MILESTONE-FINAL (99)
                              │
                              └─ REFACTOR-3-xxx (71-85) ─┘
```

### Review Task Commits

When a review adds new tasks to `.task-mgr/tasks/{{FEATURE_NAME}}.json`:

```bash
# 1. Edit the JSON file to add new tasks and update milestone dependsOn
# 2. Commit the JSON changes
git add .task-mgr/tasks/{{FEATURE_NAME}}.json
git commit -m "chore: [Review ID] - Add refactor tasks"

# 3. Mark review as passes: true in the same JSON file
# 4. Commit the completion
git add .task-mgr/tasks/{{FEATURE_NAME}}.json
git commit -m "feat: [Review ID] - Review complete"
```

The loop will automatically pick up the new tasks on the next iteration since it re-reads `.task-mgr/tasks/{{FEATURE_NAME}}.json` at the start of each iteration.

---

## Progress Report Format

APPEND to `.task-mgr/tasks/progress.txt`:

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

- Before starting work, check `.task-mgr/tasks/long-term-learnings.md` for project patterns
- These are curated, categorized learnings that persist across branches
- Raw iteration learnings in `.task-mgr/tasks/learnings.md` are auto-appended and need periodic curation

**Write concise learnings** (1-2 lines each):

- GOOD: "`temps::chrono::Timezone` accessed via full path, not temps_core"
- BAD: "The temps crate exports Timezone from temps::chrono module, so when using it you need to access it via the full path temps::chrono::Timezone rather than importing from temps_core which doesn't re-export it."

**Group related tasks** when reporting:

- Instead of separate entries for FIX-001, FIX-002, FIX-003
- Write: "FIX-001 through FIX-003: Fixed X by doing Y"

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

1. Document blocker in `progress.txt`
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
4. Milestones ensure code review and refactor review happen before proceeding

---

{{#if REFERENCE_CODE}}

## Reference Code

{{REFERENCE_CODE}}

---

{{/if}}

{{#if FEATURE_SPECIFIC_CHECKS}}

## Feature-Specific Checks

{{FEATURE_SPECIFIC_CHECKS}}

---

{{/if}}

## Important Rules

- Work on **ONE story per iteration**
- **For high-effort tasks** (`estimatedEffort: "high"` or 10+ acceptance criteria): consider using `/ralph-loop` to iterate within the task until all acceptance criteria pass, e.g.:
  `/ralph-loop "Implement [TASK-ID]: [title]. Criteria: [list]. Output <promise>DONE</promise> when all pass." --max-iterations 10`
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md` section 8
````

</details>

### Step 7: Include Required Task Types

Every task list should include:

0. **Analysis Tasks** (priority 0) - _Only for behavior-modifying changes_

   - `ANALYSIS-001: Consumer and semantic analysis`
   - Identifies all consumers of changed behavior
   - Documents semantic distinctions (same code, different contexts)
   - Blocks implementation tasks until analysis passes
   - Outputs Consumer Impact Table in progress.txt

1. **Initial Test Tasks** (priority 1-5) - **TESTS FIRST (TDD)**

   - `TEST-INIT-xxx: Initial tests for [feature]`
   - Write tests BEFORE implementation to define expected behavior
   - Cover: happy path + edge cases + at least one "known-bad" discriminator
   - PRD Known Edge Cases from Section 2.5 must flow into `edgeCases` field on TEST-INIT tasks
   - Tests should initially FAIL (no implementation yet) or use `#[ignore]` with clear TODO
   - Acceptance criteria focus on test design, not passing
   - **Required fields** (task generator must populate):
     - `edgeCases`: 3+ specific edge cases (boundary values, empty/null, invalid input)
     - `invariants`: 2-5 properties that must always hold true
     - `failureModes`: 1+ failure scenarios with expected behavior
   - **Known-bad requirement**: At least one test must distinguish correct behavior from a plausible-but-wrong implementation
   - Example: "Test that empty query returns [] not nil", "Test that expired specials are excluded not just filtered client-side"

2. **Implementation Tasks** (priority 6-12)

   - Core functionality from user stories
   - Ordered by dependency
   - Goal: Make the initial tests pass
   - Acceptance criteria: "All TEST-INIT-xxx tests pass"
   - **Integration wiring tasks** must include:
     - A known-bad discriminator: a test that asserts the NEW behavior is observable from the production entry point. It MUST fail before wiring and MUST pass after — proving the feature is live, not just implemented.
     - Example: "Test that a request through the main API hits the new `stream_json=true` path — fails until wiring is complete"

3. **Code Review Task** (priority 13) - **CAN SPAWN TASKS**

   - `CODE-REVIEW-1: Review implementation for quality, security, and INTEGRATION WIRING`
   - Review implementation for quality/security issues
   - **CRITICAL: Verify all new code is fully integrated/wired in**:
     - Exports complete (module exported from parent)?
     - Registration done (handlers/tools/routes registered)?
     - Config wired through?
     - Call sites updated (code actually called from production paths)?
     - No dead code warnings?
   - **MUST spawn CODE-FIX-xxx or WIRE-FIX-xxx tasks (priority 14-16) for any issues found**
   - Spawned tasks must have `dependsOn: []` (no deps, ready to run)
   - Add each CODE-FIX-xxx and WIRE-FIX-xxx to MILESTONE-1's `dependsOn` array
   - Mark CODE-REVIEW-1 as `passes: true` once review complete and tasks created
   - Acceptance criteria: "Any issues found have corresponding CODE-FIX-xxx or WIRE-FIX-xxx tasks created"

4. **Refactor Review Task** (priority 17) - **CAN SPAWN TASKS** - _Before MILESTONE-1_

   - `REFACTOR-REVIEW-1: Review implementation for refactoring opportunities`
   - Look for: code duplication (DRY), functions >30 lines, tight coupling, hard-to-change code
   - **MUST spawn REFACTOR-1-xxx tasks (priority 18-19) for any issues found**
   - Spawned tasks must have `dependsOn: []` (no deps, ready to run)
   - Add each REFACTOR-1-xxx to MILESTONE-1's `dependsOn` array
   - Mark REFACTOR-REVIEW-1 as `passes: true` once review complete and tasks created
   - Acceptance criteria: "Any issues found have corresponding REFACTOR-1-xxx tasks created"

5. **MILESTONE-1** (priority 20)

   - Set `"model": "<opus-id>"` and `"timeoutSecs": 1800`
   - Gate before comprehensive testing phase
   - Depends on: CODE-REVIEW-1 + REFACTOR-REVIEW-1 + all TEST-INIT-xxx + all FEAT-xxx + all FIX-xxx + all WIRE-FIX-xxx
   - Acceptance criteria must include:
     - "All initial tests (TEST-INIT-xxx) pass"
     - "CODE-REVIEW-1 passes (and any spawned CODE-FIX-xxx or WIRE-FIX-xxx tasks)"
     - "REFACTOR-REVIEW-1 passes (and any spawned REFACTOR-1-xxx tasks)"
     - **"All new code is reachable from production entry points (no dead code)"**
     - **"No unused warnings for new code in `cargo check`/`cargo clippy`"**
     - **"Integration test verifying the feature's observable behavior change exists and passes"**
     - **"The feature's primary user story is exercised end-to-end (not just unit tested)"**
   - If the feature has a critical integration boundary, add `"requiredTests": ["test_filter_name"]` to enforce test-gated completion (see `requiredTests` field docs below)
   - Notes: "Initial tests pass, implementation reviewed, refactored, and **fully wired in**. If no integration test exists for the core behavior change, create one before marking complete."

6. **Comprehensive Test Tasks** (priority 25-38)

   - `TEST-xxx: Comprehensive tests for [feature]`
   - Expand test coverage beyond initial tests
   - Cover: exceptions, error handling, parameterized tests (`#[rstest]`), boundary values, race conditions
   - Focus on robustness: invalid inputs, malformed data, timeouts, failure modes
   - May reveal implementation gaps → creates IMPL-FIX-xxx tasks

7. **Test-Driven Fix Tasks** (priority 39-42) - **CAN BE SPAWNED BY TEST-xxx**

   - `IMPL-FIX-xxx: Fix implementation for [failing test scenario]`
   - Created when comprehensive tests reveal implementation gaps
   - Must reference the specific test that revealed the issue

8. **Refactor Review Task** (priority 43) - **CAN SPAWN TASKS** - _Before MILESTONE-2_

   - `REFACTOR-REVIEW-2: Review test code for refactoring opportunities`
   - Look for: test code duplication, helper functions to extract, test clarity improvements
   - **MUST spawn REFACTOR-2-xxx tasks (priority 44-48) for any issues found**
   - Spawned tasks must have `dependsOn: []` (no deps, ready to run)
   - Add each REFACTOR-2-xxx to MILESTONE-2's `dependsOn` array
   - Mark REFACTOR-REVIEW-2 as `passes: true` once review complete and tasks created
   - Acceptance criteria: "Any issues found have corresponding REFACTOR-2-xxx tasks created"

9. **MILESTONE-2** (priority 50)

   - Set `"model": "<opus-id>"` and `"timeoutSecs": 1800`
   - Gate before integration/verification
   - Depends on: MILESTONE-1 + REFACTOR-REVIEW-2 + all TEST-xxx + all IMPL-FIX-xxx
   - Acceptance criteria must include:
     - "All comprehensive tests (TEST-xxx) pass"
     - "REFACTOR-REVIEW-2 passes (and any spawned REFACTOR-2-xxx tasks)"
   - Notes: "Full test coverage achieved, test code refactored"

10. **Integration/Verification Tasks** (priority 55-65)

    - Full test suite
    - E2E tests
    - Build verification

11. **Refactor Review Task** (priority 70) - **CAN SPAWN TASKS** - _Before MILESTONE-FINAL_

    - `REFACTOR-REVIEW-3: Final refactoring review for maintainability`
    - Comprehensive review: DRY violations, complexity hotspots, coupling issues, code clarity
    - **MUST spawn REFACTOR-3-xxx tasks (priority 71-85) for any issues found**
    - Spawned tasks must have `dependsOn: []` (no deps, ready to run)
    - Add each REFACTOR-3-xxx to MILESTONE-FINAL's `dependsOn` array
    - Mark REFACTOR-REVIEW-3 as `passes: true` once review complete and tasks created
    - Acceptance criteria: "Any issues found have corresponding REFACTOR-3-xxx tasks created"

12. **Final Verification** (priority 90-95)

    - `VERIFY-001: Final verification - all checks pass`
    - Set `"model": "<opus-id>"` and `"timeoutSecs": 1800`
    - Depends on: INT-xxx + REFACTOR-REVIEW-3

13. **MILESTONE-FINAL** (priority 99)
    - Set `"model": "<opus-id>"` and `"timeoutSecs": 1800`
    - Gate before merge
    - Depends on: VERIFY-001 + REFACTOR-REVIEW-3
    - Acceptance criteria must include: "REFACTOR-REVIEW-3 passes (and any spawned REFACTOR-3-xxx tasks)"
    - All acceptance criteria met
    - Ready for merge
    - If the feature has a critical integration path, add `"requiredTests": ["test_that_verifies_feature_purpose"]` — a test that verifies the feature's _purpose_ (the observable behavior change, not just compilation)

### Known-Bad Discriminators

A **known-bad discriminator** is a test that acts as a litmus test for whether a new feature is actually wired in to the production code path. It follows the TDD red→green cycle:

| State | Test Result | What It Proves |
|---|---|---|
| Before wiring | FAIL | The old behavior is still active — feature isn't live yet |
| After wiring | PASS | The new behavior is active — feature is correctly integrated |

The key property: it **discriminates** between "code exists but isn't called" and "code exists AND is called."

**What makes a good one:**

1. **Assert on observable new behavior**, not just that code exists
2. **Be impossible to pass with the old code path** — if someone forgets to wire it in, the test *must* fail
3. **Be as minimal as possible** — it tests wiring, not full feature correctness

**Examples:**

- "Send a request through the production API entry point and assert that the response includes `stream_json=true` in the engine call. Fails until the engine is wired to use the new streaming path."
- "Call the login endpoint with an OAuth token and assert the request reaches the new `OAuthProvider`, not the legacy `PasswordProvider`. Fails until the router is updated."
- "Send a request and assert that the response headers include `X-RateLimit-Remaining`. Fails until the rate-limiting middleware is added to the pipeline."

**Why it matters:** Without this, all unit tests for a new feature can pass (the feature works in isolation) but the feature is never actually called because nobody updated the router/factory/pipeline. The known-bad discriminator closes that gap.

### `requiredTests` Field

Hard gate for task completion. When present, `task-mgr complete` runs these tests and refuses completion if any fail.

- **Purpose**: Prevents marking a task as done when the feature's core behavior isn't actually working
- **When to use**: Milestones where the feature must be observably wired in. Not for every task — only critical integration paths.
- **Format**: `"requiredTests": ["test_name_filter"]` — each entry is a `cargo test` filter string
- **Behavior**: `force=true` bypasses the gate (consistent with dependency gate)
- **Guidance**: `requiredTests` should be rare — only for critical integration paths where the feature's core value proposition can be verified by a named test

### Step 7.1: Task Templates

When creating tasks, use these structures.

**IMPORTANT**: When a review task identifies issues, it must **add new tasks directly to the JSON file** so the task-mgr loop picks them up in subsequent iterations. The loop reads the JSON at each iteration start, so newly added tasks will be selected automatically.

#### TEST-INIT-xxx Template (priority 1-5) - **TESTS FIRST**

```json
{
  "id": "TEST-INIT-001",
  "title": "Initial tests for [feature/component]",
  "description": "Write tests BEFORE implementation. Must cover happy path, edge cases, and include at least one known-bad discriminator test.",
  "acceptanceCriteria": [
    "Happy path test defined: [specific scenario]",
    "Edge case tests cover: [from edgeCases field]",
    "Known-bad discriminator: at least one test that would PASS with a naive stub but FAIL with correct implementation",
    "Invariant assertions: [from invariants field]",
    "Test file compiles (tests may be #[ignore] or expected to fail)"
  ],
  "priority": 1,
  "estimatedEffort": "low",
  "passes": false,
  "notes": "TDD: Write these tests first. Tests must be specific enough to reject wrong implementations, not just verify no crash. Implementation tasks depend on these.",
  "edgeCases": [
    "Empty/null input: [expected behavior]",
    "Boundary value: [expected behavior]",
    "Invalid/malformed input: [expected behavior]"
  ],
  "invariants": [
    "[Property that must always hold]",
    "[Another property that must always hold]"
  ],
  "failureModes": [
    {
      "cause": "[What goes wrong]",
      "expectedBehavior": "[How system should respond]"
    }
  ],
  "touchesFiles": ["path/to/tests"],
  "dependsOn": [],
  "synergyWith": [],
  "batchWith": [],
  "conflictsWith": [],
  "modifiesBehavior": false
}
```

#### IMPL-FIX-xxx Template (priority 39-42) - **Spawned by TEST-xxx**

```json
{
  "id": "IMPL-FIX-001",
  "title": "Fix: [issue revealed by test]",
  "description": "Address implementation gap revealed by TEST-xxx: [specific failing test]",
  "acceptanceCriteria": [
    "Failing test [test_name] now passes",
    "No regression in other tests"
  ],
  "priority": 39,
  "estimatedEffort": "low",
  "passes": false,
  "notes": "Created by TEST-xxx when comprehensive tests revealed implementation gap. Reference: [test file:line]",
  "touchesFiles": ["path/to/implementation.rs"],
  "dependsOn": [],
  "synergyWith": [],
  "batchWith": [],
  "conflictsWith": [],
  "modifiesBehavior": false
}
```

#### CODE-REVIEW-1 Template (priority 13)

```json
{
  "id": "CODE-REVIEW-1",
  "title": "Review implementation for quality and security",
  "description": "Review all implementation tasks for code quality, error handling, and security",
  "acceptanceCriteria": [
    "No unwrap() in production code paths",
    "All errors properly propagated with context",
    "Security: no injection vulnerabilities",
    "Quality dimensions met: each task's qualityDimensions constraints satisfied",
    "Edge case coverage: each edgeCases entry has a corresponding test",
    "Any issues found have corresponding CODE-FIX-xxx tasks added to JSON"
  ],
  "priority": 13,
  "estimatedEffort": "medium",
  "model": "<opus-id>",
  "passes": false,
  "notes": "Use rust-code-reviewer agent if available. For each issue found: 1) Add a CODE-FIX-xxx task to the JSON (priority 14-16), 2) Add CODE-FIX-xxx to MILESTONE-1 dependsOn array, 3) Commit the JSON changes. Mark CODE-REVIEW-1 as passes:true once review is complete and all tasks are added.",
  "touchesFiles": ["<list all implementation files>"],
  "dependsOn": ["<all FEAT-xxx and FIX-xxx tasks>"],
  "synergyWith": [],
  "batchWith": [],
  "conflictsWith": [],
  "modifiesBehavior": false
}
```

#### REFACTOR-REVIEW-1 Template (priority 17, before MILESTONE-1)

```json
{
  "id": "REFACTOR-REVIEW-1",
  "title": "Review implementation for refactoring opportunities",
  "description": "Analyze implementation code for DRY violations, complexity, coupling, and maintainability issues",
  "acceptanceCriteria": [
    "No code duplication (DRY principle)",
    "Functions under 30 lines (flag complex ones)",
    "No tight coupling between modules",
    "Code is easy to change and extend",
    "Any issues found have corresponding REFACTOR-1-xxx tasks added to JSON"
  ],
  "priority": 17,
  "estimatedEffort": "medium",
  "model": "<opus-id>",
  "passes": false,
  "notes": "For each issue found: 1) Add a REFACTOR-1-xxx task to the JSON (priority 18-19), 2) Add REFACTOR-1-xxx to MILESTONE-1 dependsOn array, 3) Commit the JSON changes. If no issues found, mark passes:true with note 'No refactoring needed'. Loop will pick up new tasks automatically.",
  "touchesFiles": ["<list all implementation files>"],
  "dependsOn": ["CODE-REVIEW-1"],
  "synergyWith": [],
  "batchWith": [],
  "conflictsWith": [],
  "modifiesBehavior": false
}
```

#### REFACTOR-REVIEW-2 Template (priority 39, before MILESTONE-2)

```json
{
  "id": "REFACTOR-REVIEW-2",
  "title": "Review test code for refactoring opportunities",
  "description": "Analyze test code for duplication, helper extraction opportunities, and clarity improvements",
  "acceptanceCriteria": [
    "No duplicated test setup code",
    "Helper functions extracted for common patterns",
    "Test names clearly describe what is tested",
    "Any issues found have corresponding REFACTOR-2-xxx tasks added to JSON"
  ],
  "priority": 39,
  "estimatedEffort": "medium",
  "model": "<opus-id>",
  "passes": false,
  "notes": "For each issue found: 1) Add a REFACTOR-2-xxx task to the JSON (priority 40-44), 2) Add REFACTOR-2-xxx to MILESTONE-2 dependsOn array, 3) Commit the JSON changes. If no issues found, mark passes:true with note 'No refactoring needed'. Loop will pick up new tasks automatically.",
  "touchesFiles": ["<list all test files>"],
  "dependsOn": ["<all TEST-xxx tasks>"],
  "synergyWith": [],
  "batchWith": [],
  "conflictsWith": [],
  "modifiesBehavior": false
}
```

#### REFACTOR-REVIEW-3 Template (priority 65, before MILESTONE-FINAL)

```json
{
  "id": "REFACTOR-REVIEW-3",
  "title": "Final refactoring review for maintainability",
  "description": "Comprehensive review of all code for DRY violations, complexity hotspots, coupling issues, and code clarity",
  "acceptanceCriteria": [
    "No code duplication across entire implementation",
    "All functions under 30 lines or documented exceptions",
    "Clear separation of concerns between modules",
    "Code follows project patterns (see CLAUDE.md section 8)",
    "Any issues found have corresponding REFACTOR-3-xxx tasks added to JSON"
  ],
  "priority": 65,
  "estimatedEffort": "medium",
  "model": "<opus-id>",
  "passes": false,
  "notes": "Final opportunity to improve code before merge. For each issue found: 1) Add a REFACTOR-3-xxx task to the JSON (priority 66-80), 2) Add REFACTOR-3-xxx to MILESTONE-FINAL dependsOn array, 3) Commit the JSON changes. If no issues found, mark passes:true with note 'No refactoring needed'.",
  "touchesFiles": ["<list all implementation and test files>"],
  "dependsOn": ["INT-001", "<all integration tasks>"],
  "synergyWith": [],
  "batchWith": [],
  "conflictsWith": [],
  "modifiesBehavior": false
}
```

### Step 8: Validate and Report

After generation, verify:

- [ ] All PRD user stories are represented
- [ ] Dependencies form a valid DAG (no cycles)
- [ ] touchesFiles paths exist or are clearly new files
- [ ] Milestones have correct dependencies
- [ ] No orphan tasks (unreachable via dependencies)
- [ ] **Quality dimensions carried through**: Every implementation task has `qualityDimensions` populated from PRD section 2.5
- [ ] **Edge case coverage**: Every PRD Known Edge Case appears as an `edgeCases` entry on at least one TEST-INIT task
- [ ] **Prompt includes pre-implementation review and self-critique steps**
- [ ] **Behavior modification validation**:
  - Tasks with `modifiesBehavior: true` have a corresponding `ANALYSIS-xxx` dependency
  - Tasks modifying shared code have `consumerAnalysis` populated (or ANALYSIS task creates it)
  - If change affects code with different semantic contexts, task should be SPLIT

**Warn if:**

- A task touches caching, routing, or result handling but `modifiesBehavior` is false
- A Bug Fix task lacks the Semantic Distinctions section from PRD
- An implementation task depends on ANALYSIS but ANALYSIS has no acceptance criteria

Report to user:

```
Created:
  - .task-mgr/tasks/{feature}.json ({N} tasks)
  - .task-mgr/tasks/{feature}-prompt.md

Task breakdown:
  - {X} implementation tasks
  - {Y} test tasks
  - {Z} review/milestone tasks

Dependency graph validated: OK

To run: task-mgr loop -y .task-mgr/tasks/{feature}.json
```

## Story Sizing Guidelines

### Low Effort

- Single file modification
- Adding a field/method
- Simple configuration change
- ~30 minutes of work

### Medium Effort

- 2-3 files modified
- New function with tests
- Integration with existing system
- ~1-2 hours of work

### High Effort

- 3+ files modified
- New module/component
- Cross-cutting concerns
- Consider splitting into multiple stories
- Consider using `/ralph-loop` for iterative refinement (see below)

## Ralph Loop Integration

For `estimatedEffort: "high"` tasks — especially those involving TDD cycles, complex integrations, or iterative refinement — the agent prompt should suggest using `/ralph-loop` as an inner loop within a single task iteration.

**When to suggest ralph-loop in generated prompts:**

- TEST-INIT tasks with 10+ acceptance criteria (lots of edge cases to iterate on)
- FEAT tasks with `estimatedEffort: "high"` that touch 3+ files
- Integration tasks (INT-xxx) that require end-to-end iteration

**How it works with task-mgr:** The outer task-mgr loop selects tasks and tracks progress. For a hard task, the agent can use `/ralph-loop` to iterate within that single task until all acceptance criteria pass, then exit the ralph loop and let the outer task-mgr loop continue to the next task.

**Example prompt addition for high-effort tasks:**

```
For high-effort tasks, consider using `/ralph-loop` to iterate:
  /ralph-loop "Implement [TASK-ID]: [title]. Acceptance criteria: [list]. Output <promise>DONE</promise> when all criteria pass." --max-iterations 10
```

## Example Output

For a PRD about adding date context to prompts:

```
.task-mgr/tasks/date-context.json:
  Initial Tests - TDD (priority 1-5):
  - TEST-INIT-001: Initial tests for EnvironmentContext (priority 1)
    └── Likely scenarios: new() populates time fields, format methods return expected strings
    └── Likely edge cases: timezone at boundary, end of month
  - TEST-INIT-002: Initial tests for PromptBuilder integration (priority 3)
    └── Likely scenarios: environment_context accessible, auto-populated on new()
    └── Likely edge case: context already set before new()

  Implementation (priority 6-12):
  - ENV-001: Add time fields to EnvironmentContext (priority 6, dependsOn: TEST-INIT-001)
  - ENV-002: Add formatting methods (priority 7, dependsOn: ENV-001)
  - PB-001: Add environment_context field (priority 9, dependsOn: TEST-INIT-002)
  - PB-002: Auto-populate in new() (priority 10, dependsOn: PB-001, ENV-002)

  Code Review (priority 13, adds tasks at 14-16):
  - CODE-REVIEW-1: Review implementation (priority 13, dependsOn: all FEAT-xxx)
    └── May add: CODE-FIX-001, CODE-FIX-002, etc. to JSON

  Refactor Review 1 (priority 17, adds tasks at 18-19):
  - REFACTOR-REVIEW-1: Review for refactoring (priority 17, dependsOn: CODE-REVIEW-1)
    └── May add: REFACTOR-1-001, REFACTOR-1-002, etc. to JSON

  Milestone 1 (priority 20):
  - MILESTONE-1: Initial tests pass, implementation reviewed (priority 20)
    └── dependsOn: TEST-INIT-xxx + FEAT-xxx + CODE-REVIEW-1 + REFACTOR-REVIEW-1

  Comprehensive Testing (priority 25-38):
  - TEST-001: Comprehensive tests for EnvironmentContext (priority 25)
    └── Exceptions: invalid timezone string, nil time source
    └── Parameterized: #[rstest] for multiple timezone/format combinations
    └── Boundaries: DST transitions, leap years, year rollover
  - TEST-002: Comprehensive tests for PromptBuilder (priority 30)
    └── Error handling: nil context, malformed input
    └── Parameterized: #[rstest] for multiple builder configurations
    └── Concurrency: race conditions in context access

  Test-Driven Fixes (priority 39-42, spawned by TEST-xxx if needed):
  - IMPL-FIX-xxx: Fix issues revealed by comprehensive tests
    └── Created dynamically when tests reveal implementation gaps

  Refactor Review 2 (priority 43, adds tasks at 44-48):
  - REFACTOR-REVIEW-2: Review test code for refactoring opportunities (priority 43, dependsOn: TEST-xxx)
    └── May add: REFACTOR-2-001, REFACTOR-2-002, etc. to JSON

  Milestone 2 (priority 50):
  - MILESTONE-2: Full test coverage achieved (priority 50)
    └── dependsOn: MILESTONE-1 + TEST-xxx + IMPL-FIX-xxx + REFACTOR-REVIEW-2

  Integration (priority 55-65):
  - INT-001: Integration tests (priority 55)

  Refactor Review 3 (priority 70, adds tasks at 71-85):
  - REFACTOR-REVIEW-3: Final refactoring review (priority 70, dependsOn: INT-001)
    └── May add: REFACTOR-3-001, REFACTOR-3-002, etc. to JSON

  Final Verification (priority 90-95):
  - VERIFY-001: Full test suite (priority 90, dependsOn: INT-001, REFACTOR-REVIEW-3)

  Milestone Final (priority 99):
  - MILESTONE-FINAL: Ready for merge (priority 99, dependsOn: VERIFY-001, REFACTOR-REVIEW-3 + added tasks)
```

## Notes

- Preserve PRD intent - don't over-engineer the task breakdown
- Keep stories atomic - one clear deliverable per story
- Front-load risky/uncertain work - fail fast
- Include code review gates before major milestones
- Test tasks should reference specific coverage targets
- Use the project-config.json for consistent defaults if available

> **Final validation — the generated tasks and prompt MUST include:**
>
> - Every PRD edge case appears as an `edgeCases` entry on at least one TEST-INIT task
> - Every implementation task has `qualityDimensions` populated from PRD section 2.5
> - The prompt template contains the "Non-Negotiable Process" section (approach before code, self-critique after code)
> - TEST-INIT tasks require known-bad discriminator tests
> - CODE-REVIEW-1 checks that quality dimensions and edge cases were actually addressed
