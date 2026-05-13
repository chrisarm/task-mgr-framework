# /tasks - Convert PRD to Claude Loop Task List

Convert a markdown PRD into JSON task list and prompt file for task-mgr loop execution.

## Usage

```
/tasks tasks/prd-{feature}.md
/tasks                          # Will prompt for PRD path
```

## Instructions

You are converting a human-readable PRD into machine-executable task artifacts for the Claude Loop autonomous agent system.

> **CRITICAL — Three principles must be embedded in every task and the prompt file:**
>
> 1. **Quality dimensions explicit** — every implementation task carries `qualityDimensions` (one flat list) from PRD section 2.5. The agent must know what "good" looks like, not just what to build.
> 2. **Edge cases = test cases** — every PRD Known Edge Case becomes an `edgeCases` entry on a TEST-INIT task. 1:1 mapping, no exceptions. Unnamed edge cases get discovered in production.
> 3. **Scoped per-iteration, full suite at milestones** — iterations run format + type-check + lint + tests scoped to `touchesFiles`. Milestones run the full unscoped suite and fix every failure (including pre-existing). This is what lets iterations move fast without letting the trunk degrade.

### Step 1: Read and Parse the PRD

Load the specified PRD file and extract:

- Feature title and type (feature/bug/enhancement/refactor)
- User stories with acceptance criteria
- Functional requirements
- Technical considerations (affected files)
- Non-goals (scope boundaries)

### Step 1.5: Resolve Current Model IDs

Do **not** hardcode model IDs — they change with each Claude release and must be read fresh each time you generate a task list.

#### Current model list: (as of 2026-04-17)

- **Opus** → value of `OPUS_MODEL` = `claude-opus-4-7`
- **Sonnet** → value of `SONNET_MODEL` = `claude-sonnet-4-6`
- **Haiku** → value of `HAIKU_MODEL` = `claude-haiku-4-5`

**Model assignment rubric** (set `model` field on tasks that need a specific tier; omit for tasks that should use the PRD-level default):

| Task type                                                                  | Assign `model`             | Rationale                                              |
| -------------------------------------------------------------------------- | -------------------------- | ------------------------------------------------------ |
| `FEAT-xxx` / `FIX-xxx` with `estimatedEffort: "high"` OR `modifiesBehavior: true` | opus                | Complex implementation — stronger model reduces rework |
| `ANALYSIS-xxx`                                                             | opus                       | Deep semantic and consumer analysis                    |
| `CODE-REVIEW-1` / `REFACTOR-REVIEW-FINAL`                                  | opus                       | Nuanced quality/security/architecture judgment         |
| `MILESTONE-xxx`                                                            | opus                       | Runs full test suite + must fix any pre-existing failures |
| `VERIFY-xxx`                                                               | opus                       | Final validation gate, thoroughness required           |
| All other implementation, test, and fix tasks                              | _(omit — use PRD default)_ | Standard work handled by the Sonnet default            |

**`timeoutSecs` assignment** (set on tasks that run the full test suite):

| Task type       | `timeoutSecs` | Rationale                                             |
| --------------- | ------------- | ----------------------------------------------------- |
| `MILESTONE-xxx` | 1800          | Deep cross-PRD review + task updates can be extensive |
| `VERIFY-xxx`    | 1800          | Same — runs complete test suite                       |
| All others      | _(omit)_      | Uses loop default (12 min)                            |

Set the resolved **sonnet** model as the PRD-level `"model"` field:

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

- **All correctness / performance / style requirements** → merge into each implementation task's `qualityDimensions` array (flat — no sub-buckets). One clear line per requirement.
- **Known edge cases** → become `edgeCases` entries on TEST-INIT tasks

Every edge case in the PRD table MUST appear as an `edgeCases` entry on at least one TEST-INIT task. This ensures the implementing agent is forced to handle it rather than hoping to discover it independently.

#### Data Flow Contracts

From the PRD's **Section 6 (Data Flow Contracts)**, extract the concrete access patterns and embed them in:

1. **The prompt file** — as a "Data Flow Contracts" section with copy-pasteable code showing correct key paths (see prompt template below)
2. **Implementation task `notes`** — remind the agent which key types to use at each level
3. **TEST-INIT task `notes`** — require tests to use production-shaped data structures (real structs/schemas), not hand-built maps that might accidentally match the wrong key format

> **Why this is critical**: The #1 source of silent bugs in multi-layer systems is data access path errors — using atom keys on string-keyed maps or vice versa. Tests that construct synthetic data matching the wrong key format pass even though the code is wrong. The PRD's Data Flow Contracts section provides verified access patterns; this step ensures those patterns reach the implementing agent.

If the PRD lacks a Data Flow Contracts section but the feature accesses data across module boundaries, **generate one now** by reading existing code to verify the actual key types at each level.

---

### Step 2: Explore the Codebase

For each user story, use Glob/Grep to populate two fields:

- **`touchesFiles`**: which files will be modified. Drives CODE-REVIEW scope, test-scoping, and synergy tie-breaking at selection time.
- **`dependsOn`**: implementation order. Schema/types first → backend logic → API/endpoints → UI; base functionality before extensions.

Do NOT populate `synergyWith` / `batchWith` / `conflictsWith` — `task-mgr next` derives synergy from `touchesFiles` overlap at runtime, and anything genuinely conflicting should be expressed as `dependsOn`.

