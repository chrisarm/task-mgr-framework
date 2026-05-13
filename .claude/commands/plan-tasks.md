# /plan-tasks - Plan & Generate Lean Task List

Generate a task list and prompt directly from a plan or description, skipping the PRD step. Ideal for small-medium tasks where the overall effort can be accomplished in 2-10 tasks.

## Usage

```
/plan-tasks "description of what to build or fix"
/plan-tasks                          # Interactive mode
```

## Instructions

You are generating a lean, executable task list for the Claude Loop agent system. This skill combines planning and task generation into one step — no intermediate PRD artifact.

> **CRITICAL — These principles must be embedded in every task and the prompt file:**
>
> 1. **Quality dimensions explicit** — every implementation task carries `qualityDimensions` (one flat list). The agent must know what "good" looks like, not just what to build.
> 2. **Edge cases = test cases** — every identified edge case becomes an `edgeCases` entry on the task that handles it. 1:1 mapping. Unnamed edge cases get discovered in production.
> 3. **Scoped per-iteration, full suite at REVIEW-001** — iterations run format + type-check + lint + tests scoped to `touchesFiles`. REVIEW-001 runs the full unscoped suite and fixes every failure (including pre-existing). This is what lets iterations move fast without letting the trunk degrade.
> 4. **Data flow contracts verified** — for any data structure accessed across module boundaries, document the exact key type at each level with a copy-pasteable access pattern. Wrong-key-type bugs are silent.
>
> **Core philosophy**: Group by coherent change, not by activity type. "Add functions + their tests" is one task. Minimize ceremony, maximize code output per loop iteration.

### Step 1: Understand the Request

If the user provided a description, analyze it. If a plan file exists (check `/home/chris/.claude/plans/` for recent files), read it. Otherwise, ask:

> What do you want to implement?

### Step 1.5: Ask Clarifying Questions

Before exploring the codebase, ask 2-4 focused clarifying questions to fill gaps. Use lettered options (A, B, C) when possible — don't ask open-ended questions that require lengthy answers.

**Always ask about:**

1. **Scope** — "What's the minimal viable version?"

   - A) MVP — just the core behavior, iterate later
   - B) Complete — do it right the first time
   - C) Phased — MVP first, then iterate (suggest split points)

2. **Behavior on ambiguous cases** — "When [X] happens, should it [A] or [B]?"
   - Identify 1-2 ambiguous scenarios from the description and ask directly
   - The agent will encounter these and needs a clear answer upfront

**Ask about if relevant:**

3. **Integration** — "Does this need to integrate with [existing system]?"

   - A) Standalone — no dependencies on other modules
   - B) Integrates with [list discovered systems]
   - C) Replaces existing functionality

4. **Error handling** — "If [operation] fails, should it [A] retry, [B] skip, or [C] abort?"

5. **Breaking changes** — "This will change [function/behavior]. Is it OK to update all callers, or do we need backward compatibility?"

   - A) Update callers — clean break
   - B) Backward compatible — deprecate old path
   - C) Feature flag — gradual rollout

6. **Testing expectations** — "Are there existing tests that define expected behavior, or are we defining new behavior from scratch?"

> **Keep it lean**: 2-4 questions max. Skip questions where the answer is obvious from context. The goal is to prevent wasted loop iterations from ambiguous requirements, not to produce a requirements doc — that's what `/prd` is for.

### Step 2: Explore the Codebase

Use Glob and Grep to quickly identify:

- Files that will be modified (populates `touchesFiles`)
- Existing patterns to follow (error handling, test structure, module layout)
- Key types/functions that will be reused or changed
- Callers of any code being modified (consumer check)
- Existing tests that will need updating
- **Data flow paths**: For any data structure that crosses module boundaries, trace the key type at each hop (struct field → map key → JSONB key). Note where key types change between levels — these need Data Flow Contracts (see Step 5.5).
- **Existing documentation**: Check `docs/` for architecture design docs. If the feature adds new modules, changes data flow, or introduces new subsystems, note which docs need creating or updating.
- **CLAUDE.md excerpts**: Grep `CLAUDE.md` for the subsystems being touched (e.g. "ADP", "workflow", "KB", "sanitization") and note the 3-10 bullet points that matter for this change. The prompt file will embed these so the loop agent never has to Read CLAUDE.md.

**Time-box this to essentials.** Identify the 3-5 critical things the loop agent needs to know upfront to avoid wasted iterations:

