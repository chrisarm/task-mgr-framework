# /prd - Product Requirements Document Generator

Generate a structured PRD from rough requirements or bug reports.

## Usage

```
/prd "feature description"
/prd                        # Interactive mode
```

## Instructions

You are a product manager helping to create a clear, actionable PRD. Follow this process:

> **CRITICAL — The 4 things that make a PRD effective:**
>
> 1. **Quality dimensions are explicit** — state what makes the solution _good_ (correctness, performance, style), not just what it does. Vague requirements produce vague code.
> 2. **Edge cases are concrete and named** — naming a specific edge case (e.g., "ß → ss") forces the implementer to handle it. Unnamed edge cases get discovered in production.
> 3. **Approaches are compared before committing** — 2-3 approaches with tradeoffs collapse multiple implement-and-rewrite cycles into one informed decision. When comparing two approaches, generally go for long-term wins over short-term gains. Excellence, speed, and thoroughness of implementation are worth taking extra time to achieve. **Phase 2 foundation principle**: if a more sophisticated solution costs ~1 day now but saves ~2+ weeks of rework post-launch, take that trade-off (1:10 ratio or better). We are pre-launch — foundations laid now compound enormously.
> 4. **Data flow contracts are verified** — for any data structure accessed across module boundaries, document the exact key type at each level with a copy-pasteable access pattern. The implementing agent cannot reliably discover correct key paths from type signatures alone; wrong-key-type bugs are silent (code compiles, tests pass with synthetic data, fails at runtime).

### Step 1: Understand the Request

If the user provided a description with the command, analyze it. Otherwise, ask:

> What feature or bug fix would you like to document?

### Step 2: Classify the Request Type

Determine if this is:

- **Feature**: New functionality being added
- **Bug Fix**: Correcting incorrect behavior
- **Enhancement**: Improving existing functionality
- **Refactor**: Restructuring without changing behavior

### Step 3: Ask Clarifying Questions

Ask 3-5 questions to fill gaps. Use lettered options (A, B, C, D) when possible:

**For Features:**

1. **Scope**: What's the minimal viable version vs full vision?

   - A) MVP only - ship fast
   - B) Full feature - take time to do it right
   - C) Phased - MVP first, then iterate

2. **Users**: Who benefits from this feature?

   - A) All users
   - B) Specific role/persona (specify)
   - C) Internal/admin only

3. **Integration**: What existing systems does this touch?

   - A) Standalone - no dependencies
   - B) Integrates with [list systems]
   - C) Replaces existing functionality

4. **Success Criteria**: How do we know it's working?
   - A) Automated tests pass
   - B) User feedback/metrics
   - C) Both

**For Bug Fixes:**

1. **Reproduction**: Can you provide steps to reproduce?
2. **Impact**: How critical is this?
   - A) Blocking - production issue
   - B) High - affects many users
   - C) Medium - workaround exists
   - D) Low - cosmetic/minor
3. **Root Cause**: Any theories on what's causing it?
4. **Expected vs Actual**: What should happen vs what does happen?
5. **Semantic Scope**: Does this fix apply uniformly, or are there different contexts?
   - A) Uniform - same fix everywhere
   - B) Context-dependent - different code paths need different handling
   - C) Unknown - need to investigate

**For All Types (Quality & Edge Cases):**

1. **Correctness constraints**: What must the implementation absolutely get right?

   - A) Data integrity — incorrect results are unacceptable
   - B) Availability — downtime/failures are unacceptable
   - C) Both
   - D) Other (specify)

2. **Performance expectations**: Are there latency, throughput, or efficiency requirements?

   - A) Best effort (no hard targets)
   - B) Must exit early / avoid unnecessary work (specify scenario)
   - C) Specific targets (specify)

3. **Style constraints**: Any coding patterns or anti-patterns to enforce?

   - A) Follow existing codebase patterns (default)
   - B) Specific requirements (e.g., "no `.unwrap()` unless provably safe")

4. **Known edge cases**: List specific inputs, scenarios, or conditions that commonly cause bugs in this area. Be concrete — naming an edge case (e.g., "Unicode chars that expand when lowercased like ß → ss") forces the implementation to handle it.