### Step 2.5: Recall Relevant Learnings

Even though the PRD may have already queried `task-mgr recall` during its drafting (Step 4.7 of `/prd`), run recall **again here** when converting to tasks. At conversion time you know the exact files and functions being touched — that precision lets you find learnings the PRD-level recall missed.

Run **both tag-based AND query-based recall** — they hit different indexes and return different results:

```bash
# Tag-based: exact-match on curated tags (use PRD tags + discovered domain terms)
task-mgr recall --tags <domain1> --limit 10
task-mgr recall --tags <domain2> --limit 10

# Query-based: full-text / semantic search over title + content
task-mgr recall --query "<specific function names, file paths, or concepts>" --limit 10
task-mgr recall --query "<failure symptoms the PRD mentions>" --limit 10

# Combined: tag AND query for narrow results
task-mgr recall --tags <domain> --query "<concept>" --limit 10
```

**Why run both:** tag searches miss learnings that weren't tagged with your exact domain term (taggers are inconsistent). Query searches catch those via content matching. Conversely, query searches can miss high-signal learnings whose content phrases the topic differently. Run at least one of each per task being generated.

**Effective query-based searches:**
- Function names discovered in Step 2 exploration (e.g., `evaluate_transition`, `compute_auto_invoke_requests`)
- Type names and error messages from the existing code
- Concept phrases: "cache invalidation", "stale state", "init ordering", "wrong key type"