1. What patterns does existing code follow? (so the agent doesn't invent new ones)
2. Who calls the code being changed? (so the agent doesn't break callers)
3. What edge cases does the existing code already handle? (so the agent doesn't regress)
4. Where are the tests? (so the agent knows where to add/update them)
5. Are there any gotchas or non-obvious constraints? (from CLAUDE.md, comments, past learnings)

Do NOT populate `synergyWith` / `batchWith` / `conflictsWith` — `task-mgr next` derives synergy from `touchesFiles` overlap at runtime, and anything genuinely conflicting should be expressed as `dependsOn`.

### Step 2.5: Recall Relevant Learnings

Query `task-mgr recall` for learnings relevant to the files and domains being modified. Run **both tag-based AND query-based recall** to surface patterns, failures, and workarounds from prior loop runs — they hit different indexes and return different results:

```bash
# Tag-based: exact-match on curated tags
task-mgr recall --tags <domain1> --limit 10
task-mgr recall --tags <domain2> --limit 10

# Query-based: full-text / semantic search over title + content
task-mgr recall --query "<natural-language description of what you're touching>" --limit 10
task-mgr recall --query "<key function names, error messages, or concepts>" --limit 10

# Combined: tag AND query for narrow, high-precision results
task-mgr recall --tags <domain> --query "<concept>" --limit 10
```

**Why run both:** tag searches miss learnings that weren't tagged with your exact domain term (taggers are inconsistent). Query searches catch those via content matching. Conversely, query searches can miss high-signal learnings whose content phrases the topic differently than your query. Run **at least one of each** for any non-trivial task — do not rely on tag search alone.

**Common tag categories to search:**
- **By component**: `auth`, `grpc`, `middleware`, `config`, `pipeline`, `guardrails`, `migration`, `testing`
- **By concern**: `security`, `performance`, `database`, `validation`
- **By outcome**: `--outcome failure` (critical — avoids repeating mistakes), `--outcome pattern` (conventions to follow)

**Effective query-based searches:**
- Concrete terms from your problem: function names, type names, error messages, file paths
- Conceptual terms: "caching", "retry loop", "transition routing", "guard condition"
- Failure symptoms: "stale state", "init ordering", "wrong key type"

**How to use recalled learnings:**
1. **Embed in task `notes`**: Add a `Learning [ID]: <summary>` line for each relevant learning. The loop agent reads notes before coding — this prevents it from rediscovering patterns the hard way.
2. **Add to prompt file**: The generated prompt's `{{KEY_LEARNINGS}}` section embeds the 5-10 most relevant learnings as one-liners so the agent doesn't have to re-run recall every iteration.
3. **Adjust acceptance criteria**: If a learning reveals a known-bad pattern or a specific import path, add it as a negative criterion or known-bad discriminator.

**Skip this step** only if task-mgr has no learnings (fresh project) or the task is purely greenfield with no overlap to prior work.

### Step 3: Expose Hidden Assumptions

Before generating tasks, surface assumptions that could derail the loop agent. Use AskUserQuestion to confirm any of the following that are unclear:

**Always ask about:**

1. **Scope boundaries** — "Should this also handle [related thing] or is that out of scope?"

   - The #1 source of wasted iterations is the agent discovering mid-task that scope is larger than expected.
   - Example: "Should the archive command also handle PRDs loaded before the v9 migration (no task_prefix), or skip them?"

2. **Behavior on edge cases** — "When [unusual input/state] occurs, should it [option A] or [option B]?"

   - Don't assume — ask. The agent will encounter these cases and needs a clear answer.
   - Example: "If a PRD has metadata but zero tasks in the DB, should archive treat it as complete or skip it?"

3. **Compatibility constraints** — "Are there callers/consumers that depend on the current behavior?"
   - If exploration found callers, confirm whether their expectations are preserved.
   - Example: "branch.rs checks `result.archived.is_empty()` — should that still work the same way?"

**Ask about if uncertain:**

4. **Error handling strategy** — "If [operation] fails partway through, should we roll back, skip, or abort?"
5. **Output format expectations** — "Should the output change to show per-item details, or stay as a summary?"
6. **Testing expectations** — "Are there existing tests that define expected behavior, or are we defining new behavior?"

**Format:** Use AskUserQuestion with 2-4 focused questions. Include concrete options when possible — don't ask open-ended questions that require lengthy answers.

### Step 3.5: Define Quality Dimensions

Based on exploration and user answers, define `qualityDimensions` as **one flat list** (no sub-buckets) covering:

- **Correctness requirements** — What must the implementation get right? Specific failure modes.
- **Performance requirements** — Specific targets, or "exit early when answer is known"?
- **Style requirements** — Idiomatic patterns to follow, anti-patterns to avoid.

| Edge Case           | Why It Matters          | Expected Behavior |
| ------------------- | ----------------------- | ----------------- |
| {e.g., empty input} | {Common source of bugs} | {Return error}    |

These flow directly into each task's flat `qualityDimensions` array and `edgeCases` field.

### Step 4: Assess Complexity

| Signal                     | Small (2-4 tasks) | Medium (5-8 tasks) | Upper medium (8-10 tasks) |
| -------------------------- | ----------------- | ------------------ | ------------------------- |
| Files changed              | 1-2               | 3-5                | 5-7                       |
| Design decisions           | 0-1               | 2-3                | 3-4                       |
| Consumers/callers affected | 0-2               | 3-5                | 5-8                       |
| New public APIs            | 0                 | 1-2                | 2-3                       |
| Existing tests to update   | 0-5               | 5-15               | 15-25                     |

If the task exceeds the upper-medium range (7+ files, 4+ design decisions, 25+ test updates), suggest using `/prd` + `/tasks` instead.

### Step 5: Resolve Model IDs

Do **not** hardcode model IDs — they change with each Claude release and must be read fresh each time you generate a task list.

#### Current model list: (as of 2026-04-17)

- **Opus** → `OPUS_MODEL` = `claude-opus-4-7`
- **Sonnet** → `SONNET_MODEL` = `claude-sonnet-4-6`
- **Haiku** → `HAIKU_MODEL` = `claude-haiku-4-5`

Set the resolved **sonnet** model as the PRD-level `"model"` field. Sonnet is the iteration default; opus tasks explicitly override per-task.

**Model assignment rubric** (set `model` field only where listed; omit for everything else — uses PRD default):

| Task type                                                                   | Assign `model`             | Rationale                                              |
| --------------------------------------------------------------------------- | -------------------------- | ------------------------------------------------------ |
| `FEAT-xxx` / `FIX-xxx` with `estimatedEffort: "high"` OR `modifiesBehavior: true` | opus                 | Complex implementation — stronger model reduces rework |
| `REFACTOR-001` / `REVIEW-001`                                               | opus                       | Nuanced quality/security/architecture judgment; full suite run |
| All other implementation, test, and fix tasks                               | _(omit — use PRD default)_ | Standard work handled by the Sonnet default            |

> The old "first FEAT is always opus" rule is gone — it was pattern-worship. Iterations don't inherit patterns across runs.

**`timeoutSecs` assignment** (set on tasks that run the full test suite):

| Task type       | `timeoutSecs` | Rationale                                             |
| --------------- | ------------- | ----------------------------------------------------- |
| `REVIEW-001`    | 1800          | Runs complete test suite + may update sibling tasks   |
| `REFACTOR-001`  | 1800          | Full-context review; may spawn fix tasks              |
| All others      | _(omit)_      | Uses loop default (12 min)                            |

### Step 5.5: Define Data Flow Contracts (if applicable)

For any data structure the implementing agent will need to access across module boundaries:

1. **Trace the actual key path** through the layers — read real code, don't guess
2. **Document key type at each level** (struct/atom, map/string, JSONB/string)
3. **Provide a copy-pasteable access pattern** showing the correct way to traverse the structure
4. **Flag type transitions** where the key type changes

> **Why this matters**: Data access path bugs are silent — the code compiles, tests pass (if tests use synthetic data matching the wrong format), and failures only surface at runtime.
>
> **When to skip**: If the feature only adds new modules with no cross-module data access, this step is N/A.

**Per-task data contract enrichment**: When a task's `touchesFiles` (including dependencies' files) spans different top-level directories, embed a concrete data shape snippet in the task's `notes` field. Read the actual struct definitions from the codebase and include: source struct with relevant fields, target struct with relevant fields, and a copy-pasteable access pattern. This gives the implementing agent the exact contract at the point of use, not just in a global section.

Example `notes` enrichment:
```
Data contract: PrdUserStory (src/commands/init/parse.rs) → Task (src/models/task.rs)
Source fields: id, title, description, priority, passes, acceptance_criteria
Access: task.acceptance_criteria = serde_json::to_string(&story.acceptanceCriteria)?
```

### Step 6: Design the Task List

**Principles:**

1. **Each task = coherent unit of change + its tests.** Don't separate "write code" from "write tests for that code."
2. **Target 2-10 tasks max.** If you need more, the task is probably Large — use `/prd` + `/tasks`.
3. **One review gate, at the end.** The loop's scoped per-iteration quality checks (cargo test scoped, clippy, fmt) enforce correctness each iteration; REVIEW-001 runs the FULL gate.
4. **No separate milestone tasks.** REVIEW-001 IS the milestone.
5. **Every task has both positive and negative requirements.** What to do AND what not to do. What good looks like AND what bad looks like.
6. **Quality dimensions flow to every task.** Each task carries a flat `qualityDimensions` array so the agent knows what "good" means.
7. **Review task updates future tasks.** If issues are found, REVIEW-001 adds FIX-xxx tasks via `task-mgr add --stdin --depended-on-by REVIEW-001` (atomic DB + JSON sync) AND updates remaining task descriptions to reflect learnings.

**Task structure:**

```
FEAT-001: [First coherent change] (priority 1)
  — Implementation + unit tests (leverage dependency injection)
  — qualityDimensions (flat list)
  — Edge cases to handle (edgeCases field)
  — Known-bad patterns to avoid
  — Failure modes and expected behavior
  — Set model: opus ONLY if estimatedEffort: high OR modifiesBehavior: true

FEAT-002: [Second coherent change] (priority 2)
  — Depends on FEAT-001 if sequential
  — Implementation + tests
  — qualityDimensions + edgeCases
  — Known-bad patterns to avoid
  — Failure modes

... (2-10 FEAT tasks, grouped by coherent change)

REFACTOR-001: Full review for opportunities to improve code (priority 98)
  — Opus model, timeoutSecs: 1800
  — DRY, testable, separation of concerns, function length/complexity
  — Spawns REFACTOR-FIX-xxx via task-mgr add --stdin --depended-on-by REVIEW-001

REVIEW-001: Code review + final verification (priority 99)
  — Opus model, timeoutSecs: 1800
  — Quality, security, integration wiring, documentation
  — RUNS THE FULL QUALITY GATE (unscoped test suite)
  — Updates remaining task descriptions based on learnings
  — Spawns FIX-xxx tasks if issues found (via task-mgr add --stdin --depended-on-by REVIEW-001)
  — Checks documentation needs (architecture docs, dev guides, CLAUDE.md)
```

### Step 6.5: Writing Effective Acceptance Criteria

Each acceptance criterion should be **specific enough that a different person (or agent) could verify it unambiguously**. Include both what should happen and what should NOT happen.

**Good acceptance criteria patterns:**

```
"Clean and testable: add_task is < 30 lines, only performs one function, and uses dependency injection for easier testing"
"Unit test: is_completed_by_prefix('P1') returns true when all P1-* tasks are done"
"Unit test: is_completed_by_prefix('P1') returns false when P1-US-003 is still 'todo'"
"Edge case: prefix 'P1' must NOT match tasks with prefix 'P10' (dash separator prevents this)"
"Known-bad: a global COUNT(*) without WHERE clause would pass this test but is WRONG — must scope by prefix"
"Negative: clear_prd_data must NOT delete tasks from other PRDs — verify PRD B tasks unchanged after clearing PRD A"
"Negative: must NOT use unwrap() — use map_err(TaskMgrError::DatabaseError) for all DB operations"
"CONTRACT: field names match EXACTLY the struct fields in {source module} (grep to verify)"
"CONTRACT: serde_json::from_value::<TargetStruct>(output_from_dependency) succeeds with production data"
```

**CONTRACT: prefix — cross-module boundary checks:**

When a task has `dependsOn` that span different directories/modules (e.g., `src/commands/` and `src/loop_engine/`), add `CONTRACT:` prefixed acceptance criteria. These signal that the criterion verifies a cross-module integration contract, not just local correctness. The loop agent should verify these by grepping actual struct definitions, not by assuming field names from context.

**Bad acceptance criteria (too vague — avoid these):**

```
"Tests pass"                        → Which tests? What do they verify?
"Error handling works"              → What errors? What's the expected behavior?
"Code is clean"                     → By what standard? What patterns?
"Edge cases handled"                → Which ones specifically?
"Integration works"                 → With what? How to verify?
```

**Known-bad discriminators:**

A known-bad discriminator is a test that would PASS with a naive or wrong implementation but FAIL with the correct one. These are critical for preventing plausible-but-wrong code.

Examples:

```
"Known-bad: if is_completed checks ALL tasks globally (no prefix filter), it would return
 false when other PRDs have incomplete tasks — even though THIS PRD is fully done. The test
 must set up two PRDs (one complete, one incomplete) and verify the complete one returns true."

"Known-bad: if clear_prd_data uses DELETE FROM tasks (no WHERE), it passes the 'tasks deleted'
 assertion but ALSO deletes other PRDs' tasks. Test must verify other PRD's tasks still exist."

"Known-bad: if the LIKE pattern uses 'P1%' instead of 'P1-%', prefix P1 would match P10's
 tasks. Test must include a P10 task and verify it's excluded."
```

**Failure modes — what to do when things go wrong:**

Each task should specify expected behavior for its failure scenarios:

```
"failureMode": "If transaction fails mid-cleanup: entire PRD cleanup must roll back — no partial state"
"failureMode": "If file move fails: report error but don't roll back DB changes (files are best-effort)"
"failureMode": "If prd_files table missing (pre-v6): fall back to project-name guessing, don't error"
```

### Step 7: Determine Project Info

Determine `project` name from (in priority order):

1. `Cargo.toml` field `name` in `[package]`
2. `package.json` field `"name"`
3. Current directory name

Determine `externalGitRepo`:

- **REQUIRED if code lives in a different git repo than task-mgr.** Set to relative path (e.g. `"../my_project"`). Without this, the loop cannot detect task completion from commits in the external repo and tasks get stuck as `in_progress` forever.
- Omit entirely if code and task-mgr are in the same repo.

### Step 8: Generate JSON Task File

Create `tasks/{feature-name}.json`.

**Required: `taskPrefix`** — **Do NOT generate this yourself.** Leave `taskPrefix` absent from the JSON. The `task-mgr init` command will auto-generate a deterministic prefix from `md5(branchName + ":" + filename)[..8]` and write it back to the JSON file. This ensures the prefix is stable across re-imports and matches what the loop engine uses.

**Cross-PRD dependencies: `requires`** — If this task list depends on another PRD being completed first, add a top-level `requires` array:

```json
"requires": [
  {
    "prd": "01-proto-redesign.json",
    "task": "MILESTONE-FINAL",
    "reason": "SigningKey message must exist in enrollment.proto"
  }
]
```

The agent checks these before starting any task. If the required task hasn't passed, the agent outputs `<promise>BLOCKED</promise>` with the reason.

```json
{
  "version": "1.0",
  "project": "{{PROJECT_NAME}}",
  "model": "<resolved-sonnet-id>",
  "branchName": "feat/{feature-name}",
  "externalGitRepo": "{{EXTERNAL_GIT_REPO_OR_OMIT}}",
  "description": "Brief description of the change",
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
      "Scoped per-iteration tests, full suite at REVIEW-001 — REVIEW must leave the trunk green including pre-existing failures",
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
  "userStories": [
    {
      "id": "FEAT-001",
      "title": "Concise title",
      "taskType": "implementation",
      "description": "What to implement and key context.\n\nDO: [positive requirements]\nDO NOT: [negative requirements / anti-patterns to avoid]\n\nEdge cases:\n- [case]: [expected behavior]\n\nKnown-bad to guard against:\n- [naive approach that would seem right but is wrong]",
      "acceptanceCriteria": [
        "Positive: [what should happen — specific, testable]",
        "Negative: [what must NOT happen — specific, testable]",
        "Unit test: [scenario] returns [expected result]",
        "Unit test: [edge case scenario] returns [expected result]",
        "Known-bad: [describe naive implementation that would pass other tests but is wrong, and the test that catches it]",
        "Failure mode: if [error scenario], then [expected recovery behavior]",
        "CONTRACT: field names match EXACTLY the struct fields in {source module} (grep to verify)"
      ],
      "priority": 1,
      "estimatedEffort": "low|medium|high",
      "passes": false,
      "notes": "Implementation hints. Key functions to reuse: [list with file paths]. Patterns to follow: [reference]. Anti-patterns to avoid: [list]. Learning [ID]: <summary>.",
      "qualityDimensions": ["What 'good' looks like for this task — correctness invariants, perf/efficiency requirements, idiomatic patterns vs anti-patterns. One flat list, no sub-buckets."],
      "edgeCases": [
        "Empty/null input: [expected behavior]",
        "Boundary value: [expected behavior]",
        "Invalid/malformed input: [expected behavior]"
      ],
      "failureModes": [
        {
          "cause": "What goes wrong",
          "expectedBehavior": "How system should respond"
        }
      ],
      "touchesFiles": ["path/to/file.rs"],
      "dependsOn": [],
      "modifiesBehavior": false
    },
    {
      "id": "REFACTOR-001",
      "title": "Review for refactoring opportunities",
      "taskType": "review",
      "description": "Review all implementation for DRY violations, complexity, coupling, and maintainability.\n\nDO: Check for duplicated logic, functions >30 lines, tight coupling between modules.\nDO NOT: Skip — this gate catches issues before the final review.",
      "acceptanceCriteria": [
        "No code duplication (DRY principle)",
        "Functions under 30 lines (flag complex ones)",
        "Clear separation of concerns",
        "Code follows existing project patterns",
        "Any issues found spawn REFACTOR-FIX-xxx tasks via `task-mgr add --stdin --depended-on-by REVIEW-001`"
      ],
      "priority": 98,
      "estimatedEffort": "high",
      "passes": false,
      "model": "<resolved-opus-id>",
      "timeoutSecs": 1800,
      "notes": "If issues found: `echo '{...}' | task-mgr add --stdin --depended-on-by REVIEW-001` for each (priority 50-97) — atomic DB+JSON sync, no manual JSON edit. If no issues, invoke /simplify on any ugly touchpoint, then emit `<task-status>REFACTOR-001:done</task-status>` with a one-line progress note.",
      "qualityDimensions": ["DRY across modules", "Single-responsibility functions", "Pattern consistency with existing code"],
      "touchesFiles": ["all modified files"],
      "dependsOn": ["all FEAT-xxx"]
    },
    {
      "id": "REVIEW-001",
      "title": "Code review + final verification",
      "taskType": "review",
      "description": "Review all implementation for quality, security, integration wiring, and documentation needs. RUN THE FULL QUALITY GATE (unscoped test suite).\n\nDO: Check every new function for error handling, check callers still work, verify no dead code, check if docs need updating. Run full cargo test / pytest (no scope filter).\nDO NOT: Skip the integration wiring check — unconnected code is the #1 failure mode. Do NOT scope the test suite — this is THE place the full suite runs.",
      "acceptanceCriteria": [
        "Positive: All new code reachable from production entry points (grep-verified)",
        "Positive: All errors propagated with context (map_err, not unwrap)",
        "Positive: FULL cargo test, cargo clippy -- -D warnings, cargo fmt --check all clean (no scope filter)",
        "Negative: No unwrap() in production code paths",
        "Negative: No dead code warnings for new code",
        "Documentation: Architecture docs updated if new modules/subsystems added",
        "Documentation: CLAUDE.md updated with quick-reference for new tooling/patterns",
        "Task update: Remaining tasks reviewed and updated if implementation changed APIs/assumptions",
        "Pre-existing test failures fixed (or spawned as FIX-xxx with verifyCommand if >~12 unrelated)",
        "If issues found: FIX-xxx tasks spawned via `task-mgr add --stdin --depended-on-by REVIEW-001`"
      ],
      "priority": 99,
      "estimatedEffort": "medium",
      "passes": false,
      "model": "<resolved-opus-id>",
      "timeoutSecs": 1800,
      "notes": "Spawn fixes via `echo '{...}' | task-mgr add --stdin --depended-on-by REVIEW-001` — DB + JSON synced atomically, no manual edit. If no issues: emit `<task-status>REVIEW-001:done</task-status>` with 'Clean review' note. Review remaining tasks — if implementation changed APIs, data structures, or assumptions, update task descriptions/criteria to match (via `task-mgr init --from-json ... --append --update-existing`).",
      "qualityDimensions": ["No unwrap in production", "All new code wired to production entry point", "Full suite green including pre-existing"],
      "touchesFiles": ["all modified files"],
      "dependsOn": ["all FEAT-xxx", "REFACTOR-001"]
    }
  ]
}
```

**JSON field rules:**

- `taskPrefix`: **Do NOT set.** Omit entirely. `task-mgr init` auto-generates the deterministic hash prefix from `branchName + ":" + filename` and writes it back.
- `id`: Use FEAT-xxx for implementation, REFACTOR-001 for refactor gate, REVIEW-001 for review gate, FIX-xxx for review-spawned fixes, REFACTOR-FIX-xxx for refactor-spawned fixes. Do NOT include a project prefix — the system adds one.
- `taskType`: Required on every task. Use `"implementation"`, `"review"`, `"verification"`, `"milestone"`, `"test"`, or `"analysis"`.
- `priority`: Sequential integers. REFACTOR-001 at 98, REVIEW-001 at 99.
- `passes`: Always `false` (loop marks true).
- `model`: Per Step 5 rubric — set opus on (a) FEAT/FIX with `estimatedEffort: high` OR `modifiesBehavior: true`, (b) REFACTOR-001, (c) REVIEW-001. Omit on everything else (uses PRD default sonnet).
- `timeoutSecs`: Set `1800` on REFACTOR-001 and REVIEW-001. Omit elsewhere.
- `estimatedEffort`: `low` (1 file, 1-3 criteria), `medium` (2-3 files, new function), `high` (3+ files, new module).
- `touchesFiles`: Actual file paths the agent will modify. Drives scoped per-iteration tests and synergy-based selection at runtime.
- `dependsOn`: Only hard dependencies. Don't over-constrain — let the loop pick optimal order.
- `acceptanceCriteria`: Mix positive requirements, negative requirements, test expectations, known-bad discriminators, and failure modes. Be specific enough that an agent can verify each one unambiguously.
- `description`: Include DO/DO NOT sections, edge cases, and known-bad patterns. This is the agent's primary context.
- `notes`: Implementation hints — functions to reuse (with file paths), patterns to follow, anti-patterns to avoid, relevant `Learning [ID]:` one-liners.
- `qualityDimensions`: **Flat array** of strings, NOT `{correctness, performance, style}` sub-objects. What "good" looks like for this task.
- `edgeCases`: Specific edge cases this task must handle. 1:1 mapping from known edge cases.
- `failureModes`: What goes wrong and how the system should respond.
- `modifiesBehavior`: Set `true` if the task changes return values, side effects, caching, routing, or error handling of existing functions. When true, the task description MUST document which callers are affected and how.
- **Do NOT populate** `synergyWith` / `batchWith` / `conflictsWith` — `task-mgr next` derives synergy from `touchesFiles` overlap at runtime.

### Step 9: Generate Prompt File

Create `tasks/{feature-name}-prompt.md` using the template below. Replace placeholders:

- `{{PROJECT_NAME}}` - From `Cargo.toml`, `package.json`, or directory name
- `{{FEATURE_TITLE}}` - Feature name
- `{{FEATURE_NAME}}` - Kebab-case filename (e.g., `date-context`)
- `{{PROBLEM_STATEMENT}}` - Problem description
- `{{BRANCH_NAME}}` - Branch to work on
- `{{KEY_LEARNINGS}}` - **REQUIRED for context economy**: 5-10 distilled one-liners from `task-mgr recall` (Step 2.5). Format: `- **[ID]** <one-line takeaway>`. Omit the whole section only if recall returned zero hits.
- `{{CLAUDE_MD_EXCERPTS}}` - **REQUIRED if the change touches any area documented in CLAUDE.md**: grep CLAUDE.md for the touched subsystems and paste the 3-10 relevant bullets. The loop agent never has to Read CLAUDE.md this way. Omit if greenfield.
- `{{PROHIBITED_OUTCOMES}}` - **REQUIRED, sourced from the JSON `prohibitedOutcomes[]` array**: render as a bulleted list (one `- ` line per entry). The agent doesn't Read the JSON, so these must live in the prompt.
- `{{GLOBAL_ACCEPTANCE_CRITERIA}}` - **REQUIRED, sourced from the JSON `globalAcceptanceCriteria.criteria[]`**: render as a bulleted list.
- `{{CROSS_PRD_REQUIRES}}` - **REQUIRED only when the JSON `requires[]` array is non-empty**: render each as `- **<prd>.json :: <task-id>** — <reason>`. Omit the conditional section when `requires[]` is empty.
- `{{DATA_FLOW_CONTRACTS}}` - Required if any task accesses data across module boundaries (from Step 5.5). Copy-pasteable access patterns with key types at each level.
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

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
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

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

{{GLOBAL_ACCEPTANCE_CRITERIA}}

---

{{#if CROSS_PRD_REQUIRES}}

## Cross-PRD Dependencies (check before every task)

This task list blocks on work in other PRD files. Before claiming any task, verify each entry below shows `passes: true` in its referenced PRD JSON (use `jq '.userStories[] | select(.id=="<id>") | .passes' tasks/<other-prd>.json`). If any is still `false`, output `<promise>BLOCKED</promise>` with the reason and stop.

{{CROSS_PRD_REQUIRES}}

---

{{/if}}

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Cross-PRD Requires, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/{{FEATURE_NAME}}.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` — priority auto-computed; DB + PRD JSON updated atomically                                                    |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level field that's not surfaced per-task (rare — e.g., cross-PRD `requires[]`), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/{{FEATURE_NAME}}.json
jq '.globalAcceptanceCriteria' tasks/{{FEATURE_NAME}}.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/{{FEATURE_NAME}}-prompt.md`   | This prompt file (read-only)                                               |
| `tasks/progress-$PREFIX.txt`         | Progress log — **tail** for recent context, **append** after each task     |

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
   The output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes` — everything you need. If it reports no eligible task or unmet cross-PRD `requires`, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `task-mgr next` listed a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log. Skip entirely on the first iteration.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, or the task touches a file that's likely documented there, `grep` for the relevant term and read only the surrounding lines:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```
   The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, Key Context, and the CLAUDE.md excerpts that matter here) are already embedded in **this prompt file**. Prefer it over re-reading source docs.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed. Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section or grep 2-3 existing call sites. Never guess key types from variable names.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — one rejected alternative with a one-line reason is enough. For normal tasks: pick and go.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). Fix failures before committing; never commit broken code.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:` as appropriate).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON. (Legacy `<completed>TASK-ID</completed>` still works; prefer `<task-status>`.)

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---` so the next iteration's tail works.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` already picks: eligible tasks (`passes: false`, deps complete), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority. You don't pick — you claim what it returns.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

When a task declares `modifiesBehavior: true`:

1. Read the specific callers/consumers named in the task description.
2. Decide per-caller: `OK` (proceed), `BREAKS` (split the task into per-context subtasks via `task-mgr add --stdin`, then `task-mgr skip` the original with reason "split into …"), or `NEEDS_REVIEW` (verify manually before implementing).
3. If multiple call sites need different handling (e.g., LLM-invoked vs auto-invoke), split rather than shoehorn.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **REVIEW-001** runs the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (FEAT / FIX / REFACTOR-FIX tasks)

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

**Do NOT** run the entire workspace test suite (`cargo test` with no filter, `pytest` with no path) during regular iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

These tasks run the **full, unscoped** suite on a clean checkout and must finish green:

```bash
# Rust
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test

# Python
ruff check && ruff format --check && mypy --strict && pytest
```

If ANY test fails — including pre-existing failures that predate this change — REVIEW-001 fixes them. Default: **attempt every failure**, even ones that look out-of-scope. Trunk-green is the invariant this mechanism exists to protect.

Pragmatic escape hatch: if there are **more than ~12 failures AND they're all clearly unrelated to this work** (e.g., a sibling team's integration test against a now-missing service), triage:

1. Fix everything attributable to this change's diff, inline in the REVIEW-001 commit.
2. For the remaining unrelated failures: spawn a single `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID so a human can route ownership.

Below the ~12-failure threshold, just fix them.

---

## Common Wiring Failures (REVIEW-001 reference)

New code must be reachable from production — REVIEW-001 verifies. Most common misses:

- Not registered in dispatcher/router → add to registration
- Test mocks bypass real wiring → verify production path separately
- Config field read but not passed through → wire through
- Unused-import warning on new code → call sites missing
- Wrong key type on map access (atom vs string) — struct keys ≠ JSONB keys → check Data Flow Contracts
- New CLI subcommand / DB column / JSON field defined but not threaded into the dispatcher / `TryFrom<Row>` / parse-to-task mapping

---

## Review Tasks

REFACTOR-001 and REVIEW-001 spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

### What each review looks for

| Review         | Priority | Spawns (priority)                  | Focus                                                                                                   |
| -------------- | -------- | ---------------------------------- | ------------------------------------------------------------------------------------------------------- |
| REFACTOR-001   | 98       | `REFACTOR-FIX-xxx` (50-97)         | DRY, complexity, coupling, clarity, pattern adherence                                                   |
| REVIEW-001     | 99       | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) | Language idioms, security, memory, error handling, no `unwrap()`, `qualityDimensions` met, wiring reachable, full-suite green |

Use the **rust-python-code-reviewer** / equivalent language agent when reviewing code. Document findings in the progress file. If a specific prior iteration produced something ugly and you don't want to wait for REFACTOR-001, invoke `/simplify` on that touchpoint directly — don't file a dedicated review task just for it.

### Spawning follow-up tasks

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 60,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

`--depended-on-by` wires the new task into REVIEW-001's `dependsOn` AND syncs the PRD JSON atomically — don't edit the JSON yourself. Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it **tight** — future iterations tail this; verbosity here bloats every later context.

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

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL tasks have `passes: true`
2. Verify no new tasks were created in final review
3. Verify REVIEW-001 passed with full suite green

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in the progress file
2. Create clarification task via `echo '{...}' | task-mgr add --stdin --depended-on-by <blocked-task>` (priority 0)
3. Output:

```
<promise>BLOCKED</promise>
```

---

{{#if REFERENCE_CODE}}

## Reference Code

{{REFERENCE_CODE}}

---

{{/if}}

{{#if KEY_LEARNINGS}}

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this task list. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

{{KEY_LEARNINGS}}

---

{{/if}}

{{#if CLAUDE_MD_EXCERPTS}}

## CLAUDE.md Excerpts (only what applies to this change)

These bullets were extracted from `CLAUDE.md` for the subsystems this change touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md` to pull just that block.

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

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- Work on the correct branch: **{{BRANCH_NAME}}**
````

</details>

Replace all `{{PLACEHOLDERS}}` with actual values derived from codebase exploration and the JSON you generated. After generating, grep the prompt for `{{` — zero hits means all placeholders substituted; remaining `{{X}}` indicates a missed field.

### Step 10: Validate and Report

Verify:

- [ ] `taskPrefix` is NOT set (let `task-mgr init` auto-generate it)
- [ ] All tasks have `taskType` set
- [ ] Dependencies form a valid DAG (no cycles)
- [ ] `touchesFiles` paths exist or are clearly new files
- [ ] Each task has both positive and negative acceptance criteria
- [ ] Each task with non-trivial logic has at least one known-bad discriminator
- [ ] Consumer/caller impacts noted in relevant task descriptions
- [ ] Each implementation task has `qualityDimensions` populated as a **flat array** (NOT `{correctness, performance, style}` sub-objects)
- [ ] Each task's known edge cases appear in `edgeCases` field
- [ ] Tasks with `modifiesBehavior: true` have caller impact documented in description
- [ ] REFACTOR-001 and REVIEW-001 both have `model: <opus-id>` and `timeoutSecs: 1800`
- [ ] **No task has `synergyWith` / `batchWith` / `conflictsWith` populated** (dropped — `touchesFiles` drives synergy at runtime)
- [ ] **Context-economy placeholders populated in the generated prompt** (the agent can't read the JSON, so these MUST be in the prompt):
  - [ ] `{{PROHIBITED_OUTCOMES}}` — rendered from JSON `prohibitedOutcomes[]` as a bullet list
  - [ ] `{{GLOBAL_ACCEPTANCE_CRITERIA}}` — rendered from JSON `globalAcceptanceCriteria.criteria[]` as a bullet list
  - [ ] `{{CROSS_PRD_REQUIRES}}` — rendered as bullets if JSON `requires[]` is non-empty; whole section omitted otherwise
  - [ ] `{{KEY_LEARNINGS}}` — 5-10 recalled learnings distilled into one-liners (or omitted if recall was empty)
  - [ ] `{{CLAUDE_MD_EXCERPTS}}` — only the CLAUDE.md bullets that apply to this change's touched subsystems (or omitted if greenfield)
  - [ ] `{{DATA_FLOW_CONTRACTS}}` — populated if cross-module data access exists; omitted otherwise
  - [ ] Grep the generated prompt for `{{` — zero hits confirms all placeholders substituted
- [ ] Prompt splits **scoped per-iteration** vs **full-suite at REVIEW-001** quality gates
- [ ] Documentation needs identified and included in REVIEW-001 criteria
- [ ] Task count is 2-10 (if more, suggest `/prd` + `/tasks`)

Report:

```
Created:
  - tasks/{feature}.json ({N} tasks)
  - tasks/{feature}-prompt.md

Task breakdown:
  - {X} implementation tasks
  - 1 refactoring gate (REFACTOR-001)
  - 1 review task (REVIEW-001)

To run: task-mgr loop -y tasks/{feature}.json
```

## When NOT to Use This Skill

- **Large tasks** (7+ files, architectural decisions, multiple phases): Use `/prd` → `/tasks`
- **Trivial tasks** (typo fix, one-line change): Just do it directly — no task list needed
- **Uncertain scope** (need to explore extensively before knowing what to build): Use `/prd` first to crystallize requirements
- **Behavior-modifying changes needing deep consumer analysis**: Use `/prd` + `/tasks` (which supports ANALYSIS tasks and full consumer impact tables)

## When to Use This Skill

- Single-file refactors with clear requirements
- Adding a new command/feature to an existing module
- Bug fixes that touch 2-5 files
- Implementing functionality where a reference implementation exists (e.g., Python script → Rust)
- Tasks where you already have a plan from a planning session

## Anti-Patterns to Avoid in Task Generation

| Anti-Pattern                                   | Why It's Bad                                                  | What to Do Instead                                              |
| ---------------------------------------------- | ------------------------------------------------------------- | --------------------------------------------------------------- |
| Separate TEST-INIT tasks before implementation | Wastes iterations writing tests for APIs that don't exist yet | Include tests in each FEAT task                                 |
| Multiple MILESTONE tasks                       | No-op iterations that just run cargo test                     | One REVIEW-001 at the end (IS the milestone)                    |
| Multiple REFACTOR-REVIEW tasks                 | Reviews the same code 3 times                                 | One REFACTOR-001 + one REVIEW-001                               |
| Vague acceptance criteria ("tests pass")       | Agent can't verify completion                                 | Specific: "Unit test: foo(empty) returns Err(Empty)"            |
| Over-constraining dependsOn                    | Forces sequential execution when tasks could parallelize      | Only hard dependencies                                          |
| Tasks with no negative requirements            | Agent doesn't know what to avoid                              | Every task has DO NOT section                                   |
| Missing qualityDimensions                      | Agent doesn't know what "good" means for this task            | Every FEAT task has a flat `qualityDimensions` array            |
| Missing edgeCases                              | Agent discovers edge cases in production                      | Every identified edge case has an edgeCases entry               |
| Populating `synergyWith` / `batchWith`         | Ignored — `task-mgr next` derives synergy from `touchesFiles` | Just populate `touchesFiles` accurately; drop synergyWith       |
| `qualityDimensions` as sub-objects             | Old schema; agent reads flat arrays now                       | One flat list of strings, not `{correctness, performance, style}` |
| Setting `model: opus` on FEAT-001              | "First FEAT = opus" rule was pattern-worship, now removed     | Only set opus for `high` effort OR `modifiesBehavior: true`     |
| Prompt that tells agent to Read `tasks/*.json` | Wastes context; agent can't edit JSON anyway                  | Use `task-mgr next --claim`; embed global fields in prompt      |
| Prompt without `{{PROHIBITED_OUTCOMES}}` etc.  | Agent can't see those JSON fields                             | Render all global fields as bullet lists in the prompt          |
| No data flow contracts for cross-module data   | Silent wrong-key-type bugs                                    | Trace key types, document in prompt                             |
| No documentation check in REVIEW               | Future sessions can't understand the system                   | REVIEW-001 checks if docs need updating                         |
| Running full cargo test every iteration        | Slow; defeats scoping                                         | Scoped per-iteration gate; full gate only at REVIEW-001         |
| Agent reads CLAUDE.md in full                  | CLAUDE.md is hundreds of lines                                | Embed the 3-10 relevant bullets in `{{CLAUDE_MD_EXCERPTS}}`     |
| Agent reads `tasks/long-term-learnings.md`     | Grows unboundedly                                             | Embed in `{{KEY_LEARNINGS}}`; use `task-mgr recall` for gaps    |