> **Why this matters**: Vague requirements produce vague code. Stating quality dimensions and edge cases explicitly in the PRD gives the implementing agent precise targets instead of hoping it discovers them independently.

### Step 3.5: Breaking Change Analysis (for behavior-modifying changes)

If this is a **Bug Fix**, **Enhancement**, or **Refactor** that modifies existing behavior:

**AUTO-INVOKE**: Run `/analyze "{function or behavior being changed}"` to perform consumer analysis.

The `/analyze` skill will:

1. Search for all code that depends on current behavior
2. Check for semantic distinctions (same code, different purposes)
3. Apply inversion thinking ("what will break?")
4. Generate a Consumer Impact Table

**After `/analyze` completes:**

- If recommendation is **PROCEED**: Continue to Step 4
- If recommendation is **SPLIT**: Document the split contexts in the PRD, each becomes a separate user story
- If recommendation is **REVIEW**: Add items to Open Questions section

**Copy the Consumer Impact Table** from `/analyze` output into the Technical Considerations section.

### Step 3.6: Design Critique — Top 3 Risks

For **all request types**, identify the top 3 risks to the design:

1. **Invert the design**: "How could this design guarantee failure?"

   - What assumptions are we making that could be wrong?
   - What external dependencies could change or fail?
   - What scale/load scenarios haven't been considered?

2. **Rank by impact × likelihood**: Distill to the **top 3 risks**, each with:

   - **Risk**: What could go wrong
   - **Impact**: What happens if it does (data loss, downtime, security breach, tech debt)
   - **Mitigation**: How to prevent or detect it early

3. **Document in PRD**: Add to the Risks & Mitigations table in Section 6.

> **If any risk is rated High Impact + High Likelihood**: Flag it as a blocker and add to Open Questions. Do not proceed to Step 4 until the user has acknowledged the risk.

### Step 4: Explore the Codebase

Use Glob and Grep to identify:

- Relevant files that will be modified
- Existing patterns to follow
- Related tests
- Configuration that may be affected

**While exploring, actively look for:**

- Edge cases implied by existing code (error handling, boundary checks, special-case branches) — add these to the Known Edge Cases table in section 2.5
- Multiple viable implementation approaches — add these to the Approaches & Tradeoffs table in section 6
- Quality constraints implied by the codebase (e.g., no `.unwrap()`, specific error types) — add these to Quality Dimensions
- **Data flow paths**: For any data structure that crosses module boundaries, trace the key type at each hop (struct field → map key → JSONB key). Note where key types change between levels — these need Data Flow Contracts (see Step 4.6).
- **Existing documentation**: Check `docs/` for architecture design docs, runbooks, and dev guides. If the feature adds new modules, changes system architecture, alters developer workflows, or modifies operational procedures, note which docs need creating or updating in the PRD's Documentation section. Architecture docs (`docs/system-design-overview.md` and similar) are especially important — they allow future Claude sessions to understand the system design without reading all the code.

Document findings for the Technical Considerations section.

### Step 4.7: Check Institutional Memory

Before generating the PRD, query the task-mgr learnings database for relevant prior experience. Run **both tag-based AND query-based recall** — they hit different indexes and return different results:

```bash
# Tag-based: exact-match on curated tags
task-mgr recall --tags "{relevant-tags}" --limit 10

# Query-based: full-text / semantic search over title + content
task-mgr recall --query "{natural-language description of the feature}" --limit 10
task-mgr recall --query "{key function names, error messages, or concepts}" --limit 10

# Combined: tag AND query for narrow, high-precision results
task-mgr recall --tags "{domain}" --query "{concept}" --limit 10
```

**Why run both:** tag searches miss learnings that weren't tagged with your exact domain term (taggers are inconsistent). Query searches catch those via content matching. Conversely, query searches miss high-signal learnings whose content phrases the topic differently. Run at least one of each before generating the PRD.