**How to use recalled learnings:**
1. **Embed in task `notes`** — add `Learning [ID]: <summary>` lines so the loop agent reads them before coding
2. **Adjust acceptance criteria** — if a learning reveals a known-bad pattern, add it as a negative criterion or known-bad discriminator
3. **Add to prompt file** — include a "Key learnings from task-mgr" section in the prompt (see Step 11's prompt template)

Skip this step only if task-mgr has no learnings (fresh project) or the feature is purely greenfield with no overlap.

### Step 3: Validate Story Sizing

For each user story, check complexity indicators:

**Warn if too large (suggest splitting):**

- More than 4 acceptance criteria that modify code
- Touches more than 4 files
- Description exceeds 150 words
- Spans multiple architectural layers

**Flag for review (recommended split):**

- More than 7 acceptance criteria — agents start losing coherence at this size. Split unless there's a strong reason (e.g., atomic migration). If the `md-to-json-prd-reviewer` flags it, split unless you can justify keeping together.
- Touches more than 7 files — high blast radius for one iteration. Flag for reviewer decision.

**MUST split if (hard rule for automation reliability):**

- More than 12 acceptance criteria — autonomous agents lose coherence across this many requirements
- Touches more than 10 files — too many files for a single iteration to handle reliably
- Split into subtasks (e.g., FEAT-001a, FEAT-001b) with clear boundaries between them

**Effort sizing** (set `estimatedEffort` on each task):

| Effort   | Indicators                                                           |
| -------- | -------------------------------------------------------------------- |
| `low`    | 1 file, 1-3 acceptance criteria, single function/field               |
| `medium` | 2-3 files, new function with tests, integration with existing system |
| `high`   | 3+ files, new module/component, cross-cutting — consider splitting   |

### Step 4: Generate Story IDs

Use context-appropriate prefixes. Set the `taskType` field on each task to let the agent apply different strategies per type:

| Prefix                | `taskType`         | Notes                                                              |
| --------------------- | ------------------ | ------------------------------------------------------------------ |
| `ANALYSIS-xxx`        | `"analysis"`       | Consumer and semantic analysis (priority 0, blocks implementation) |
| `FEAT-xxx`            | `"implementation"` | New features                                                       |
| `FIX-xxx`             | `"implementation"` | Bug fixes                                                          |
| `ENV-xxx`             | `"implementation"` | Environment/configuration                                          |
| `TEST-INIT-xxx`       | `"test"`           | Initial TDD tests (before implementation)                          |
| `TEST-xxx`            | `"test"`           | Comprehensive test implementation                                  |
| `INT-xxx`             | `"verification"`   | Integration verification                                           |
| `WIRE-xxx`            | `"implementation"` | Integration wiring (spawned by CODE-REVIEW)                        |
| `WIRE-FIX-xxx`        | `"implementation"` | Fix wiring issues (exports, registration, call sites)              |
| `CODE-REVIEW-xxx`     | `"review"`         | Code review tasks                                                  |
| `REFACTOR-xxx`        | `"implementation"` | Refactoring tasks                                                  |
| `REFACTOR-REVIEW-xxx` | `"review"`         | Refactoring review tasks                                           |
| `VERIFY-xxx`          | `"verification"`   | Final validation + documentation                                   |
| `MILESTONE-xxx`       | `"milestone"`      | Gate checkpoints                                                   |
| `POLISH-xxx`          | `"implementation"` | Formatting/cleanup                                                 |

**Special `taskType` values:**

- `"research"` — For spike/evaluation tasks (e.g., "evaluate 3 libraries, write ADR"). Set `requiresHuman: true` so the loop agent skips it and flags it for human attention. Set `difficulty: "high"` so the loop controller selects a larger model if it does attempt the task.
- `"milestone"` — Agent behavior: review completed work, update remaining tasks, check sibling PRDs.
- `"review"` — Agent behavior: read and analyze code, spawn fix tasks, don't implement.
- `"verification"` — Agent behavior: run full test suite, verify integration, update docs.

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

### Step 4.7: Enrich Cross-Boundary Tasks with Data Contract Snippets

For tasks where `touchesFiles` (including dependencies' `touchesFiles`) spans different top-level directories (e.g., `src/commands/` and `src/loop_engine/`, or `src/db/` and `src/models/`):

1. **Identify the boundary**: Which module produces data and which consumes it?
2. **Read the actual struct definitions** at the boundary (use Grep/Read — never guess from variable names)
3. **Embed a concrete data shape example** in the task's `notes` field showing:
   - The source struct/type (with relevant fields)
   - The target struct/type (with relevant fields)
   - A copy-pasteable example of the correct access pattern
4. **Source from the codebase** — the example must come from reading real struct definitions, not invented

Example `notes` enrichment:
```
Data contract: PrdUserStory (src/commands/init/parse.rs) → Task (src/models/task.rs)
Source fields: id, title, description, priority, passes, acceptance_criteria
Access: task.acceptance_criteria = serde_json::to_string(&story.acceptanceCriteria)?
```

**When to skip**: If all `touchesFiles` are in the same directory, or the task only adds new code with no cross-module dependencies.

### Step 4.8: Auto-Detect Cross-Boundary Integration Gaps

After building the dependency graph (Step 4) and enriching cross-boundary tasks (Step 4.7), scan for integration paths that need INT-xxx coverage:

1. **For each dependency edge** (`dependsOn` relationship), check if the two tasks' `touchesFiles` are in different top-level directories (e.g., `src/commands/` vs `src/loop_engine/`, or `src/db/` vs `src/models/`)
2. **If cross-boundary paths exist** and no INT-xxx task already traces that specific path, generate one:
   - Name the specific data/control path being traced
   - List the handoff points at each module boundary
   - Set `taskType: "verification"` and priority 55-65
3. **Cap**: 1 INT-xxx per distinct cross-boundary data/control path, not per task pair. Multiple tasks touching the same cross-boundary path share a single INT-xxx.

**When to skip**: If all tasks touch files in the same top-level directory, or the PRD is small enough (2-4 tasks) that CODE-REVIEW-1 will catch any wiring issues.

### Step 5: Create JSON Task File

Generate `tasks/{feature}.json` following this schema.

**Required: `taskPrefix`** — **Do NOT generate this yourself.** Leave `taskPrefix` absent from the JSON. The `task-mgr init` command will auto-generate a deterministic prefix from `md5(branchName + ":" + filename)[..8]` and write it back to the JSON file. This ensures the prefix is stable across re-imports and matches what the loop engine uses. If you set a `taskPrefix` manually, it may conflict with the auto-generated one, causing tasks to be imported under the wrong namespace and breaking dependency tracking.

**Cross-PRD dependencies: `requires`** — If this PRD depends on another PRD being completed first (e.g., proto changes must land before Home can use them), add a top-level `requires` array:

```json
"requires": [
  {
    "prd": "01-proto-redesign.json",
    "task": "MILESTONE-FINAL",
    "reason": "SigningKey message must exist in enrollment.proto"
  }
]
```

The agent checks these before starting any task. If the required task in the other PRD hasn't passed, the agent outputs `<promise>BLOCKED</promise>` with the reason.

```json
{
  "version": "1.0",
  "project": "{{PROJECT_NAME}}",
  "model": "<resolved-sonnet-id>",
  "branchName": "feat/{feature-name}",
  "externalGitRepo": "{{EXTERNAL_GIT_REPO_OR_OMIT}}",
  "mergeStrategy": "Merge to main after MILESTONE-FINAL passes. Squash commits optional.",
  "description": "{Feature description from PRD}",
  "requires": [],
  "priorityPhilosophy": {
    "description": "Hierarchy of what matters most when implementing tasks",
    "hierarchy": [
      "1. PLAN — Anticipate edge cases before coding",
      "2. PHASE 2 FOUNDATION — ~1 day now to save ~2+ weeks later (1:10+ ratio); we are pre-launch, foundations compound",
      "3. FUNCTIONING CODE — Pragmatic, reliable, wired in per plan",
      "4. CORRECTNESS — Compiles, type-checks, scoped tests pass deterministically",
      "5. CODE QUALITY — Clean code, qualityDimensions satisfied, no warnings",
      "6. POLISH — Docs, formatting, minor improvements"
    ],
    "principles": [
      "Quality dimensions explicit — qualityDimensions on every task tells you what 'good' looks like",
      "Phase 2 foundation — prefer solutions that lay strong post-launch foundations (1:10+ savings ratio)",
      "Edge cases = test cases — every known edge case must have a corresponding test",
      "Scoped per-iteration tests, full suite at milestones — milestones must leave the trunk green including pre-existing failures",
      "Ship working code with tests to prove it; handle Option/Result explicitly; avoid unwrap() in production"
    ]
  },
  "prohibitedOutcomes": [
    "Tests that only assert 'no crash' or check type without verifying content",
    "Tests that mirror implementation internals (break when refactoring)",
    "Abstractions with only one concrete use",
    "Error messages that don't identify what went wrong",
    "Catch-all error handlers that swallow context"
  ],
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
      "taskType": "implementation",
      "description": "What this story accomplishes",
      "acceptanceCriteria": [
        "Specific, testable criterion 1",
        "Specific, testable criterion 2",
        "CONTRACT: field names match EXACTLY the struct fields in {source module} (grep to verify)",
        "CONTRACT: serde_json::from_value::<TargetStruct>(output) succeeds with production data"
      ],
      "priority": 1,
      "estimatedEffort": "low|medium|high",
      "passes": false,
      "requiresHuman": false,
      "environmentRequirements": ["docker", "protoc", "uv"],
      "preflightChecks": ["docker --version", "protoc --version"],
      "completionCheck": "cargo test -p deskmait-proto",
      "notes": "Implementation hints, gotchas",
      "model": "<opus-id-if-review/milestone, omit otherwise>",
      "timeoutSecs": 1800,
      "touchesFiles": ["path/to/file.rs"],
      "dependsOn": [],
      "modifiesBehavior": false,
      "qualityDimensions": ["What 'good' looks like for this task — from PRD 2.5: correctness invariants, perf/efficiency requirements, idiomatic patterns vs anti-patterns. One flat list, no sub-buckets."],
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

Create `tasks/{feature}-prompt.md` using the template below, replacing placeholders:

- `{{PROJECT_NAME}}` - Determine from (in order of priority):
  1. `tasks/project-config.json` field `"project"`
  2. `package.json` field `"name"`
  3. `Cargo.toml` field `name` in `[package]`
  4. Current directory name
- `{{EXTERNAL_GIT_REPO_OR_OMIT}}` - **REQUIRED if code lives in a different git repo than task-mgr.** Set to relative path (e.g. `"../restaurant_agent_ex"`). Without this, the loop cannot detect task completion from commits in the external repo and tasks get stuck as `in_progress` forever. Omit the field entirely if the code and task-mgr are in the same repo.
- `{{FEATURE_TITLE}}` - Feature name from PRD
- `{{FEATURE_NAME}}` - Kebab-case filename (e.g., `date-context`)
- `{{PROBLEM_STATEMENT}}` - Problem description from PRD
- `{{REFERENCE_CODE}}` - Optional: code patterns identified during exploration
- `{{DATA_FLOW_CONTRACTS}}` - Optional but **strongly recommended**: Copy-pasteable access patterns from PRD Section 6 "Data Flow Contracts". If the feature accesses data across module boundaries, this section prevents the #1 class of silent bugs (wrong key types). Read actual code to verify key types at each level — never guess from variable names.
- `{{KEY_LEARNINGS}}` - **REQUIRED for context economy**: Distilled excerpts from `task-mgr recall` (Step 2.5). Embed the 5-10 most relevant learnings (IDs + one-line summaries) directly in the prompt so the loop agent does **not** need to call `task-mgr recall` on every iteration or Read `tasks/long-term-learnings.md` / `tasks/learnings.md` at all. Format: `- **[ID]** <one-line takeaway>`. Omit the section entirely only when recall returned zero relevant hits.
- `{{CLAUDE_MD_EXCERPTS}}` - **REQUIRED if the PRD touches any area documented in CLAUDE.md**: Grep CLAUDE.md for the touched subsystems (e.g. "ADP", "workflow", "KB", "sanitization") and paste the 3-10 bullet points that matter for this PRD — nothing more. This way the loop agent never has to Read CLAUDE.md (which can be hundreds of lines) during iterations. Omit the section if the PRD is greenfield and no existing gotchas apply.
- `{{PROHIBITED_OUTCOMES}}` - **REQUIRED, sourced from the JSON you're generating**: Render the `prohibitedOutcomes` array from the PRD JSON as a bulleted list (one `- ` line per entry). The loop agent is told not to Read the JSON, so these must live in the prompt.
- `{{GLOBAL_ACCEPTANCE_CRITERIA}}` - **REQUIRED, sourced from the JSON**: Render the `globalAcceptanceCriteria.criteria` array from the PRD JSON as a bulleted list. Same reason — the agent can't see the JSON fields directly, so anything that applies to every task must be embedded here.
- `{{CROSS_PRD_REQUIRES}}` - **REQUIRED only when the JSON `requires[]` array is non-empty**: Render each entry as a bulleted line: `- **<other-prd>.json :: <task-id>** — <reason>`. Omit the whole conditional section when `requires[]` is empty. The loop agent reads this block every iteration to decide whether to block, so it must be present; do NOT expect the agent to `jq '.requires'` during iterations.
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

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

{{PROHIBITED_OUTCOMES}}

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

{{GLOBAL_ACCEPTANCE_CRITERIA}}

---

{{#if CROSS_PRD_REQUIRES}}

## Cross-PRD Dependencies (check before every task)

This PRD blocks on work in other PRD files. Before claiming any task, verify each entry below shows `passes: true` in its referenced PRD JSON (use `jq '.userStories[] | select(.id=="<id>") | .passes' tasks/<other-prd>.json`). If any is still `false`, output `<promise>BLOCKED</promise>` with the reason and stop.

{{CROSS_PRD_REQUIRES}}

---

{{/if}}

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Cross-PRD Requires, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/{{FEATURE_NAME}}.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD. If a later note says `{{TASK_PREFIX}}`, substitute `$PREFIX`.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task    | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by MILESTONE-N` — priority auto-computed; DB + PRD JSON updated atomically                                                   |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task (rare — e.g., cross-PRD `requires[]`), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/{{FEATURE_NAME}}.json
jq '.globalAcceptanceCriteria' tasks/{{FEATURE_NAME}}.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/{{FEATURE_NAME}}-prompt.md`   | This prompt file (read-only)                                               |
| `tasks/progress-{{TASK_PREFIX}}.txt` | Progress log — **tail** for recent context, **append** after each task     |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration. Two targeted patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on, or a dependsOn task)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing; never crash on absent files.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/{{FEATURE_NAME}}.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes` — everything you need for the task. If it reports no eligible task or unmet cross-PRD `requires`, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `task-mgr next` listed a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log (`grep -n -A 40 '## .* - <THAT-TASK-ID>' tasks/progress-$PREFIX.txt`). Skip entirely on the first iteration (file won't exist).

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, or the task touches a file that's likely documented there, `grep` for the relevant term and read only the surrounding lines:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```
   The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, Key Context, and the CLAUDE.md excerpts that matter for this PRD) are already embedded in **this prompt file**. Prefer it over re-reading source docs.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed. Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section or grep 2-3 existing call sites. Never guess key types from variable names.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — and even then, one rejected alternative with a one-line reason is enough. For normal tasks: pick and go.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). Fix failures before committing; never commit broken code.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:` as appropriate). Multiple tasks per iteration: `feat: ID1-completed, ID2-completed - [Title]`.

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON. (Legacy `<completed>TASK-ID</completed>` still works; prefer `<task-status>`.)

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---` so the next iteration's tail works.

11. For TEST-xxx tasks: target 80%+ coverage on new methods; use `assert_eq!` on string outputs.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` already picks: eligible tasks (`passes: false`, deps complete, not `requiresHuman`), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority. You don't pick — you claim what it returns.

Two runtime checks you DO own:

- If the returned task has `preflightChecks`, run them. If any fails: `task-mgr skip <TASK-ID> --reason "<preflight failure>"` and re-run `task-mgr next`.
- If the previous task had a `completionCheck`, run it before starting the new one. If it fails: `task-mgr fail <prev-task> --error "completionCheck failed"` and fix it first.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

1. **ANALYSIS gate**: a corresponding `ANALYSIS-xxx` must exist and have `passes: true`. If missing, `task-mgr add --stdin` one and work on it first.
2. **Consumer Impact Table** (in the progress file from the ANALYSIS task):
   - `BREAKS` → split the task into per-context subtasks (e.g. `FIX-002a`, `FIX-002b`) via `task-mgr add`, then `task-mgr skip` the original with reason "split into …".
   - `NEEDS_REVIEW` → verify manually before implementing.
   - `OK` → proceed.
3. **Semantic distinctions**: if ANALYSIS identified multiple contexts for the same code path (e.g. LLM-invoked vs auto-invoke), each context may need different handling — split rather than shoehorn.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **milestones** run the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (implementation / test / fix tasks)

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
# Rust — scope tests to the touched crate/module (grep touchesFiles to pick)
cargo fmt --check
cargo check                                         # fast type check
cargo clippy -- -D warnings
cargo test -p <affected-crate>                       # whole crate
cargo test -p <affected-crate> <module_or_fn_name>   # narrower match within the crate

# Python
ruff check --fix && ruff format
mypy --strict <touched/dir>
pytest tests/<touched_module> -x                     # scope to tests around changed files
```

Scoping heuristic: start from `touchesFiles`. For each Rust file, run `cargo test -p <its crate>`. For Python, run `pytest` against the test file(s) that target the touched module. If you can't determine the scope confidently, widen to the whole package (still cheaper than the full workspace).

**Do NOT** run the entire workspace test suite (`cargo test` with no filter, `pytest` with no path) during regular iterations — that's the milestone's job.

### Milestone gate (MILESTONE-1 / -2 / -FINAL)

Milestones run the **full, unscoped** suite on a clean checkout and must finish green:

```bash
# Rust
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test

# Python
ruff check && ruff format --check && mypy --strict && pytest
```

If ANY test fails — including pre-existing failures that predate this PRD — the milestone fixes them. Default: **attempt every failure**, even ones that look out-of-scope. They become scope the moment the milestone gates the phase on the full suite being green. Trunk-green is the invariant this mechanism exists to protect.

Pragmatic escape hatch: if there are **more than ~12 failures AND they're all clearly unrelated to this PRD** (e.g., a sibling team's integration test against a now-missing service), don't try to do all of them inline. Triage:

