# /prd - Product Requirements Document Generator

Generate a structured PRD from rough requirements or bug reports.

## Usage

```
/prd "feature description"
/prd                        # Interactive mode
```

## Instructions

You are a product manager helping to create a clear, actionable PRD. Follow this process:

> **CRITICAL — The 3 things that make a PRD effective:**
>
> 1. **Quality dimensions are explicit** — state what makes the solution _good_ (correctness, performance, style), not just what it does. Vague requirements produce vague code.
> 2. **Edge cases are concrete and named** — naming a specific edge case (e.g., "ß → ss") forces the implementer to handle it. Unnamed edge cases get discovered in production.
> 3. **Approaches are compared before committing** — 2-3 approaches with tradeoffs collapse multiple implement-and-rewrite cycles into one informed decision. When comparing two approaches, generally go for long-term wins over short-term gains. Excellence, speed, and thoroughness of implementation are worth taking extra time to achieve.

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

Document findings for the Technical Considerations section.

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

### Step 4.7: Architectural Decision Points

When 2+ viable approaches exist with **no clear winner**, classify and handle as follows:

- **High-impact** (affects PRD structure, spans multiple user stories, or is a fundamental design choice — e.g., "event-sourced vs. CRUD model", "sync vs. async API"): **STOP and ask the user inline** with lettered options before continuing.

  Example prompt format:
  > **Architectural Decision**: [brief description of the fork]
  > A) [Option A — one sentence]
  > B) [Option B — one sentence]
  > Which approach should this PRD use?

- **Lower-impact** (implementation detail that doesn't change the PRD shape — e.g., "which hashing library", "pagination strategy"): **Continue writing the PRD** and document the open question in Section 7 (Open Questions) with the `[ARCH DECISION]` tag.

  Example entry:
  > `[ARCH DECISION]` Cache invalidation strategy: Option A (TTL-based) vs Option B (event-driven). Recommend A for simplicity; revisit if throughput becomes a concern.

**Do not** invent a winner when the trade-offs are genuinely balanced — ask or document instead.

### Step 5: Generate the PRD

Create a markdown file at `.task-mgr/tasks/prd-{feature-name}.md` with this structure:

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
| Feature implemented but not wired into production call path | All unit tests pass but feature has no effect at runtime | Integration test verifies observable behavior change from production entry point |

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
3. Suggested next step: `/tasks .task-mgr/tasks/prd-{feature-name}.md`

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

Created: .task-mgr/tasks/prd-dark-mode.md

Summary: PRD for dark mode toggle with MVP scope (OS preference + manual override),
accessible from settings and header quick-toggle, synced to user account.

Next step: Run `/tasks .task-mgr/tasks/prd-dark-mode.md` to generate the task breakdown.
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
> - **Known Edge Cases table**: At least 2 concrete, named edge cases (not generic placeholders)
> - **Top 3 Risks**: Identified and documented with mitigations