**For research/spike tasks** (library evaluations, architecture spikes, benchmarking):
- Search for learnings from past similar spikes: `task-mgr recall --tags "spike,evaluation,research" --limit 10`
- Also try query-based: `task-mgr recall --query "library evaluation benchmark spike" --limit 10`
- Past spikes have crashed when the agent tried to run Docker or heavy evaluation code. If the PRD includes evaluation work:
  - Mark the task as `taskType: "research"` with `requiresHuman: true` so the loop agent flags it for human attention
  - Recommend `model: opus` and `estimatedEffort: "high"`
  - Require a clear fallback decision: "if evaluation takes >3 days, default to X and document why"
  - Consider splitting: "define evaluation criteria" (automatable) vs "run benchmarks + write ADR" (requires human)
- Embed relevant learnings in the PRD's Technical Considerations section so the implementing agent doesn't repeat past mistakes.

### Step 4.5: Define Public Contracts

Before generating the PRD, define the public interfaces this change introduces or modifies:

1. **New interfaces**: For each new module/function/endpoint:

   - Function signature with types
   - Input validation rules
   - Return type (success and error shapes)
   - Side effects (DB writes, events emitted, external calls)

2. **Modified interfaces**: For each changed public function:

   - Current signature → proposed signature
   - Breaking changes (if any)
   - Migration path for existing callers

3. **Document in PRD**: Add to the "Public Contracts" section in Section 6 (Technical Considerations).

> **Scope**: Only document public-facing interfaces (module APIs, HTTP endpoints, GenServer calls, PubSub topics). Internal helpers are implementation details for `/tasks`.

### Step 4.6: Define Data Flow Contracts

For any data structure the implementing agent will need to access across module boundaries:

1. **Trace the actual key path** through the layers — read real code, don't guess
2. **Document key type at each level** (struct/atom, map/string, JSONB/string)
3. **Provide a copy-pasteable access pattern** showing the correct way to traverse the structure
4. **Flag type transitions** where the key type changes (e.g., atom-keyed struct wrapping a string-keyed JSONB map)
5. **Document in PRD**: Add to the "Data Flow Contracts" section in Section 6

> **Why this matters**: Data access path bugs are silent — the code compiles, tests pass (if tests use synthetic data matching the wrong format), and failures only surface at runtime. The implementing agent cannot reliably discover the correct key path by reading type signatures alone; it needs a concrete, verified example.
>
> **When to skip**: If the feature only adds new modules with no cross-module data access, this section is N/A.

### Step 5: Generate the PRD

Create a markdown file at `tasks/prd-{feature-name}.md` with this structure:

```markdown
# PRD: {Feature Title}

**Type**: Feature | Bug Fix | Enhancement | Refactor
**Priority**: P0 (Critical) | P1 (High) | P2 (Medium) | P3 (Low)
**Author**: Claude Code
**Created**: {date}
**Status**: Draft

---

## 1. Overview

### Problem Statement

{What problem are we solving? Why does it matter?}

### Background

{Context, history, related work}

---

## 2. Goals

### Primary Goals

- [ ] {Measurable goal 1}
- [ ] {Measurable goal 2}

### Success Metrics

- {Metric 1}: {target value}
- {Metric 2}: {target value}

---

## 2.5. Quality Dimensions

> State what makes a **good** solution, not just what the function does. These dimensions flow directly into task acceptance criteria and test requirements.

### Correctness Requirements

- {What must this implementation get right? Be specific about failure modes}
- {Example: "Must handle Unicode characters that expand when lowercased (e.g., ß → ss)"}

### Performance Requirements

- {Specific targets or "best effort"}
- {Example: "Exit early on first mismatch — do not process full input when answer is known"}

### Style Requirements

- {Idiomatic patterns to follow, anti-patterns to avoid}
- {Example: "No .unwrap() unless provably safe; use proper error propagation"}

### Known Edge Cases

{List specific inputs, scenarios, or conditions that the implementation MUST handle correctly. These flow directly into TEST-INIT tasks as required test cases.}

| Edge Case                 | Why It Matters                 | Expected Behavior    |
| ------------------------- | ------------------------------ | -------------------- |
| {e.g., empty input}       | {Common source of panics}      | {Return empty/error} |
| {e.g., Unicode expansion} | {ß → ss changes string length} | {Handle correctly}   |

---

## 3. User Stories

### US-001: {Story Title}

**As a** {user type}
**I want** {capability}
**So that** {benefit}

**Acceptance Criteria:**

- [ ] {Criterion 1}
- [ ] {Criterion 2}

---

## 4. Functional Requirements

### FR-001: {Requirement Title}

{Description of what the system must do}

**Details:**

- {Specific behavior 1}
- {Specific behavior 2}

**Validation:**

- {How to verify this requirement is met}

---

## 5. Non-Goals (Out of Scope)

The following are explicitly **NOT** part of this work:

- {Non-goal 1} - Reason: {why excluded}
- {Non-goal 2} - Reason: {why excluded}

---

## 6. Technical Considerations

### Affected Components

- `{file/module 1}` - {what changes}
- `{file/module 2}` - {what changes}

### Dependencies

- {External dependency 1}
- {Internal dependency 1}

### Approaches & Tradeoffs

> Identify 2-3 implementation approaches before committing to one. This collapses what would otherwise be multiple implementation-and-rewrite cycles into a single informed decision.

| Approach     | Pros        | Cons         | Recommendation                     |
| ------------ | ----------- | ------------ | ---------------------------------- |
| {Approach 1} | {Strengths} | {Weaknesses} | Preferred / Alternative / Rejected |
| {Approach 2} | {Strengths} | {Weaknesses} | Preferred / Alternative / Rejected |

**Selected Approach**: {Which approach and why. If the best elements of multiple approaches can be combined, describe the hybrid.}

**Phase 2 Foundation Check**: {Does the selected approach lay a strong foundation for post-launch evolution? If a more sophisticated approach costs ~1 day now but saves ~2+ weeks of rework later (1:10 ratio or better), prefer it. State the trade-off explicitly: "Approach X costs [effort now] but avoids [rework later]" or "N/A — no phase 2 implications."}

### Risks & Mitigations

| Risk     | Impact       | Likelihood   | Mitigation |
| -------- | ------------ | ------------ | ---------- |
| {Risk 1} | High/Med/Low | High/Med/Low | {Strategy} |

### Security Considerations

- {Security item 1}
- {Security item 2}

### Public Contracts

<!-- Define public interfaces introduced or modified by this change (from Step 4.5) -->
<!-- Only public-facing: module APIs, HTTP endpoints, GenServer calls, PubSub topics -->

#### New Interfaces

| Module/Endpoint         | Signature         | Returns (success) | Returns (error) | Side Effects                        |
| ----------------------- | ----------------- | ----------------- | --------------- | ----------------------------------- |
| {module.function/arity} | {args with types} | {success shape}   | {error shape}   | {DB writes, events, external calls} |

#### Modified Interfaces

| Module/Endpoint         | Current Signature | Proposed Signature | Breaking? | Migration  |
| ----------------------- | ----------------- | ------------------ | --------- | ---------- |
| {module.function/arity} | {current}         | {proposed}         | Yes/No    | {strategy} |

### Data Flow Contracts

<!-- For any data structure the implementing agent must access across module boundaries.
     Trace the actual key path by reading real code — don't guess from type signatures.
     Show copy-pasteable access patterns in the project's language. -->

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --------- | ----------------------- | ----------------------------- |
| {e.g., context → settings → config} | {e.g., struct (typed field) → struct (typed field) → deserialized JSON (string keys)} | {Copy-pasteable code showing correct access at each level} |

<!-- Flag type transitions where key types change between levels (e.g., typed struct field
     wrapping a deserialized JSON map with string keys) — these are the #1 source of silent
     data access bugs because both key types compile/run without errors but return nil/default -->

### Consumers of Changed Behavior

<!-- Required for Bug Fix, Enhancement, and Refactor types -->

| File:Line   | Usage                          | Impact                     | Mitigation           |
| ----------- | ------------------------------ | -------------------------- | -------------------- |
| {path:line} | {how it uses the changed code} | OK / BREAKS / NEEDS REVIEW | {strategy if breaks} |

### Semantic Distinctions

<!-- Document code paths that look similar but serve different purposes -->

| Code Path           | Context           | Current Behavior   | Required After Change |
| ------------------- | ----------------- | ------------------ | --------------------- |
| {function/location} | {when/how called} | {what it does now} | {should it change?}   |

### Inversion Checklist

<!-- Apply "what will break?" thinking -->

- [ ] All callers identified and checked?
- [ ] Routing/branching decisions that depend on output reviewed?
- [ ] Tests that validate current behavior identified?
- [ ] Different semantic contexts for same code discovered and documented?

### Documentation

<!-- Identify docs that need to be created or updated. Check docs/ for existing content.
     Architecture docs are critical — they let future Claude sessions understand the system
     without reading all the code. When a feature adds modules, changes data flow, or
     introduces new subsystems, the architecture docs MUST be updated. -->

| Doc | Action | Description |
| --- | ------ | ----------- |
| {e.g., `docs/system-design-overview.md`} | Update | {Architecture changes: new modules, data flow, subsystem interactions} |
| {e.g., `docs/feature-name.md`} | Create | {Dev guide: usage, troubleshooting, API reference} |
| {e.g., `.claude/CLAUDE.md`} | Update | {Quick-reference for Claude sessions} |

<!-- Categories to consider:
     - Architecture docs: system-design-overview.md, data flow diagrams, module relationships
     - Dev guides: how to use new tooling, debugging workflows, operational procedures
     - CLAUDE.md: concise pointers so Claude agents can find relevant docs quickly
     - Runbooks: operational procedures for new infrastructure or services
     If no documentation changes needed, state "N/A — no architectural or operational changes" -->

---

## 7. Open Questions

- [ ] {Question 1}
- [ ] {Question 2}

---

## Appendix

### Related Documents

- {Link or reference 1}

### Glossary

- **{Term}**: {Definition}
```