1. Fix everything you can attribute to this PRD's changes, inline in the milestone commit.
2. For the remaining unrelated failures: spawn a single `FIX-xxx` or `CLARIFY-xxx` task via `task-mgr add --stdin --depended-on-by <THIS-MILESTONE>` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID so a human can route ownership.

Below the ~12-failure threshold, just fix them. Each failure you punt is a tax on every future milestone, so the bar to punt is deliberately high.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production — CODE-REVIEW-1 verifies. Most common misses:

- Not registered in dispatcher/router → add to registration
- Test mocks bypass real wiring → verify production path separately
- Config field read but not passed through → wire through
- Unused-import warning on new code → call sites missing
- Wrong key type on map access (atom vs string) — struct keys ≠ JSONB keys → check Data Flow Contracts
- New CLI subcommand / DB column / JSON field defined but not threaded into the dispatcher / `TryFrom<Row>` / parse-to-task mapping

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REFACTOR-REVIEW-FINAL`) spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

### What each review looks for

| Review                  | Priority | Spawns (priority)                  | Before            | Focus                                                                                                   |
| ----------------------- | -------- | ---------------------------------- | ----------------- | ------------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | MILESTONE-1       | Language idioms, security, memory, error handling, no `unwrap()`, `qualityDimensions` met, wiring reachable |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)             | MILESTONE-FINAL   | All code + tests: DRY, complexity, coupling, clarity, pattern adherence — full-context final pass        |

Use the **rust-python-code-reviewer** / equivalent language agent when reviewing code. Document findings in the progress file. If a specific prior iteration produced something ugly and you don't want to wait for REFACTOR-REVIEW-FINAL, invoke `/simplify` on that touchpoint directly — don't file a dedicated review task just for it.

### Spawning follow-up tasks

One shape covers CODE-FIX, WIRE-FIX, and all REFACTOR-N-xxx — vary `id`, `priority`, and include `rootCause`/`exactFix`/`verifyCommand` for fix tasks so the implementing agent lands the fix in one pass:

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-1: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 14,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by MILESTONE-1
```

`--depended-on-by` wires the new task into the milestone's `dependsOn` AND syncs the PRD JSON atomically — don't edit the JSON yourself. Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-{{TASK_PREFIX}}.txt` (create with a one-line header if missing). Keep it **tight** — future iterations tail this; verbosity here bloats every later context.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block. If your entry is longer than ~25 lines, compress it — a future iteration has to read this.

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw, auto-appended). **Do not Read those files directly** during a loop iteration — they grow unboundedly. Instead:

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval of learnings scored for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries when recall is sparse

Record your own learnings with `task-mgr learn` so they're indexed for future recall. Don't append directly to those files.

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

1. Document blocker in the progress file
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Add to JSON and commit: `chore: Add blocker task CLARIFY-001`
4. Output:

```
<promise>BLOCKED</promise>
```

---

## Milestones

Milestones (MILESTONE-xxx) are **full-gate checkpoints**: they prove the trunk is green before the next phase begins. They are NOT a sweep to rewrite remaining tasks — stale tasks self-correct when their agent picks them up.

### Milestone Protocol

1. Check all `dependsOn` tasks have `passes: true`. If any don't, the milestone can't run yet.
2. **Run the full quality gate** (see Quality Checks § Milestone gate — unscoped format, type-check, lint, and the complete test suite). This is the ONE place in the loop where the entire test suite runs.
3. **Leave the repo green.** For every failure, including pre-existing ones that predate this PRD:
   - Trivial fixes go in the milestone's own commit: `chore: MILESTONE-N - fix stale test <name>`.
   - Non-trivial failures → spawn a `FIX-xxx` task via `task-mgr add --stdin --depended-on-by <THIS-MILESTONE>` with the failure's `verifyCommand`. The loop picks it up; the milestone re-runs when the FIX passes.
   - If the failure reveals that a remaining task in this PRD is stale or needs splitting, spawn the correction now. This is the ONLY time milestones touch the task graph — and only in response to a concrete test failure, not a speculative sweep.
4. **Batch sibling PRDs** (if the "Sibling PRD Tasks" section is present AND the full suite revealed cross-PRD breakage): update only the affected sibling tasks with `task-mgr add`/`--append --update-existing`. Commit separately: `chore: MILESTONE-N - update sibling tasks in <file>.json`.
5. Mark the milestone `<task-status>MILESTONE-N:done</task-status>` only when the full gate is green.

---

{{#if REFERENCE_CODE}}

## Reference Code

{{REFERENCE_CODE}}

---

{{/if}}

{{#if KEY_LEARNINGS}}

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

{{KEY_LEARNINGS}}

---

{{/if}}

{{#if CLAUDE_MD_EXCERPTS}}

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md` to pull just that block.