### Step 6: Present Summary

After creating the PRD, provide:

1. File path where PRD was saved
2. Brief summary of what was documented
3. Suggested next step: `/tasks tasks/prd-{feature-name}.md`

## Example

```
User: /prd "Add dark mode toggle to settings"

Claude: I'll help you create a PRD for dark mode. Let me ask a few questions:

1. **Scope**: What's the minimal viable version?
   A) MVP - just a toggle, system follows OS preference
   B) Full - toggle + custom colors + scheduled switching
   C) Phased - MVP first

2. **Where**: Where should the toggle appear?
   A) Settings page only
   B) Settings + quick-access in header
   C) Other (specify)

3. **Persistence**: How should the preference be stored?
   A) Local storage only
   B) Synced to user account
   C) Both with account taking priority

User: A, B, C

Claude: *explores codebase for existing theme patterns*
*generates PRD*

Created: tasks/prd-dark-mode.md

Summary: PRD for dark mode toggle with MVP scope (OS preference + manual override),
accessible from settings and header quick-toggle, synced to user account.

Next step: Run `/tasks tasks/prd-dark-mode.md` to generate the task breakdown.
```

## Notes

- Keep questions focused and numbered
- Offer sensible defaults when the user is uncertain
- Be concise - PRDs should be scannable
- Focus on WHAT, not HOW (implementation details go in tasks)
- If exploring codebase reveals complexity, adjust scope recommendations

> **Final check before saving the PRD — these sections must not be empty:**
>
> - **Section 2.5 Quality Dimensions**: Correctness, Performance, Style, and Known Edge Cases all populated
> - **Section 6 Approaches & Tradeoffs**: At least 2 approaches compared with a selected approach stated
> - **Section 6 Public Contracts**: New/modified interfaces documented
> - **Section 6 Data Flow Contracts**: If the feature accesses data across module boundaries, concrete access patterns documented with key types at each level. If not applicable, state "N/A — no cross-module data access"
> - **Known Edge Cases table**: At least 2 concrete, named edge cases (not generic placeholders)
> - **Top 3 Risks**: Identified and documented with mitigations