{{CLAUDE_MD_EXCERPTS}}

---

{{/if}}

{{#if DATA_FLOW_CONTRACTS}}

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments.

{{DATA_FLOW_CONTRACTS}}

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

Every task list follows this phased structure. The table is the spine; notes below cover non-obvious requirements per type.

| # | Priority | ID pattern                      | Type            | Spawned by       | Depends on                          | Model / timeout        |
| - | -------- | ------------------------------- | --------------- | ---------------- | ----------------------------------- | ---------------------- |
| 0 | 0        | `ANALYSIS-xxx`                  | analysis        | —                | —                                   | opus                   |
| 1 | 1-5      | `TEST-INIT-xxx`                 | test            | —                | ANALYSIS (if present)               | opus                   |
| 2 | 6-12     | `FEAT-xxx`/`FIX-xxx`            | implementation  | —                | relevant TEST-INIT                  | —                      |
| 3 | 13       | `CODE-REVIEW-1`                 | review          | —                | all FEAT/FIX                        | opus                   |
| 3a| 14-16    | `CODE-FIX-xxx` / `WIRE-FIX-xxx` | implementation  | CODE-REVIEW-1    | — (→ MILESTONE-1)                   | —                      |
| 4 | 20       | `MILESTONE-1`                   | milestone       | —                | CODE-REVIEW-1 + all FEAT/FIX + spawned FIX/WIRE-FIX | opus / 1800s |
| 5 | 25-38    | `TEST-xxx`                      | test            | —                | MILESTONE-1                         | —                      |
| 6 | 39-42    | `IMPL-FIX-xxx`                  | implementation  | TEST-xxx         | the failing TEST-xxx                | —                      |
| 7 | 50       | `MILESTONE-2`                   | milestone       | —                | MILESTONE-1 + all TEST-xxx + IMPL-FIX | opus / 1800s         |
| 8 | 55-65    | `INT-xxx`                       | verification    | —                | MILESTONE-2                         | —                      |
| 9 | 70       | `REFACTOR-REVIEW-FINAL`         | review          | —                | all INT-xxx                         | opus                   |
| 9a| 71-85    | `REFACTOR-xxx`                  | implementation  | REFACTOR-REVIEW-FINAL | — (→ MILESTONE-FINAL)          | —                      |
|10 | 90-95    | `VERIFY-001`                    | verification    | —                | REFACTOR-REVIEW-FINAL + all INT-xxx | opus / 1800s           |
|11 | 99       | `MILESTONE-FINAL`               | milestone       | —                | VERIFY-001 + all REFACTOR-xxx       | opus / 1800s           |

### Notes that don't fit in the table

- **ANALYSIS-xxx** (phase 0) — only when the PRD has behavior-modifying changes. Identifies consumers, documents semantic distinctions, outputs the Consumer Impact Table into the progress file. Blocks dependent FEAT/FIX until it passes.

- **TEST-INIT-xxx** (phase 1) — tests first; PRD Known Edge Cases MUST flow into the `edgeCases` field (1:1). Required per task: `edgeCases` (3+), `invariants` (2-5), `failureModes` (1+). Cross-module tests must use production-shaped data (real structs/schemas) — never hand-built maps, because matching the wrong key format silently passes.

- **FEAT-xxx** (phase 2) — set `model: "<opus-id>"` only on tasks that are `estimatedEffort: "high"` OR `modifiesBehavior: true`. The old "first FEAT is always opus" rule was pattern-worship — iterations don't inherit patterns across runs.

- **INT-xxx** (phase 8) — names a specific data/control path (e.g., "CLI arg → parse → DB insert → query → display"), lists handoff points at each boundary. Does NOT write new code unless wiring is missing. Cap: 1 INT-xxx per distinct cross-boundary path.

- **Milestones** — gates, not sweeping update sessions. Each milestone checks all its `dependsOn` are `passes: true`, **runs the full quality gate** (see Quality Checks — tests, lint, format, type-check), and **must leave the repo fully green** (fix any pre-existing failures before closing). Milestones do NOT sweep the JSON to rewrite remaining task descriptions — if a later task is stale because of an earlier implementation change, the agent working that task will fail its preflight or spawn a FIX at pickup time. This keeps the milestone's job scoped.

- **VERIFY-001** (phase 10) — authoritative final check. Acceptance must include: architecture docs updated (if new subsystems), CLAUDE.md updated, dev guide in `docs/` (if new developer-facing tooling), grep-verified reachability for new public functions/CLI commands, no `.unwrap()` in production, test fixtures use real struct field names.

- **Refactoring** — the old REFACTOR-REVIEW-1 and -2 phases (after implementation, after tests) are removed. CODE-REVIEW-1 catches most DRY/complexity issues during implementation phase; the `/simplify` skill can be invoked ad-hoc on a specific touchpoint if a task produces something ugly; REFACTOR-REVIEW-FINAL before merge catches everything with full context.

### Step 7.1: Task Templates

Every task shares a common shape (see Step 5 canonical JSON). Only the fields that vary per type are shown here.

#### TEST-INIT-xxx (priority 1-5, `taskType: "test"`, `model: <opus-id>`)

Adds three test-only fields on top of the base shape:

```json
{
  "id": "TEST-INIT-001",
  "title": "Initial tests for <feature>",
  "acceptanceCriteria": [
    "Happy path test defined: <scenario>",
    "Edge case tests cover: <from edgeCases field>",
    "Known-bad discriminator: at least one test that would PASS with a naive stub but FAIL with correct implementation",
    "Invariant assertions: <from invariants field>",
    "Test file compiles (may be #[ignore] or expected to fail)",
    "Structural assertion: serde_json::from_value::<TargetStruct>(fixture).is_ok() (Rust) or Model.model_validate(fixture) (Python)",
    "Fixtures derive field names from actual struct definitions — never invented"
  ],
  "edgeCases":     ["Empty/null input: <expected>", "Boundary value: <expected>", "Invalid/malformed: <expected>"],
  "invariants":    ["<property that must always hold>"],
  "failureModes":  [{ "cause": "<what goes wrong>", "expectedBehavior": "<how system responds>" }],
  "notes": "TDD: write tests first. Must be specific enough to reject wrong implementations. For cross-module data, use production-shaped fixtures (real structs/schemas), NEVER hand-built maps (can silently match wrong key format)."
}
```

#### IMPL-FIX-xxx (priority 39-42, `taskType: "implementation"`) — spawned by TEST-xxx

```json
{
  "id": "IMPL-FIX-001",
  "title": "Fix: <issue revealed by test>",
  "description": "Address implementation gap revealed by TEST-xxx: <specific failing test>",
  "acceptanceCriteria": ["Failing test <name> now passes", "No regression in other tests"],
  "notes": "Created by TEST-xxx. Reference: <test file:line>"
}
```

#### Review tasks — one shape, four instances

All review tasks share this structure. Vary per the table below.

```json
{
  "id": "<REVIEW-ID>",
  "title": "<review title>",
  "taskType": "review",
  "description": "<what the review analyzes>",
  "acceptanceCriteria": [
    "<type-specific criteria>",
    "Any issues found have corresponding <FIX-PREFIX>-xxx tasks added via task-mgr add --stdin"
  ],
  "priority": <P>,
  "estimatedEffort": "medium",
  "model": "<opus-id>",
  "notes": "For each issue: `echo '{...}' | task-mgr add --stdin --depended-on-by <MILESTONE>` — atomic DB+JSON sync, no manual edit. If no issues, emit `<task-status><REVIEW-ID>:done</task-status>` with a one-line progress note.",
  "dependsOn": [<see table>]
}
```

| Review ID               | Priority | Spawns prefix         | Depends on                 | Focus (drives acceptance criteria)                                     |
| ----------------------- | -------- | --------------------- | -------------------------- | ---------------------------------------------------------------------- |
| `CODE-REVIEW-1`         | 13       | `CODE-FIX` / `WIRE-FIX` | all `FEAT` / `FIX` tasks | `unwrap()`, error propagation, injection, `qualityDimensions` met, wiring |
| `REFACTOR-REVIEW-FINAL` | 70       | `REFACTOR-xxx`        | all `INT-xxx`              | All code + tests: DRY, complexity, coupling, clarity — full-context final pass |

### Step 8: Validate and Report

After generation, verify:

- [ ] `taskPrefix` is NOT set (let `task-mgr init` auto-generate it)
- [ ] All tasks have `taskType` set
- [ ] Research/spike tasks have `requiresHuman: true`
- [ ] Cross-PRD dependencies documented in `requires` array (if applicable)
- [ ] No task has >12 acceptance criteria (must split if so)
- [ ] No task touches >10 files (must split if so)
- [ ] All PRD user stories are represented
- [ ] Dependencies form a valid DAG (no cycles)
- [ ] No priority collisions between tasks with different dependency chains (add secondary sort)
- [ ] touchesFiles paths exist or are clearly new files
- [ ] Milestones have correct dependencies
- [ ] No orphan tasks (unreachable via dependencies)
- [ ] **Quality dimensions carried through**: Every implementation task has `qualityDimensions` populated from PRD section 2.5
- [ ] **Edge case coverage**: Every PRD Known Edge Case appears as an `edgeCases` entry on at least one TEST-INIT task
- [ ] **Prompt instructs scoped per-iteration testing and full-suite-at-milestones** (Quality Checks section is present and splits the two gates)
- [ ] **No task has `synergyWith` / `batchWith` / `conflictsWith` populated** (dropped — `touchesFiles` drives synergy at selection time; conflicts expressed via `dependsOn`)
- [ ] **`qualityDimensions` is a flat array**, NOT `{correctness, performance, style}` sub-objects
- [ ] **Only CODE-REVIEW-1 and REFACTOR-REVIEW-FINAL exist** — no REFACTOR-REVIEW-1 or -2 tasks in the JSON
- [ ] **Context-economy placeholders populated in the generated prompt** (the agent can't read the JSON, so these MUST be in the prompt):
  - [ ] `{{PROHIBITED_OUTCOMES}}` — rendered from JSON `prohibitedOutcomes[]` as a bullet list
  - [ ] `{{GLOBAL_ACCEPTANCE_CRITERIA}}` — rendered from JSON `globalAcceptanceCriteria.criteria[]` as a bullet list
  - [ ] `{{CROSS_PRD_REQUIRES}}` — rendered as bullets if JSON `requires[]` is non-empty; whole section omitted otherwise
  - [ ] `{{KEY_LEARNINGS}}` — 5-10 recalled learnings distilled into one-liners (or omitted if recall was empty)
  - [ ] `{{CLAUDE_MD_EXCERPTS}}` — only the CLAUDE.md bullets that apply to this PRD's touched subsystems (or omitted if greenfield)
  - [ ] Grep the generated prompt for `{{` — zero hits means all placeholders were substituted; any remaining `{{X}}` indicates a missed field
- [ ] **Data flow contracts**: If the feature accesses data across module boundaries, the prompt's `{{DATA_FLOW_CONTRACTS}}` section is populated with verified, copy-pasteable access patterns showing key types at each level. If not applicable, section is omitted.
- [ ] **Behavior modification validation**:
  - Tasks with `modifiesBehavior: true` have a corresponding `ANALYSIS-xxx` dependency
  - Tasks modifying shared code have `consumerAnalysis` populated (or ANALYSIS task creates it)
  - If change affects code with different semantic contexts, task should be SPLIT

**Warn if:**

- A task touches caching, routing, or result handling but `modifiesBehavior` is false
- A Bug Fix task lacks the Semantic Distinctions section from PRD
- An implementation task depends on ANALYSIS but ANALYSIS has no acceptance criteria
- The feature accesses nested data structures across module boundaries but no Data Flow Contracts section exists in the prompt — this is the #1 source of silent bugs in multi-layer systems

Report to user:

```
Created:
  - tasks/{feature}.json ({N} tasks)
  - tasks/{feature}-prompt.md

Task breakdown:
  - {X} implementation tasks
  - {Y} test tasks
  - {Z} review/milestone tasks

Dependency graph validated: OK

To run: task-mgr loop -y tasks/{feature}.json
```
